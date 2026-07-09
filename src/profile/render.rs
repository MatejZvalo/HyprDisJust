use anyhow::{bail, Context};

use crate::hyprland::monitor::MonitorState;
use crate::profile::r#match::profile_monitor_match_score;
use crate::profile::store::{Profile, ProfileMonitor, ProfileOutput};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMonitorRules {
    pub mappings: Vec<RuleMapping>,
    pub rules: Vec<String>,
    pub batch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMapping {
    pub monitor_id: String,
    pub output_name: String,
    pub rule: String,
}

pub fn render_monitor_rules(
    profile: &Profile,
    current: &[MonitorState],
) -> anyhow::Result<RenderedMonitorRules> {
    if profile.outputs.is_empty() {
        bail!("profile `{}` has no saved monitor outputs", profile.name);
    }

    let mut used_current = vec![false; current.len()];
    let mut mappings = Vec::with_capacity(current.len().max(profile.outputs.len()));
    let mut rules = Vec::with_capacity(current.len().max(profile.outputs.len()));

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
        let rule = render_monitor_rule(&current_output_name, output)?;
        mappings.push(RuleMapping {
            monitor_id: output.monitor_id.clone(),
            output_name: current_output_name,
            rule: rule.clone(),
        });
        rules.push(rule);
    }

    for (current_index, current_monitor) in current.iter().enumerate() {
        if used_current[current_index] || !current_monitor.enabled {
            continue;
        }

        let rule = render_disabled_monitor_rule(&current_monitor.output_name)?;
        mappings.push(RuleMapping {
            monitor_id: current_monitor.id.clone(),
            output_name: current_monitor.output_name.clone(),
            rule: rule.clone(),
        });
        rules.push(rule);
    }

    let batch = render_hyprctl_batch(&rules)?;
    Ok(RenderedMonitorRules {
        mappings,
        rules,
        batch,
    })
}

pub fn render_hyprctl_batch(rules: &[String]) -> anyhow::Result<String> {
    if rules.is_empty() {
        bail!("cannot render an empty Hyprland monitor batch");
    }

    let mut commands = Vec::with_capacity(rules.len());
    for rule in rules {
        validate_lua_monitor_call(rule)?;
        commands.push(format!("eval {rule}"));
    }

    Ok(commands.join(" ; "))
}

pub fn render_hyprland_lua(rules: &[String]) -> anyhow::Result<String> {
    if rules.is_empty() {
        bail!("cannot render an empty Hyprland monitor Lua config");
    }

    for rule in rules {
        validate_lua_monitor_call(rule)?;
    }

    Ok(rules.join("\n"))
}

pub fn format_hyprctl_batch_command(batch: &str) -> String {
    format!("hyprctl --batch \"{}\"", batch.replace('"', "\\\""))
}

fn validate_lua_monitor_call(rule: &str) -> anyhow::Result<()> {
    validate_batch_component(rule, "monitor Lua call")?;
    if !rule.starts_with("hl.monitor({ ") || !rule.ends_with(" })") {
        bail!("monitor Lua call `{rule}` must be an hl.monitor table call");
    }

    Ok(())
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

fn render_disabled_monitor_rule(output_name: &str) -> anyhow::Result<String> {
    validate_batch_component(output_name, "output name")?;
    Ok(format!(
        "hl.monitor({{ output = \"{}\", disabled = true }})",
        escape_lua_string(output_name)
    ))
}

fn render_monitor_rule(output_name: &str, output: &ProfileOutput) -> anyhow::Result<String> {
    validate_batch_component(output_name, "output name")?;

    if !output.enabled {
        return render_disabled_monitor_rule(output_name);
    }

    validate_batch_component(&output.mode, "monitor mode")?;
    let mut fields = vec![
        format!("output = \"{}\"", escape_lua_string(output_name)),
        format!("mode = \"{}\"", escape_lua_string(&output.mode)),
        format!("position = \"{}x{}\"", output.x, output.y),
        format!("scale = {}", format_number(output.scale)),
    ];

    if output.transform != 0 {
        fields.push(format!("transform = {}", output.transform));
    }

    Ok(format!("hl.monitor({{ {} }})", fields.join(", ")))
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
