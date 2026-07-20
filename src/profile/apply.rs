use std::collections::HashSet;
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};

use crate::atomic::{ensure_private_directory, open_private_lock};
use crate::config::ConfigPaths;
use crate::hyprland::hyprctl::HyprctlClient;
use crate::hyprland::monitor::MonitorState;
use crate::profile::r#match::{
    best_profile_match, decide_auto_apply, AutoApplyDecision, MonitorMatchMode,
};
use crate::profile::render::{
    render_hyprctl_batch, render_monitor_rules, render_monitor_rules_automatic,
    render_monitor_rules_with_mode, RuleMapping,
};
use crate::profile::store::{Profile, ProfileOutput, ProfileStore};
use crate::profile::validation::{parse_mode, validate_profile};
use crate::text::{sanitize_terminal_text, write_stdout, write_stdout_line};

const REFRESH_TOLERANCE_HZ: f64 = 0.2;
const SCALE_TOLERANCE: f64 = 0.001;
const LOGICAL_SIZE_TOLERANCE: f64 = 0.01;
pub const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(15);
const VERIFY_ATTEMPTS: usize = 9;
const VERIFY_INITIAL_DELAY: Duration = Duration::from_millis(50);
const VERIFY_MAX_DELAY: Duration = Duration::from_millis(500);

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
    match_mode: MonitorMatchMode,
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

