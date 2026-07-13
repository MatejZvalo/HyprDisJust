use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};

use crate::hyprland::hyprctl::HyprctlClient;
use crate::hyprland::monitor::MonitorState;
use crate::profile::render::{render_hyprctl_batch, render_monitor_rules, RuleMapping};
use crate::profile::store::{Profile, ProfileOutput};

const REFRESH_TOLERANCE_HZ: f64 = 0.2;
const SCALE_TOLERANCE: f64 = 0.001;
const LOGICAL_SIZE_TOLERANCE: f64 = 0.01;
const MIN_SCALE: f64 = 0.1;
const MAX_SCALE: f64 = 10.0;
pub const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(15);
const VERIFY_ATTEMPTS: usize = 3;
const VERIFY_RETRY_DELAY: Duration = Duration::from_millis(75);

#[derive(Debug, Clone, PartialEq)]
pub struct ApplyPlan {
    pub profile_name: String,
    pub mappings: Vec<RuleMapping>,
    pub rules: Vec<String>,
    pub batch: String,
    pub warnings: Vec<ApplyWarning>,
    pub is_noop: bool,
    profile: Profile,
    expected: Vec<ExpectedMonitorState>,
    rollback_batch: String,
    rollback_expected: Vec<ExpectedMonitorState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationResult {
    Confirmed,
    TimedOut,
    Rejected,
    EndOfInput,
}

impl ConfirmationResult {
    pub fn rollback_reason(self) -> Option<&'static str> {
        match self {
            Self::Confirmed => None,
            Self::TimedOut => Some("confirmation timed out"),
            Self::Rejected => Some("a key other than `y` was pressed"),
            Self::EndOfInput => Some("confirmation input closed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    Noop,
    Confirmed,
    Unattended,
    RolledBack { reason: String },
}

pub trait ApplyConfirmation {
    /// Check confirmation input before the monitor layout is changed.
    fn prepare(&mut self) -> anyhow::Result<()>;

    fn confirm(
        &mut self,
        profile_name: &str,
        timeout: Duration,
    ) -> anyhow::Result<ConfirmationResult>;
}

pub trait MonitorController {
    fn apply_monitor_batch(&mut self, batch: &str) -> anyhow::Result<()>;
    fn monitors_all(&mut self) -> anyhow::Result<Vec<MonitorState>>;
}

pub trait ApplyWait {
    fn wait(&mut self, duration: Duration);
}

#[derive(Debug, Default)]
pub struct SystemApplyWait;

impl ApplyWait for SystemApplyWait {
    fn wait(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[derive(Debug, Default)]
pub struct TerminalConfirmation;

impl ApplyConfirmation for TerminalConfirmation {
    fn prepare(&mut self) -> anyhow::Result<()> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            bail!(
                "interactive confirmation requires terminal stdin and stdout; pass --unattended only when automatic acceptance is intended"
            );
        }
        Ok(())
    }

    fn confirm(
        &mut self,
        profile_name: &str,
        timeout: Duration,
    ) -> anyhow::Result<ConfirmationResult> {
        let raw_mode_was_enabled =
            is_raw_mode_enabled().context("failed to inspect terminal input mode")?;
        if !raw_mode_was_enabled {
            enable_raw_mode().context("failed to enable raw terminal input for confirmation")?;
        }

        let result = read_terminal_confirmation(profile_name, timeout);
        let restore_result = if raw_mode_was_enabled {
            Ok(())
        } else {
            disable_raw_mode().context("failed to restore terminal input mode after confirmation")
        };

        match (result, restore_result) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(restore_error)) => Err(error.context(restore_error)),
        }
    }
}

fn read_terminal_confirmation(
    profile_name: &str,
    timeout: Duration,
) -> anyhow::Result<ConfirmationResult> {
    let deadline = Instant::now() + timeout;
    let mut last_displayed = None;

    loop {
        let now = Instant::now();
        let Some(remaining) = deadline.checked_duration_since(now) else {
            finish_confirmation_line()?;
            return Ok(ConfirmationResult::TimedOut);
        };
        let seconds = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0));
        if last_displayed != Some(seconds) {
            print!(
                "\rKeep profile `{profile_name}`? Press `y` to confirm ({seconds:>2}s remaining): "
            );
            io::stdout()
                .flush()
                .context("failed to show confirmation countdown")?;
            last_displayed = Some(seconds);
        }

