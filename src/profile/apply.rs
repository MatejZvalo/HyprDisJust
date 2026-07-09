use anyhow::{bail, Context};

use crate::hyprland::hyprctl::HyprctlClient;
use crate::hyprland::monitor::MonitorState;
use crate::profile::render::{render_monitor_rules, RuleMapping};
use crate::profile::store::{Profile, ProfileOutput};

#[derive(Debug, Clone, PartialEq)]
pub struct ApplyPlan {
    pub profile_name: String,
    pub mappings: Vec<RuleMapping>,
    pub rules: Vec<String>,
    pub batch: String,
    pub warnings: Vec<ApplyWarning>,
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

    Ok(ApplyPlan {
        profile_name: profile.name.clone(),
        mappings: rendered.mappings,
        rules: rendered.rules,
        batch: rendered.batch,
        warnings,
    })
}

pub fn apply_plan(plan: &ApplyPlan) -> anyhow::Result<()> {
    ensure_plan_safe_to_apply(plan)?;

    let client = HyprctlClient;
    client
        .apply_monitor_batch(&plan.batch)
        .with_context(|| format!("failed to apply profile `{}`", plan.profile_name))
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
    for output in &profile.outputs {
        if output.monitor_id.trim().is_empty() {
            bail!(
                "profile `{}` has an output without a monitor id",
                profile.name
            );
        }

        if output.enabled {
            if output.mode.trim().is_empty() {
                bail!(
                    "profile `{}` output `{}` has an empty mode",
                    profile.name,
                    output.monitor_id
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
        }
    }

    Ok(())
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