#[derive(Debug, Clone, PartialEq)]
pub enum ApplyTransactionRequest {
    Named(String),
    Automatic { fallback_profile: Option<String> },
    Draft(Profile),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApplyTransactionResult {
    pub outcome: ApplyOutcome,
    pub execution: ApplyExecution,
    pub plan: ApplyPlan,
    pub snapshot: Vec<MonitorState>,
    pub final_state: Vec<MonitorState>,
    pub automatic_decision: Option<AutoApplyDecision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyExecution {
    Suppressed,
    Noop,
    Applied,
    RolledBack,
}

pub(crate) enum ApplyTransactionState {
    Completed(Box<ApplyTransactionResult>),
    NoAutomaticMatch {
        decision: AutoApplyDecision,
        snapshot: Vec<MonitorState>,
    },
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
    fn rollback_monitor_batch(&mut self, batch: &str) -> anyhow::Result<()> {
        self.apply_monitor_batch(batch)
    }
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
            write_stdout(&format!(
                "\rKeep profile `{profile_name}`? Press `y` to confirm ({seconds:>2}s remaining): "
            ))?;
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
    write_stdout_line("").context("failed to finish confirmation countdown")
}

impl MonitorController for HyprctlClient {
    fn apply_monitor_batch(&mut self, batch: &str) -> anyhow::Result<()> {
        HyprctlClient::apply_monitor_batch(self, batch)
    }

    fn rollback_monitor_batch(&mut self, batch: &str) -> anyhow::Result<()> {
        HyprctlClient::rollback_monitor_batch(self, batch)
    }

    fn monitors_all(&mut self) -> anyhow::Result<Vec<MonitorState>> {
        HyprctlClient::monitors_all(self)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ExpectedMonitorState {
    output_name: String,
    monitor_id: String,
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
    plan_apply_with_mode(profile, current, MonitorMatchMode::Explicit)
}

pub fn plan_apply_automatic(
    profile: &Profile,
    current: &[MonitorState],
) -> anyhow::Result<ApplyPlan> {
    plan_apply_with_mode(profile, current, MonitorMatchMode::Automatic)
}

fn plan_apply_with_mode(
    profile: &Profile,
    current: &[MonitorState],
    match_mode: MonitorMatchMode,
) -> anyhow::Result<ApplyPlan> {
    validate_profile(profile)?;
    let rendered = match match_mode {
        MonitorMatchMode::Automatic => render_monitor_rules_automatic(profile, current)?,
        MonitorMatchMode::Explicit => render_monitor_rules(profile, current)?,
    };
    validate_mapped_output_scales(profile, current, &rendered.mappings)?;
    let warnings = apply_warnings(profile, current, &rendered.mappings);
    let expected = expected_monitor_states(profile, current, &rendered.mappings, false);
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
        match_mode,
    })
}

pub fn apply_plan_safely(
    plan: &ApplyPlan,
    confirmation: Option<&mut dyn ApplyConfirmation>,
) -> anyhow::Result<ApplyOutcome> {
    let paths = ConfigPaths::resolve()?;
    execute_apply_transaction(
        paths.profile_store_path(),
        ApplyTransactionRequest::Draft(plan.profile.clone()),
        confirmation,
    )
    .map(|result| result.outcome)
}

pub fn execute_apply_transaction(
    profile_store_path: &Path,
    request: ApplyTransactionRequest,
    confirmation: Option<&mut dyn ApplyConfirmation>,
) -> anyhow::Result<ApplyTransactionResult> {
    match execute_apply_transaction_if(profile_store_path, request, confirmation, |_, _, _| {
        Ok(true)
    })? {
        ApplyTransactionState::Completed(result) => Ok(*result),
        ApplyTransactionState::NoAutomaticMatch { decision, .. } => {
            Err(auto_decision_error(&decision))
        }
    }
}

pub(crate) fn execute_apply_transaction_if(
    profile_store_path: &Path,
    request: ApplyTransactionRequest,
    confirmation: Option<&mut dyn ApplyConfirmation>,
    before_apply: impl FnOnce(
        &ApplyPlan,
        &[MonitorState],
        Option<&AutoApplyDecision>,
    ) -> anyhow::Result<bool>,
) -> anyhow::Result<ApplyTransactionState> {
    let _lock = acquire_apply_lock(profile_store_path)?;
    let mut client = HyprctlClient;
    let mut wait = SystemApplyWait;
    let snapshot = client
        .monitors_all()
        .context("failed to query authoritative monitor topology inside apply transaction")?;
    let (profile, automatic_decision) = match request {
        ApplyTransactionRequest::Draft(profile) => (profile, None),
        ApplyTransactionRequest::Named(name) => {
            let store = ProfileStore::load(profile_store_path)?;
            let profile = store
                .profiles
                .iter()
                .find(|profile| profile.name == name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("profile `{name}` does not exist"))?;
            (profile, None)
        }
        ApplyTransactionRequest::Automatic { fallback_profile } => {
            let store = ProfileStore::load(profile_store_path)?;
            let best = best_profile_match(&store, &snapshot);
            let decision = decide_auto_apply(&store, &best, fallback_profile.as_deref());
            let profile_name = match &decision {
                AutoApplyDecision::Apply { profile_name, .. } => profile_name,
                _ => {
                    return Ok(ApplyTransactionState::NoAutomaticMatch { decision, snapshot });
                }
            };
            let profile = store
                .profiles
                .iter()
                .find(|profile| profile.name == *profile_name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("selected profile `{profile_name}` disappeared"))?;
            (profile, Some(decision))
        }
    };
    let plan = match automatic_decision.is_some() {
        true => plan_apply_automatic(&profile, &snapshot),
        false => plan_apply(&profile, &snapshot),
    }
    .context("failed to build apply plan from authoritative monitor topology")?;
    if !before_apply(&plan, &snapshot, automatic_decision.as_ref())? {
        return Ok(ApplyTransactionState::Completed(Box::new(
            ApplyTransactionResult {
                outcome: ApplyOutcome::Noop,
                execution: ApplyExecution::Suppressed,
                plan,
                final_state: snapshot.clone(),
                snapshot,
                automatic_decision,
            },
        )));
    }
    let mut outcome =
        apply_plan_safely_with_controller(&plan, &mut client, &mut wait, confirmation).map_err(
            |error| {
                anyhow::anyhow!(
                    "{error:#}\nPrevious layout:\n(authoritative transaction snapshot)\n{}",
                    format_snapshot(&snapshot)
                )
            },
        )?;
    let mut final_state = if outcome == ApplyOutcome::Noop {
        snapshot.clone()
    } else {
        client
            .monitors_all()
            .context("failed to query final monitor state after apply transaction")?
    };
    let expected_final = match &outcome {
        ApplyOutcome::Confirmed | ApplyOutcome::Unattended => Some(&plan.expected),
        ApplyOutcome::RolledBack { .. } => Some(&plan.rollback_expected),
        ApplyOutcome::Noop => None,
    };
    if let Some(expected) = expected_final {
        if let Err(error) = verify_expected_state(expected, &final_state) {
            let reason = format!("monitor state drifted before transaction completion: {error:#}");
            if matches!(outcome, ApplyOutcome::Confirmed | ApplyOutcome::Unattended) {
                rollback_after_unconfirmed(&mut client, &mut wait, &plan, &reason)?;
                final_state = client
                    .monitors_all()
                    .context("failed to query monitor state after final-drift rollback")?;
                verify_expected_state(&plan.rollback_expected, &final_state)
                    .context("final-drift rollback state was not preserved")?;
                outcome = ApplyOutcome::RolledBack { reason };
            } else {
                bail!("{reason}");
            }
        }
    }
    let execution = match &outcome {
        ApplyOutcome::Noop => ApplyExecution::Noop,
        ApplyOutcome::Confirmed | ApplyOutcome::Unattended => ApplyExecution::Applied,
        ApplyOutcome::RolledBack { .. } => ApplyExecution::RolledBack,
    };
    Ok(ApplyTransactionState::Completed(Box::new(
        ApplyTransactionResult {
            outcome,
            execution,
            plan,
            snapshot,
            final_state,
            automatic_decision,
        },
    )))
}

fn auto_decision_error(decision: &AutoApplyDecision) -> anyhow::Error {
    match decision {
        AutoApplyDecision::Apply { profile_name, .. } => {
            anyhow::anyhow!("selected profile `{profile_name}` disappeared")
        }
        AutoApplyDecision::NoProfiles => anyhow::anyhow!("no profiles saved"),
        AutoApplyDecision::Ambiguous { reason } => {
            anyhow::anyhow!("automatic apply is ambiguous: {reason}")
        }
        AutoApplyDecision::MissingFallback { profile_name } => {
            anyhow::anyhow!("fallback_profile `{profile_name}` does not exist")
        }
        AutoApplyDecision::NotEligible { reason } => {
            anyhow::anyhow!("no exact or high-confidence profile match: {reason}")
        }
        AutoApplyDecision::NoMatch => anyhow::anyhow!("no useful profile match"),
    }
}

fn acquire_apply_lock(profile_store_path: &Path) -> anyhow::Result<std::fs::File> {
    let _ = profile_store_path;
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is not set; cannot create apply lock"))?;
    if runtime_dir.is_empty() {
        bail!("XDG_RUNTIME_DIR is empty; cannot create apply lock");
    }
    let lock_path = apply_lock_path_for_runtime_dir(profile_store_path, Path::new(&runtime_dir))?;
    let file = open_private_lock(&lock_path, "apply lock")?;
    file.lock()
        .with_context(|| format!("failed to lock apply transaction {}", lock_path.display()))?;
    Ok(file)
}

fn apply_lock_path_for_runtime_dir(
    _profile_store_path: &Path,
    runtime_dir: &Path,
) -> anyhow::Result<PathBuf> {
    ensure_private_directory(runtime_dir)?;
    let lock_dir = runtime_dir.join("hyprdisjust");
    match ensure_private_directory(&lock_dir) {
        Ok(()) => Ok(lock_dir.join("apply.lock")),
        Err(error) if runtime_path_is_unavailable(&error) => {
            let fallback_dir = env::temp_dir().join(format!("hyprdisjust-{}", current_user_tag()));
            ensure_private_directory(&fallback_dir)?;
            Ok(fallback_dir.join("apply.lock"))
        }
        Err(error) => Err(error),
    }
}

fn runtime_path_is_unavailable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            )
        })
    })
}

