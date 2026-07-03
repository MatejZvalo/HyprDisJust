use anyhow::{bail, Context};

use crate::hyprland::monitor::MonitorState;
use crate::profile::r#match::profile_monitor_match_score;
use crate::profile::store::{Profile, ProfileMonitor, ProfileOutput};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMonitorRules {
    pub rules: Vec<String>,
    pub batch: String,
}

pub fn render_monitor_rules(
    profile: &Profile,
    current: &[MonitorState],
) -> anyhow::Result<RenderedMonitorRules> {
    if profile.outputs.is_empty() {
        bail!("profile `{}` has no saved monitor outputs", profile.name);
    }

    let mut used_current = vec![false; current.len()];
    let mut rules = Vec::with_capacity(profile.outputs.len());

    for output in &profile.outputs {
        let current_output_name =
            resolve_current_output_name(profile, output, current, &mut used_current).with_context(
                || {
                    format!(
                        "failed to map profile `{}` monitor `{}` to a current output",
                        profile.name, output.monitor_id
                    )
                },
            )?;
        rules.push(render_monitor_rule(&current_output_name, output)?);
    }

    let batch = render_hyprctl_batch(&rules)?;
    Ok(RenderedMonitorRules { rules, batch })
}

pub fn render_hyprctl_batch(rules: &[String]) -> anyhow::Result<String> {
    if rules.is_empty() {
        bail!("cannot render an empty Hyprland monitor batch");
    }

    let mut commands = Vec::with_capacity(rules.len());
    for rule in rules {
        let value = monitor_rule_value(rule)?;
        commands.push(format!("keyword monitor {value}"));
    }

    Ok(commands.join(" ; "))
}

pub fn render_hyprland_conf(rules: &[String]) -> anyhow::Result<String> {
    if rules.is_empty() {
        bail!("cannot render an empty Hyprland monitor config");
    }

    let mut lines = Vec::with_capacity(rules.len());
    for rule in rules {
        lines.push(format!("monitor = {}", monitor_rule_value(rule)?));
    }

    Ok(lines.join("\n"))
}

pub fn render_hyprland_lua(rules: &[String]) -> anyhow::Result<String> {
    if rules.is_empty() {
        bail!("cannot render an empty Hyprland monitor Lua config");
    }

    let mut lines = Vec::with_capacity(rules.len());
    for rule in rules {
        lines.push(format!(
            "hyprland.keyword(\"monitor\", \"{}\")",
            escape_lua_string(monitor_rule_value(rule)?)
        ));
    }

    Ok(lines.join("\n"))
}

pub fn format_hyprctl_batch_command(batch: &str) -> String {
    format!("hyprctl --batch \"{}\"", batch.replace('"', "\\\""))
}

fn monitor_rule_value(rule: &str) -> anyhow::Result<&str> {
    validate_batch_component(rule, "monitor rule")?;
    rule.strip_prefix("monitor ")
        .with_context(|| format!("monitor rule `{rule}` must start with `monitor `"))
}

fn resolve_current_output_name(
    profile: &Profile,
    output: &ProfileOutput,
    current: &[MonitorState],
    used_current: &mut [bool],
) -> anyhow::Result<String> {
    let profile_monitor = profile
        .monitors
        .iter()
        .find(|monitor| monitor.id == output.monitor_id);

    let mut best_score = 0;
    let mut best_indexes = Vec::new();

    for (current_index, current_monitor) in current.iter().enumerate() {
        if used_current[current_index] {
            continue;
        }

        let score = match profile_monitor {
            Some(profile_monitor) => {
                profile_monitor_match_score(profile_monitor, current_monitor).0
            }
            None if output.monitor_id == current_monitor.id => 100,
            None => 0,
        };

        if score > best_score {
            best_score = score;
            best_indexes.clear();
            best_indexes.push(current_index);
        } else if score == best_score && score > 0 {
            best_indexes.push(current_index);
        }
    }

    if best_score <= 0 {
        bail!("required monitor is not currently connected");
    }

    if best_indexes.len() > 1 {
        let label = profile_monitor
            .map(profile_monitor_label)
            .unwrap_or_else(|| output.monitor_id.clone());
        bail!("monitor `{label}` maps to multiple current outputs");
    }

    let current_index = best_indexes[0];
    used_current[current_index] = true;
    Ok(current[current_index].output_name.clone())
}

fn render_monitor_rule(output_name: &str, output: &ProfileOutput) -> anyhow::Result<String> {
    validate_batch_component(output_name, "output name")?;

    if !output.enabled {
        return Ok(format!("monitor {output_name},disable"));
    }

    validate_batch_component(&output.mode, "monitor mode")?;
    let mut rule = format!(
        "monitor {output_name},{},{}x{},{}",
        output.mode,
        output.x,
        output.y,
        format_number(output.scale)
    );

    if output.transform != 0 {
        rule.push_str(&format!(",transform,{}", output.transform));
    }

    Ok(rule)
}

fn validate_batch_component(value: &str, label: &str) -> anyhow::Result<()> {
    if value.contains(';') || value.contains('\n') || value.contains('\r') {
        bail!("{label} must not contain command separators");
    }

    Ok(())
}

fn escape_lua_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn profile_monitor_label(profile_monitor: &ProfileMonitor) -> String {
    if !profile_monitor.name_hint.trim().is_empty() {
        return profile_monitor.name_hint.clone();
    }

    profile_monitor.id.clone()
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        return format!("{value:.0}");
    }

    let formatted = format!("{value:.3}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}