        let poll_for = remaining.min(Duration::from_secs(1));
        if !event::poll(poll_for).context("failed to wait for confirmation input")? {
            continue;
        }

        match event::read().context("failed to read confirmation input")? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                finish_confirmation_line()?;
                return Ok(if key.code == KeyCode::Char('y') {
                    ConfirmationResult::Confirmed
                } else {
                    ConfirmationResult::Rejected
                });
            }
            _ => {}
        }
    }
}

fn finish_confirmation_line() -> anyhow::Result<()> {
    println!();
    io::stdout()
        .flush()
        .context("failed to finish confirmation countdown")
}

impl MonitorController for HyprctlClient {
    fn apply_monitor_batch(&mut self, batch: &str) -> anyhow::Result<()> {
        HyprctlClient::apply_monitor_batch(self, batch)
    }

    fn monitors_all(&mut self) -> anyhow::Result<Vec<MonitorState>> {
        HyprctlClient::monitors_all(self)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ExpectedMonitorState {
    output_name: String,
    enabled: bool,
    mode: String,
    x: i32,
    y: i32,
    scale: f64,
    transform: i32,
    verify_details_when_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyWarning {
    OverlappingOutputs { left: String, right: String },
    DisablesAllOutputs,
}

impl ApplyWarning {
    pub fn message(&self) -> String {
        match self {
            Self::OverlappingOutputs { left, right } => {
                format!("outputs `{left}` and `{right}` overlap")
            }
            Self::DisablesAllOutputs => "profile disables every saved output".to_owned(),
        }
    }
}

pub fn plan_apply(profile: &Profile, current: &[MonitorState]) -> anyhow::Result<ApplyPlan> {
    validate_profile_outputs(profile)?;
    let rendered = render_monitor_rules(profile, current)?;
    validate_mapped_output_scales(profile, current, &rendered.mappings)?;
    let warnings = apply_warnings(profile);
    let expected = expected_monitor_states(profile, &rendered.mappings, false);
    let (rollback_batch, rollback_expected) = render_rollback_snapshot(current)?;
    let is_noop = verify_expected_state(&expected, current).is_ok();

    Ok(ApplyPlan {
        profile_name: profile.name.clone(),
        mappings: rendered.mappings,
        rules: rendered.rules,
        batch: rendered.batch,
        warnings,
        is_noop,
        profile: profile.clone(),
        expected,
        rollback_batch,
        rollback_expected,
    })
}

pub fn apply_plan_safely(
    plan: &ApplyPlan,
    confirmation: Option<&mut dyn ApplyConfirmation>,
) -> anyhow::Result<ApplyOutcome> {
    let mut client = HyprctlClient;
    let mut wait = SystemApplyWait;
    let current = client
        .monitors_all()
        .context("failed to query live monitor topology immediately before apply")?;
    let refreshed_plan = replan_apply(plan, &current)
        .context("failed to rebuild the apply plan from the immediate monitor topology")?;
    apply_plan_safely_with_controller(&refreshed_plan, &mut client, &mut wait, confirmation, false)
}

pub fn replan_apply(plan: &ApplyPlan, current: &[MonitorState]) -> anyhow::Result<ApplyPlan> {
    plan_apply(&plan.profile, current)
}

fn render_rollback_snapshot(
    current: &[MonitorState],
) -> anyhow::Result<(String, Vec<ExpectedMonitorState>)> {
    let rollback_profile =
        Profile::from_monitors("rollback".to_owned(), current, String::new(), String::new());
    let rollback_rendered = render_monitor_rules(&rollback_profile, current)
        .context("failed to capture rollback monitor rules")?;
    let rollback_expected =
        expected_monitor_states(&rollback_profile, &rollback_rendered.mappings, true);

    let disabled_monitor_ids: HashSet<_> = rollback_profile
        .outputs
        .iter()
        .filter(|output| !output.enabled)
        .map(|output| output.monitor_id.as_str())
        .collect();
    if disabled_monitor_ids.is_empty() {
        return Ok((rollback_rendered.batch, rollback_expected));
    }

    // Reapply every captured setting before restoring the disabled state. A
    // disable-only rule can otherwise retain mode/position changes made by the
    // failed or rejected profile.
    let mut configured_profile = rollback_profile.clone();
    for output in &mut configured_profile.outputs {
        output.enabled = true;
    }
    let configured = render_monitor_rules(&configured_profile, current)
        .context("failed to capture rollback settings for disabled outputs")?;
    let mut rules = configured.rules;
    rules.extend(
        rollback_rendered
            .mappings
            .iter()
            .filter(|mapping| disabled_monitor_ids.contains(mapping.monitor_id.as_str()))
            .map(|mapping| mapping.rule.clone()),
    );
    let batch = render_hyprctl_batch(&rules).context("failed to render rollback monitor batch")?;
    Ok((batch, rollback_expected))
}

pub fn apply_plan_safely_with_controller(
    plan: &ApplyPlan,
    client: &mut dyn MonitorController,
    wait: &mut dyn ApplyWait,
    mut confirmation: Option<&mut dyn ApplyConfirmation>,
    skip_verification: bool,
) -> anyhow::Result<ApplyOutcome> {
    ensure_plan_safe_to_apply(plan)?;
    if plan.is_noop {
        return Ok(ApplyOutcome::Noop);
    }

    if let Some(confirmation) = confirmation.as_deref_mut() {
        confirmation.prepare()?;
    }

    apply_plan_with_controller(plan, client, wait, skip_verification)?;

    let Some(confirmation) = confirmation else {
        return Ok(ApplyOutcome::Unattended);
    };

    match confirmation.confirm(&plan.profile_name, CONFIRMATION_TIMEOUT) {
        Ok(ConfirmationResult::Confirmed) => Ok(ApplyOutcome::Confirmed),
        Ok(result) => {
            let reason = match result {
                ConfirmationResult::TimedOut => "confirmation timed out",
                ConfirmationResult::Rejected => "a key other than `y` was pressed",
                ConfirmationResult::EndOfInput => "confirmation input closed",
                ConfirmationResult::Confirmed => return Ok(ApplyOutcome::Confirmed),
            };
            rollback_after_unconfirmed(client, wait, plan, reason, skip_verification)?;
            Ok(ApplyOutcome::RolledBack {
                reason: reason.to_owned(),
            })
        }
        Err(error) => {
            let reason = format!("confirmation could not be read: {error:#}");
            rollback_after_unconfirmed(client, wait, plan, &reason, skip_verification)?;
            Err(error.context("confirmation failed; the previous monitor layout was restored"))
        }
    }
}

pub fn apply_plan_with_controller(
    plan: &ApplyPlan,
    client: &mut dyn MonitorController,
    wait: &mut dyn ApplyWait,
    skip_verification: bool,
) -> anyhow::Result<()> {
    ensure_plan_safe_to_apply(plan)?;
    if plan.is_noop {
        return Ok(());
    }

    if let Err(error) = client.apply_monitor_batch(&plan.batch) {
        return Err(apply_failure_with_rollback(
            client,
            wait,
            plan,
            error,
            skip_verification,
        ));
    }

    if skip_verification {
        return Ok(());
    }

    if let Err(error) = verify_state_with_retries(client, wait, &plan.expected) {
        return Err(apply_failure_with_rollback(
            client,
            wait,
            plan,
            error.context("monitor state did not converge after apply"),
            false,
        ));
    }

    Ok(())
}

fn apply_failure_with_rollback(
    client: &mut dyn MonitorController,
    wait: &mut dyn ApplyWait,
    plan: &ApplyPlan,
    error: anyhow::Error,
    skip_verification: bool,
) -> anyhow::Error {
    match restore_previous_layout(client, wait, plan, skip_verification) {
        Ok(()) => error.context(format!(
            "failed to apply profile `{}`; the previous monitor layout was restored",
            plan.profile_name
        )),
        Err(rollback_error) => error.context(format!(
            "failed to apply profile `{}`; rollback also failed: {rollback_error:#}",
            plan.profile_name
        )),
    }
}

fn rollback_after_unconfirmed(
    client: &mut dyn MonitorController,
    wait: &mut dyn ApplyWait,
    plan: &ApplyPlan,
    reason: &str,
    skip_verification: bool,
) -> anyhow::Result<()> {
    restore_previous_layout(client, wait, plan, skip_verification).with_context(|| {
        format!(
            "{reason}; failed to restore the previous monitor layout for profile `{}`",
            plan.profile_name
        )
    })
}

fn restore_previous_layout(
    client: &mut dyn MonitorController,
    wait: &mut dyn ApplyWait,
    plan: &ApplyPlan,
    skip_verification: bool,
) -> anyhow::Result<()> {
    client
        .apply_monitor_batch(&plan.rollback_batch)
        .context("rollback command was rejected by Hyprland")?;
    if !skip_verification {
        verify_state_with_retries(client, wait, &plan.rollback_expected)
            .context("rollback monitor state did not converge")?;
    }
    Ok(())
}

fn verify_state_with_retries(
    client: &mut dyn MonitorController,
    wait: &mut dyn ApplyWait,
    expected: &[ExpectedMonitorState],
) -> anyhow::Result<()> {
    let mut last_mismatch = String::new();
    for attempt in 0..VERIFY_ATTEMPTS {
        match client.monitors_all() {
            Ok(monitors) => match verify_expected_state(expected, &monitors) {
                Ok(()) => return Ok(()),
                Err(error) => last_mismatch = error.to_string(),
            },
            Err(error) => {
                last_mismatch = format!("failed to query monitor state: {error:#}");
            }
        }
        if attempt + 1 < VERIFY_ATTEMPTS {
            wait.wait(VERIFY_RETRY_DELAY);
        }
    }
    bail!("{last_mismatch}")
}

fn expected_monitor_states(
    profile: &Profile,
    mappings: &[RuleMapping],
    verify_details_when_disabled: bool,
) -> Vec<ExpectedMonitorState> {
    mappings
        .iter()
        .map(|mapping| {
            let output = profile
                .outputs
                .iter()
                .find(|output| output.monitor_id == mapping.monitor_id);
            ExpectedMonitorState {
                output_name: mapping.output_name.clone(),
                enabled: output.is_some_and(|output| output.enabled),
                mode: output.map_or_else(String::new, |output| output.mode.clone()),
                x: output.map_or(0, |output| output.x),
                y: output.map_or(0, |output| output.y),
                scale: output.map_or(1.0, |output| output.scale),
                transform: output.map_or(0, |output| output.transform),
                verify_details_when_disabled,
            }
        })
        .collect()
}

fn verify_expected_state(
    expected: &[ExpectedMonitorState],
    actual: &[MonitorState],
) -> anyhow::Result<()> {
    for expected in expected {
        let Some(actual) = actual
            .iter()
            .find(|monitor| monitor.output_name == expected.output_name)
        else {
            bail!("output `{}` disappeared", expected.output_name);
        };
        if actual.enabled != expected.enabled {
            bail!(
                "output `{}` enabled state is {}, expected {}",
                expected.output_name,
                actual.enabled,
                expected.enabled
            );
        }
        if !expected.enabled && !expected.verify_details_when_disabled {
            continue;
        }
        if actual.x != expected.x || actual.y != expected.y {
            bail!(
                "output `{}` position did not converge",
                expected.output_name
            );
        }
        if (actual.scale - expected.scale).abs() > SCALE_TOLERANCE {
            bail!("output `{}` scale did not converge", expected.output_name);
        }
        if actual.transform != expected.transform {
            bail!(
                "output `{}` transform did not converge",
                expected.output_name
            );
        }
        verify_expected_mode(&expected.mode, actual)
            .with_context(|| format!("output `{}` mode did not converge", expected.output_name))?;
    }
    Ok(())
}

fn parse_mode(mode: &str) -> Option<(i32, i32, f64)> {
    let mode = mode.strip_suffix("Hz").unwrap_or(mode);
    let (dimensions, refresh) = mode.split_once('@')?;
    let (width, height) = dimensions.split_once('x')?;
    Some((
        width.parse().ok()?,
        height.parse().ok()?,
        refresh.parse().ok()?,
    ))
}

pub fn ensure_plan_safe_to_apply(plan: &ApplyPlan) -> anyhow::Result<()> {
    if plan
        .warnings
        .iter()
        .any(|warning| matches!(warning, ApplyWarning::DisablesAllOutputs))
    {
        bail!(
            "refusing to apply profile `{}` because it disables every saved output",
            plan.profile_name
        );
    }

    Ok(())
}

fn validate_profile_outputs(profile: &Profile) -> anyhow::Result<()> {
    let mut monitor_ids = HashSet::new();
    for monitor in &profile.monitors {
        if monitor.id.trim().is_empty() {
            bail!(
                "profile `{}` has monitor metadata without an id",
                profile.name
            );
        }
        if !monitor_ids.insert(monitor.id.as_str()) {
            bail!(
                "profile `{}` has duplicate monitor id `{}`",
                profile.name,
                monitor.id
            );
        }
    }

    let mut output_ids = HashSet::new();
    for output in &profile.outputs {
        if output.monitor_id.trim().is_empty() {
            bail!(
                "profile `{}` has an output without a monitor id",
                profile.name
            );
        }
        if !output_ids.insert(output.monitor_id.as_str()) {
            bail!(
                "profile `{}` has duplicate output for monitor `{}`",
                profile.name,
                output.monitor_id
            );
        }
        if !monitor_ids.contains(output.monitor_id.as_str()) {
            bail!(
                "profile `{}` output `{}` has no matching monitor identity metadata",
                profile.name,
                output.monitor_id
            );
        }

        if !valid_mode(&output.mode) {
            bail!(
                "profile `{}` output `{}` has invalid mode `{}`; expected preferred, highres, highrr, maxwidth, or WIDTHxHEIGHT@REFRESH",
                profile.name,
                output.monitor_id,
                output.mode
            );
        }

        if !output.scale.is_finite() || !(MIN_SCALE..=MAX_SCALE).contains(&output.scale) {
            bail!(
                "profile `{}` output `{}` has invalid scale {}; expected {MIN_SCALE}..={MAX_SCALE}",
                profile.name,
                output.monitor_id,
                output.scale
            );
        }

        if let Some((width, height, _)) = parse_mode(&output.mode) {
            validate_logical_output_size(profile, output, width, height)?;
        }

        if !(0..=7).contains(&output.transform) {
            bail!(
                "profile `{}` output `{}` has invalid transform {}; expected 0..=7",
                profile.name,
                output.monitor_id,
                output.transform
            );
        }
    }

    for monitor_id in &monitor_ids {
        if !output_ids.contains(monitor_id) {
            bail!(
                "profile `{}` monitor `{monitor_id}` has no output settings",
                profile.name
            );
        }
    }

    Ok(())
}

fn valid_mode(mode: &str) -> bool {
    let trimmed = mode.trim();
    if trimmed.is_empty() || mode != trimmed {
        return false;
    }
    let mode = trimmed;
    if matches!(mode, "preferred" | "highres" | "highrr" | "maxwidth") {
        return true;
    }

    let Some((dimensions, refresh)) = mode.split_once('@') else {
        return false;
    };
    let Some((width, height)) = dimensions.split_once('x') else {
        return false;
    };
    matches!(
        (
            width.parse::<u32>(),
            height.parse::<u32>(),
            refresh.parse::<f64>()
        ),
        (Ok(width), Ok(height), Ok(refresh))
            if width > 0 && height > 0 && refresh.is_finite() && refresh > 0.0
    )
}

fn validate_mapped_output_scales(
    profile: &Profile,
    current: &[MonitorState],
    mappings: &[RuleMapping],
) -> anyhow::Result<()> {
    for mapping in mappings {
        let Some(output) = profile
            .outputs
            .iter()
            .find(|output| output.monitor_id == mapping.monitor_id)
        else {
            continue;
        };
        if parse_mode(&output.mode).is_some() {
            continue;
        }
        let monitor = current
            .iter()
            .find(|monitor| monitor.output_name == mapping.output_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "profile `{}` output `{}` mapped to missing current output `{}`",
                    profile.name,
                    output.monitor_id,
                    mapping.output_name
                )
            })?;
        let (width, height) = resolved_special_mode_dimensions(&output.mode, monitor)?;
        validate_logical_output_size(profile, output, width, height)?;
    }
    Ok(())
}

fn validate_logical_output_size(
    profile: &Profile,
    output: &ProfileOutput,
    width: i32,
    height: i32,
) -> anyhow::Result<()> {
    let logical_width = f64::from(width) / output.scale;
    let logical_height = f64::from(height) / output.scale;
    if !approximately_integral(logical_width) || !approximately_integral(logical_height) {
        bail!(
            "profile `{}` output `{}` has invalid scale {} for {}x{}; logical dimensions must be whole pixels",
            profile.name,
            output.monitor_id,
            output.scale,
            width,
            height
        );
    }
    Ok(())
}

fn approximately_integral(value: f64) -> bool {
    (value - value.round()).abs() <= LOGICAL_SIZE_TOLERANCE
}

fn verify_expected_mode(mode: &str, actual: &MonitorState) -> anyhow::Result<()> {
    if let Some((width, height, refresh)) = parse_mode(mode) {
        if actual.width == width
            && actual.height == height
            && (actual.refresh_rate - refresh).abs() <= REFRESH_TOLERANCE_HZ
        {
            return Ok(());
        }
        bail!("expected {width}x{height}@{refresh}");
    }

    let modes = parsed_available_modes(actual);
    if modes.is_empty() {
        bail!("cannot verify special mode `{mode}` because no available modes were advertised");
    }
    match mode {
        "preferred" => {
            let (width, height, refresh) = modes[0];
            if actual.width == width
                && actual.height == height
                && (actual.refresh_rate - refresh).abs() <= REFRESH_TOLERANCE_HZ
            {
                Ok(())
            } else {
                bail!("expected preferred mode {width}x{height}@{refresh}")
            }
        }
        "highres" => {
            let max_area = modes
                .iter()
                .map(|(width, height, _)| i64::from(*width) * i64::from(*height))
                .max()
                .unwrap_or_default();
            if modes.iter().any(|(width, height, _)| {
                i64::from(*width) * i64::from(*height) == max_area
                    && actual.width == *width
                    && actual.height == *height
            }) {
                Ok(())
            } else {
                bail!("expected the highest-resolution mode")
            }
        }
        "highrr" => {
            let max_refresh = modes
                .iter()
                .map(|(_, _, refresh)| *refresh)
                .fold(f64::NEG_INFINITY, f64::max);
            if (actual.refresh_rate - max_refresh).abs() <= REFRESH_TOLERANCE_HZ {
                Ok(())
            } else {
                bail!("expected the highest-refresh mode")
            }
        }
        "maxwidth" => {
            let max_width = modes
                .iter()
                .map(|(width, _, _)| *width)
                .max()
                .unwrap_or_default();
            if actual.width == max_width {
                Ok(())
            } else {
                bail!("expected the widest mode")
            }
        }
        _ => bail!("unsupported special mode `{mode}`"),
    }
}

fn resolved_special_mode_dimensions(
    mode: &str,
    monitor: &MonitorState,
) -> anyhow::Result<(i32, i32)> {
    let modes = parsed_available_modes(monitor);
    let selected = match mode {
        "preferred" => modes.first().copied(),
        "highres" => modes
            .into_iter()
            .max_by_key(|(width, height, _)| i64::from(*width) * i64::from(*height)),
        "highrr" => modes.into_iter().max_by(|left, right| {
            left.2
                .partial_cmp(&right.2)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        "maxwidth" => modes.into_iter().max_by_key(|(width, _, _)| *width),
        _ => None,
    };
    selected
        .map(|(width, height, _)| (width, height))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot resolve special mode `{mode}` for output `{}` from its advertised modes",
                monitor.output_name
            )
        })
}

