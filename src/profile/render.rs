use anyhow::bail;

use crate::hyprland::monitor::MonitorState;
use crate::profile::r#match::resolve_monitor_matches;
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

    let ordered_outputs: Vec<_> = profile
        .outputs
        .iter()
        .filter(|output| output.enabled)
        .chain(profile.outputs.iter().filter(|output| !output.enabled))
        .collect();
    let mapping_monitors: Vec<_> = ordered_outputs
        .iter()
        .map(|output| {
            profile
                .monitors
                .iter()
                .find(|monitor| monitor.id == output.monitor_id)
                .cloned()
                .unwrap_or_else(|| ProfileMonitor {
                    id: output.monitor_id.clone(),
                    name_hint: String::new(),
                    description: String::new(),
                    make: String::new(),
                    model: String::new(),
                    serial: String::new(),
                    physical_width: 0,
                    physical_height: 0,
                })
        })
        .collect();
    let resolved = resolve_monitor_matches(&mapping_monitors, current);
    let mut used_current = vec![false; current.len()];
    let mut mappings = Vec::with_capacity(current.len().max(profile.outputs.len()));
    let mut rules = Vec::with_capacity(current.len().max(profile.outputs.len()));

    // Hyprland applies a batch sequentially. Configure every desired active
    // output before disabling anything so a profile transition cannot create
    // a transient zero-output layout.
    for (output, resolved) in ordered_outputs.into_iter().zip(resolved) {
        let Some(resolved) = resolved else {
            // A desired-disabled monitor that is physically absent already has
            // the requested outcome. Do not make it a required dependency.
            if !output.enabled {
                continue;
            }
            bail!(
                "failed to map profile `{}` monitor `{}` to a current output: required monitor is not currently connected",
                profile.name,
                output.monitor_id
            );
        };
        if resolved.ambiguous {
            bail!(
                "failed to map profile `{}` monitor `{}` to a current output: monitor maps to multiple current outputs",
                profile.name,
                output.monitor_id
            );
        }
        used_current[resolved.current_index] = true;
        let current_output_name = current[resolved.current_index].output_name.clone();
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
    let fields = [
        format!("output = \"{}\"", escape_lua_string(output_name)),
        "disabled = false".to_owned(),
        format!("mode = \"{}\"", escape_lua_string(&output.mode)),
        format!("position = \"{}x{}\"", output.x, output.y),
        format!("scale = {}", format_number(output.scale)),
        format!("transform = {}", output.transform),
    ];

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

fn format_number(value: f64) -> String {
    value.to_string()
}
