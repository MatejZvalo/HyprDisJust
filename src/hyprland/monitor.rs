use std::collections::{HashMap, HashSet};

use anyhow::bail;

pub const MAX_CURRENT_MONITORS: usize = 32;
pub const MAX_AVAILABLE_MODES_PER_MONITOR: usize = 256;
const MAX_MONITOR_TEXT_BYTES: usize = 4096;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RawHyprMonitor {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub make: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub serial: String,
    pub width: i32,
    pub height: i32,
    #[serde(default, rename = "physicalWidth")]
    pub physical_width: i32,
    #[serde(default, rename = "physicalHeight")]
    pub physical_height: i32,
    #[serde(default, rename = "refreshRate")]
    pub refresh_rate: f64,
    pub x: i32,
    pub y: i32,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub transform: i32,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, rename = "availableModes")]
    pub available_modes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorState {
    pub output_name: String,
    pub id: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub enabled: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub refresh_rate: f64,
    pub scale: f64,
    pub transform: i32,
    pub available_modes: Vec<String>,
    pub physical_width: i32,
    pub physical_height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityProvenance {
    PhysicalSerial,
    PhysicalNoSerial,
    Description,
    ConnectorFallback,
    ConnectorDisambiguated,
    LegacyUntrusted,
}

impl MonitorState {
    pub fn identity_provenance(&self) -> IdentityProvenance {
        infer_identity_provenance(
            &self.id,
            &self.make,
            &self.model,
            &self.serial,
            &self.description,
        )
    }
}

pub fn infer_identity_provenance(
    id: &str,
    make: &str,
    model: &str,
    serial: &str,
    description: &str,
) -> IdentityProvenance {
    let expected = stable_monitor_id(make, model, serial, description, "");
    if id == expected {
        if expected.starts_with("output:") {
            IdentityProvenance::ConnectorFallback
        } else if useful(make) && useful(model) && useful(serial) {
            IdentityProvenance::PhysicalSerial
        } else if useful(make) && useful(model) {
            IdentityProvenance::PhysicalNoSerial
        } else if useful(description) {
            IdentityProvenance::Description
        } else {
            IdentityProvenance::LegacyUntrusted
        }
    } else if expected != "output:unknown"
        && id
            .strip_prefix(&expected)
            .is_some_and(|suffix| suffix.starts_with(":output:") && suffix.len() > 8)
    {
        IdentityProvenance::ConnectorDisambiguated
    } else if id.starts_with("output:") {
        IdentityProvenance::ConnectorFallback
    } else {
        IdentityProvenance::LegacyUntrusted
    }
}

pub fn parse_raw_monitors(json: &str) -> anyhow::Result<Vec<RawHyprMonitor>> {
    let monitors: Vec<RawHyprMonitor> = serde_json::from_str(json)?;
    validate_raw_monitors(&monitors)?;
    Ok(monitors)
}

pub fn validate_raw_monitors(monitors: &[RawHyprMonitor]) -> anyhow::Result<()> {
    if monitors.len() > MAX_CURRENT_MONITORS {
        bail!("monitor topology has more than the maximum {MAX_CURRENT_MONITORS} outputs");
    }
    for monitor in monitors {
        if monitor.available_modes.len() > MAX_AVAILABLE_MODES_PER_MONITOR {
            bail!(
                "output `{}` advertises more than the maximum {MAX_AVAILABLE_MODES_PER_MONITOR} modes",
                monitor.name
            );
        }
        for (label, value) in [
            ("name", monitor.name.as_str()),
            ("description", monitor.description.as_str()),
            ("make", monitor.make.as_str()),
            ("model", monitor.model.as_str()),
            ("serial", monitor.serial.as_str()),
        ] {
            if value.len() > MAX_MONITOR_TEXT_BYTES {
                bail!("output monitor {label} is longer than {MAX_MONITOR_TEXT_BYTES} bytes");
            }
        }
        if monitor
            .available_modes
            .iter()
            .any(|mode| mode.len() > MAX_MONITOR_TEXT_BYTES)
        {
            bail!(
                "output `{}` advertises an oversized mode string",
                monitor.name
            );
        }
    }
    Ok(())
}

pub fn normalize_monitors(raw_monitors: Vec<RawHyprMonitor>) -> Vec<MonitorState> {
    let mut monitors: Vec<_> = raw_monitors.into_iter().map(MonitorState::from).collect();
    disambiguate_duplicate_monitor_ids(&mut monitors);
    monitors
}

impl From<RawHyprMonitor> for MonitorState {
    fn from(raw: RawHyprMonitor) -> Self {
        let id = stable_monitor_id(
            &raw.make,
            &raw.model,
            &raw.serial,
            &raw.description,
            &raw.name,
        );

        Self {
            output_name: raw.name,
            id,
            description: raw.description,
            make: raw.make,
            model: raw.model,
            serial: raw.serial,
            enabled: !raw.disabled,
            x: raw.x,
            y: raw.y,
            width: raw.width,
            height: raw.height,
            refresh_rate: raw.refresh_rate,
            scale: raw.scale,
            transform: raw.transform,
            available_modes: raw.available_modes,
            physical_width: raw.physical_width,
            physical_height: raw.physical_height,
        }
    }
}

pub fn stable_monitor_id(
    make: &str,
    model: &str,
    serial: &str,
    description: &str,
    output_name: &str,
) -> String {
    let make = make.trim();
    let model = model.trim();
    let serial = serial.trim();
    let description = description.trim();

    if useful(make) && useful(model) && useful(serial) {
        return format!(
            "{}:{}:{}",
            slug_component(make),
            slug_component(model),
            slug_component(serial)
        );
    }

    if useful(make) && useful(model) {
        return format!(
            "{}:{}:no-serial",
            slug_component(make),
            slug_component(model)
        );
    }

    if useful(description) {
        return format!("description:{}", slug_component(description));
    }

    format!("output:{}", slug_component(output_name))
}

pub fn slug_component(input: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_dash = false;

    for character in input.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            previous_was_dash = false;
        } else if !previous_was_dash && !slug.is_empty() {
            slug.push('-');
            previous_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "unknown".to_owned()
    } else {
        slug.to_owned()
    }
}

fn disambiguate_duplicate_monitor_ids(monitors: &mut [MonitorState]) {
    let mut counts = HashMap::new();
    for monitor in monitors.iter() {
        *counts.entry(monitor.id.clone()).or_insert(0usize) += 1;
    }

    let mut used_ids: HashSet<String> = counts
        .iter()
        .filter_map(
            |(id, count)| {
                if *count == 1 {
                    Some(id.clone())
                } else {
                    None
                }
            },
        )
        .collect();

    for monitor in monitors {
        if counts.get(&monitor.id).copied().unwrap_or_default() <= 1 {
            continue;
        }

        let base = monitor.id.clone();
        let candidate_base = format!("{base}:output:{}", slug_component(&monitor.output_name));
        let mut candidate = candidate_base.clone();
        let mut suffix = 2;
        while used_ids.contains(&candidate) {
            candidate = format!("{candidate_base}-{suffix}");
            suffix += 1;
        }

        used_ids.insert(candidate.clone());
        monitor.id = candidate;
    }
}

fn useful(value: &str) -> bool {
    !value.trim().is_empty() && slug_component(value) != "unknown"
}

fn default_scale() -> f64 {
    1.0
}