fn current_user_tag() -> String {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and does not dereference pointers.
        unsafe { libc::geteuid() }.to_string()
    }
    #[cfg(not(unix))]
    {
        "current-user".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_lock_path_is_independent_of_profile_store_root() {
        let runtime = tempfile::tempdir().unwrap();
        let first =
            apply_lock_path_for_runtime_dir(Path::new("/one/config/profiles.toml"), runtime.path())
                .unwrap();
        let second =
            apply_lock_path_for_runtime_dir(Path::new("/two/config/profiles.toml"), runtime.path())
                .unwrap();

        assert_eq!(first, second);
    }
}

fn format_snapshot(monitors: &[MonitorState]) -> String {
    monitors
        .iter()
        .map(|monitor| {
            format!(
                "{} id={} enabled={} mode={}x{}@{} position={}x{} scale={} transform={}",
                safe_display(&monitor.output_name),
                safe_display(&monitor.id),
                monitor.enabled,
                monitor.width,
                monitor.height,
                monitor.refresh_rate,
                monitor.x,
                monitor.y,
                monitor.scale,
                monitor.transform
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn safe_display(value: &str) -> String {
    sanitize_terminal_text(value)
}

pub fn replan_apply(plan: &ApplyPlan, current: &[MonitorState]) -> anyhow::Result<ApplyPlan> {
    plan_apply_with_mode(&plan.profile, current, plan.match_mode)
}

fn render_rollback_snapshot(
    current: &[MonitorState],
) -> anyhow::Result<(String, Vec<ExpectedMonitorState>)> {
    let rollback_profile =
        Profile::from_monitors("rollback".to_owned(), current, String::new(), String::new());
    let rollback_rendered =
        render_monitor_rules_with_mode(&rollback_profile, current, MonitorMatchMode::Explicit)
            .context("failed to capture rollback monitor rules")?;
    let rollback_expected = expected_monitor_states(
        &rollback_profile,
        current,
        &rollback_rendered.mappings,
        true,
    );

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
    let configured =
        render_monitor_rules_with_mode(&configured_profile, current, MonitorMatchMode::Explicit)
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
) -> anyhow::Result<ApplyOutcome> {
    ensure_plan_safe_to_apply(plan)?;
    if plan.is_noop {
        return Ok(ApplyOutcome::Noop);
    }

    if let Some(confirmation) = confirmation.as_deref_mut() {
        confirmation.prepare()?;
    }

    apply_plan_with_controller(plan, client, wait)?;

    let Some(confirmation) = confirmation else {
        return Ok(ApplyOutcome::Unattended);
    };

    match confirmation.confirm(&plan.profile_name, CONFIRMATION_TIMEOUT) {
        Ok(ConfirmationResult::Confirmed) => {
            if let Err(error) = verify_state_with_retries(client, wait, &plan.expected) {
                let reason = format!("monitor state drifted during confirmation: {error:#}");
                rollback_after_unconfirmed(client, wait, plan, &reason)?;
                return Ok(ApplyOutcome::RolledBack { reason });
            }
            Ok(ApplyOutcome::Confirmed)
        }
        Ok(result) => {
            let reason = match result {
                ConfirmationResult::TimedOut => "confirmation timed out",
                ConfirmationResult::Rejected => "a key other than `y` was pressed",
                ConfirmationResult::EndOfInput => "confirmation input closed",
                ConfirmationResult::Confirmed => return Ok(ApplyOutcome::Confirmed),
            };
            rollback_after_unconfirmed(client, wait, plan, reason)?;
            Ok(ApplyOutcome::RolledBack {
                reason: reason.to_owned(),
            })
        }
        Err(error) => {
            let reason = format!("confirmation could not be read: {error:#}");
            rollback_after_unconfirmed(client, wait, plan, &reason)?;
            Err(error.context("confirmation failed; the previous monitor layout was restored"))
        }
    }
}

fn apply_plan_with_controller(
    plan: &ApplyPlan,
    client: &mut dyn MonitorController,
    wait: &mut dyn ApplyWait,
) -> anyhow::Result<()> {
    ensure_plan_safe_to_apply(plan)?;
    if plan.is_noop {
        return Ok(());
    }

    if let Err(error) = client.apply_monitor_batch(&plan.batch) {
        return Err(apply_failure_with_rollback(client, wait, plan, error));
    }

    if let Err(error) = verify_state_with_retries(client, wait, &plan.expected) {
        return Err(apply_failure_with_rollback(
            client,
            wait,
            plan,
            error.context("monitor state did not converge after apply"),
        ));
    }

    Ok(())
}

fn apply_failure_with_rollback(
    client: &mut dyn MonitorController,
    wait: &mut dyn ApplyWait,
    plan: &ApplyPlan,
    error: anyhow::Error,
) -> anyhow::Error {
    match restore_previous_layout(client, wait, plan) {
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
) -> anyhow::Result<()> {
    restore_previous_layout(client, wait, plan).with_context(|| {
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
) -> anyhow::Result<()> {
    client
        .rollback_monitor_batch(&plan.rollback_batch)
        .context("rollback command was rejected by Hyprland")?;
    verify_state_with_retries(client, wait, &plan.rollback_expected)
        .context("rollback monitor state did not converge")?;
    Ok(())
}

fn verify_state_with_retries(
    client: &mut dyn MonitorController,
    wait: &mut dyn ApplyWait,
    expected: &[ExpectedMonitorState],
) -> anyhow::Result<()> {
    let mut last_mismatch = String::new();
    let mut saw_state_mismatch = false;
    let mut delay = VERIFY_INITIAL_DELAY;
    for attempt in 0..VERIFY_ATTEMPTS {
        match client.monitors_all() {
            Ok(monitors) => match verify_expected_state(expected, &monitors) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    last_mismatch = error.to_string();
                    saw_state_mismatch = true;
                }
            },
            Err(error) => {
                if !saw_state_mismatch {
                    last_mismatch = format!("failed to query monitor state: {error:#}");
                }
            }
        }
        if attempt + 1 < VERIFY_ATTEMPTS {
            wait.wait(delay);
            delay = delay.saturating_mul(2).min(VERIFY_MAX_DELAY);
        }
    }
    bail!("{last_mismatch}")
}

fn expected_monitor_states(
    profile: &Profile,
    current: &[MonitorState],
    mappings: &[RuleMapping],
    verify_details_when_disabled: bool,
) -> Vec<ExpectedMonitorState> {
    let mut expected: Vec<_> = mappings
        .iter()
        .map(|mapping| {
            let output = profile
                .outputs
                .iter()
                .find(|output| output.monitor_id == mapping.monitor_id);
            ExpectedMonitorState {
                output_name: mapping.output_name.clone(),
                monitor_id: current
                    .iter()
                    .find(|monitor| monitor.output_name == mapping.output_name)
                    .map_or_else(|| mapping.monitor_id.clone(), |monitor| monitor.id.clone()),
                enabled: output.is_some_and(|output| output.enabled),
                mode: output.map_or_else(String::new, |output| output.mode.clone()),
                x: output.map_or(0, |output| output.x),
                y: output.map_or(0, |output| output.y),
                scale: output.map_or(1.0, |output| output.scale),
                transform: output.map_or(0, |output| output.transform),
                verify_details_when_disabled,
            }
        })
        .collect();
    let mapped_names: HashSet<_> = mappings
        .iter()
        .map(|mapping| mapping.output_name.as_str())
        .collect();
    expected.extend(
        current
            .iter()
            .filter(|monitor| !mapped_names.contains(monitor.output_name.as_str()))
            .map(|monitor| ExpectedMonitorState {
                output_name: monitor.output_name.clone(),
                monitor_id: monitor.id.clone(),
                enabled: monitor.enabled,
                mode: format!(
                    "{}x{}@{}",
                    monitor.width, monitor.height, monitor.refresh_rate
                ),
                x: monitor.x,
                y: monitor.y,
                scale: monitor.scale,
                transform: monitor.transform,
                verify_details_when_disabled,
            }),
    );
    expected
}

fn verify_expected_state(
    expected: &[ExpectedMonitorState],
    actual: &[MonitorState],
) -> anyhow::Result<()> {
    if actual.len() != expected.len() {
        let expected_names: HashSet<_> = expected
            .iter()
            .map(|monitor| monitor.output_name.as_str())
            .collect();
        let unexpected = actual
            .iter()
            .filter(|monitor| !expected_names.contains(monitor.output_name.as_str()))
            .map(|monitor| monitor.output_name.as_str())
            .collect::<Vec<_>>();
        bail!(
            "monitor topology drifted: expected {} outputs, found {}; unexpected outputs: {}",
            expected.len(),
            actual.len(),
            if unexpected.is_empty() {
                "none".to_owned()
            } else {
                unexpected.join(", ")
            }
        );
    }
    for expected in expected {
        let Some(actual) = actual
            .iter()
            .find(|monitor| monitor.output_name == expected.output_name)
        else {
            bail!(
                "monitor topology drifted: output `{}` disappeared or was renamed",
                expected.output_name
            );
        };
        if actual.id != expected.monitor_id {
            bail!(
                "monitor topology drifted: output `{}` identity changed from `{}` to `{}`",
                expected.output_name,
                expected.monitor_id,
                actual.id
            );
        }
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
        let (width, height) = resolved_mode_dimensions(&output.mode, monitor)?;
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

pub(crate) fn resolved_mode_dimensions(
    mode: &str,
    monitor: &MonitorState,
) -> anyhow::Result<(i32, i32)> {
    if let Some((width, height, _)) = parse_mode(mode) {
        return Ok((width, height));
    }
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

fn apply_warnings(
    profile: &Profile,
    current: &[MonitorState],
    mappings: &[RuleMapping],
) -> Vec<ApplyWarning> {
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
        let Some(left_rect) = output_rect(left, current, mappings) else {
            continue;
        };

        for right in enabled_outputs.iter().skip(left_index + 1) {
            let Some(right_rect) = output_rect(right, current, mappings) else {
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

fn output_rect(
    output: &ProfileOutput,
    current: &[MonitorState],
    mappings: &[RuleMapping],
) -> Option<Rect> {
    let (mut width, mut height) = parse_mode_dimensions(&output.mode).or_else(|| {
        let mapping = mappings
            .iter()
            .find(|mapping| mapping.monitor_id == output.monitor_id)?;
        let monitor = current
            .iter()
            .find(|monitor| monitor.output_name == mapping.output_name)?;
        resolved_mode_dimensions(&output.mode, monitor).ok()
    })?;
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
