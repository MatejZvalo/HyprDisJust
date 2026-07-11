use std::collections::HashSet;

use anyhow::bail;

use crate::hyprland::hyprctl::HyprctlClient;
use crate::hyprland::monitor::MonitorState;
use crate::profile::render::{render_monitor_rules, RuleMapping};
use crate::profile::store::{Profile, ProfileOutput};

const REFRESH_TOLERANCE_HZ: f64 = 0.2;
const SCALE_TOLERANCE: f64 = 0.001;

#[derive(Debug, Clone, PartialEq)]
pub struct ApplyPlan {
    pub profile_name: String,
    pub mappings: Vec<RuleMapping>,
    pub rules: Vec<String>,
    pub batch: String,
    pub warnings: Vec<ApplyWarning>,
    pub is_noop: bool,
    expected: Vec<ExpectedMonitorState>,
    rollback_batch: String,
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
    let warnings = apply_warnings(profile);
    let expected: Vec<ExpectedMonitorState> = rendered
        .mappings
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
            }
        })
        .collect();
    let rollback_profile =
        Profile::from_monitors("rollback".to_owned(), current, String::new(), String::new());
    let rollback_batch = render_monitor_rules(&rollback_profile, current)?.batch;
    let is_noop = verify_expected_state(&expected, current).is_ok();

    Ok(ApplyPlan {
        profile_name: profile.name.clone(),
        mappings: rendered.mappings,
        rules: rendered.rules,
        batch: rendered.batch,
        warnings,
        is_noop,
        expected,
        rollback_batch,
    })
}

pub fn apply_plan(plan: &ApplyPlan) -> anyhow::Result<()> {
    ensure_plan_safe_to_apply(plan)?;
    if plan.is_noop {
        return Ok(());
    }

    let client = HyprctlClient;
    if let Err(error) = client.apply_monitor_batch(&plan.batch) {
        return Err(apply_failure_with_rollback(&client, plan, error));
    }

    // Fixture-backed runs are an explicit command-invocation test seam; they
    // cannot observe compositor mutations. Production runs always verify.
    if std::env::var_os("HYPRDISJUST_MONITORS_JSON").is_some() {
        return Ok(());
    }

    let mut last_mismatch = String::new();
    for _ in 0..3 {
        match client.monitors_all() {
            Ok(monitors) => match verify_expected_state(&plan.expected, &monitors) {
                Ok(()) => return Ok(()),
                Err(error) => last_mismatch = error.to_string(),
            },
            Err(error) => {
                last_mismatch = format!("failed to query resulting monitor state: {error:#}")
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(75));
    }

    Err(apply_failure_with_rollback(
        &client,
        plan,
        anyhow::anyhow!("monitor state did not converge: {last_mismatch}"),
    ))
}

fn apply_failure_with_rollback(
    client: &HyprctlClient,
    plan: &ApplyPlan,
    error: anyhow::Error,
) -> anyhow::Error {
    match client.apply_monitor_batch(&plan.rollback_batch) {
        Ok(()) => error.context(format!(
            "failed to apply profile `{}`; Hyprland accepted the rollback batch",
            plan.profile_name
        )),
        Err(rollback_error) => error.context(format!(
            "failed to apply profile `{}`; rollback also failed: {rollback_error:#}",
            plan.profile_name
        )),
    }
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
        if !expected.enabled {
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
        if let Some((width, height, refresh)) = parse_mode(&expected.mode) {
            if actual.width != width
                || actual.height != height
                || (actual.refresh_rate - refresh).abs() > REFRESH_TOLERANCE_HZ
            {
                bail!("output `{}` mode did not converge", expected.output_name);
            }
        }
    }
    Ok(())
}

fn parse_mode(mode: &str) -> Option<(i32, i32, f64)> {
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
                "profile `{}` output `{}` has invalid mode `{}`; expected preferred, current, highres, highrr, or WIDTHxHEIGHT@REFRESH",
                profile.name,
                output.monitor_id,
                output.mode
            );
        }

        if !output.scale.is_finite() || output.scale <= 0.0 {
            bail!(
                "profile `{}` output `{}` has invalid scale {}",
                profile.name,
                output.monitor_id,
                output.scale
            );
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
    let mode = mode.trim();
    if matches!(mode, "preferred" | "current" | "highres" | "highrr") {
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