fn parsed_available_modes(monitor: &MonitorState) -> Vec<(i32, i32, f64)> {
    monitor
        .available_modes
        .iter()
        .filter_map(|mode| parse_mode(mode))
        .collect()
}

fn apply_warnings(profile: &Profile) -> Vec<ApplyWarning> {
    let mut warnings = Vec::new();

    let enabled_outputs: Vec<_> = profile
        .outputs
        .iter()
        .filter(|output| output.enabled)
        .collect();
    if !profile.outputs.is_empty() && enabled_outputs.is_empty() {
        warnings.push(ApplyWarning::DisablesAllOutputs);
    }

    for (left_index, left) in enabled_outputs.iter().enumerate() {
        let Some(left_rect) = output_rect(left) else {
            continue;
        };

        for right in enabled_outputs.iter().skip(left_index + 1) {
            let Some(right_rect) = output_rect(right) else {
                continue;
            };

            if left_rect.overlaps(&right_rect) {
                warnings.push(ApplyWarning::OverlappingOutputs {
                    left: left.monitor_id.clone(),
                    right: right.monitor_id.clone(),
                });
            }
        }
    }

    warnings
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Rect {
    fn overlaps(self, other: &Self) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }
}

fn output_rect(output: &ProfileOutput) -> Option<Rect> {
    let (mut width, mut height) = parse_mode_dimensions(&output.mode)?;
    if matches!(output.transform, 1 | 3 | 5 | 7) {
        std::mem::swap(&mut width, &mut height);
    }

    if width <= 0 || height <= 0 || output.scale <= 0.0 || !output.scale.is_finite() {
        return None;
    }

    Some(Rect {
        x: f64::from(output.x),
        y: f64::from(output.y),
        width: f64::from(width) / output.scale,
        height: f64::from(height) / output.scale,
    })
}

fn parse_mode_dimensions(mode: &str) -> Option<(i32, i32)> {
    let dimensions = mode
        .split_once('@')
        .map_or(mode, |(dimensions, _)| dimensions);
    let (width, height) = dimensions.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}
