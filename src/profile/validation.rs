use std::collections::HashSet;

use anyhow::bail;

use crate::profile::store::{Profile, ProfileOutput, ProfileStore};

pub const MAX_PROFILE_STORE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_PROFILES: usize = 128;
pub const MAX_MONITORS_PER_PROFILE: usize = 16;
pub const MAX_PROFILE_NAME_BYTES: usize = 128;
pub const MAX_MONITOR_ID_BYTES: usize = 512;
pub const MAX_COORDINATE: i32 = 1_000_000;
pub const MIN_SCALE: f64 = 0.1;
pub const MAX_SCALE: f64 = 10.0;
pub const LOGICAL_SIZE_TOLERANCE: f64 = 0.01;

pub fn validate_store(store: &ProfileStore) -> anyhow::Result<()> {
    if store.profiles.len() > MAX_PROFILES {
        bail!("profile store has more than the maximum {MAX_PROFILES} profiles");
    }
    let mut names = HashSet::new();
    for profile in &store.profiles {
        validate_profile_name(&profile.name)?;
        if !names.insert(profile.name.as_str()) {
            bail!("duplicate profile name `{}`", profile.name);
        }
    }
    for profile in &store.profiles {
        validate_profile(profile)?;
    }
    Ok(())
}

pub fn validate_profile(profile: &Profile) -> anyhow::Result<()> {
    validate_profile_name(&profile.name)?;
    if profile.monitors.is_empty() {
        bail!(
            "profile `{}` has no monitor identity metadata",
            profile.name
        );
    }
    if profile.outputs.is_empty() {
        bail!("profile `{}` has no output settings", profile.name);
    }
    if profile.monitors.len() > MAX_MONITORS_PER_PROFILE {
        bail!(
            "profile `{}` has more than the maximum {MAX_MONITORS_PER_PROFILE} monitors",
            profile.name
        );
    }
    if profile.outputs.len() > MAX_MONITORS_PER_PROFILE {
        bail!(
            "profile `{}` has more than the maximum {MAX_MONITORS_PER_PROFILE} outputs",
            profile.name
        );
    }
    let mut monitor_ids = HashSet::new();
    for monitor in &profile.monitors {
        validate_identifier(&monitor.id, "monitor id")?;
        if !monitor_ids.insert(monitor.id.as_str()) {
            bail!(
                "profile `{}` has duplicate monitor id `{}`",
                profile.name,
                monitor.id
            );
        }
        for (label, value) in [
            ("output name hint", monitor.name_hint.as_str()),
            ("description", monitor.description.as_str()),
            ("make", monitor.make.as_str()),
            ("model", monitor.model.as_str()),
            ("serial", monitor.serial.as_str()),
        ] {
            reject_controls(value, label)?;
        }
        if monitor.physical_width < 0 || monitor.physical_height < 0 {
            bail!(
                "profile `{}` has negative physical dimensions",
                profile.name
            );
        }
    }

    let mut output_ids = HashSet::new();
    for output in &profile.outputs {
        validate_output(profile, output, &monitor_ids)?;
        if !output_ids.insert(output.monitor_id.as_str()) {
            bail!(
                "profile `{}` has duplicate output for monitor `{}`",
                profile.name,
                output.monitor_id
            );
        }
    }
    for monitor_id in monitor_ids {
        if !output_ids.contains(monitor_id) {
            bail!(
                "profile `{}` monitor `{monitor_id}` has no output settings",
                profile.name
            );
        }
    }
    Ok(())
}

pub fn validate_profile_name(name: &str) -> anyhow::Result<&str> {
    if name.is_empty() || name.trim() != name {
        bail!("profile name must not be empty or have surrounding whitespace");
    }
    if name.len() > MAX_PROFILE_NAME_BYTES {
        bail!("profile name must be at most {MAX_PROFILE_NAME_BYTES} bytes");
    }
    reject_controls(name, "profile name")?;
    Ok(name)
}

pub fn parse_mode(mode: &str) -> Option<(i32, i32, f64)> {
    let mode = mode.strip_suffix("Hz").unwrap_or(mode);
    let (dimensions, refresh) = mode.split_once('@')?;
    let (width, height) = dimensions.split_once('x')?;
    Some((
        width.parse().ok()?,
        height.parse().ok()?,
        refresh.parse().ok()?,
    ))
}

pub fn valid_mode(mode: &str) -> bool {
    if mode.is_empty() || mode.trim() != mode || mode.chars().any(char::is_control) {
        return false;
    }
    if matches!(mode, "preferred" | "highres" | "highrr" | "maxwidth") {
        return true;
    }
    matches!(
        parse_mode(mode),
        Some((width, height, refresh))
            if width > 0 && height > 0 && refresh.is_finite() && refresh > 0.0
    )
}

fn validate_output(
    profile: &Profile,
    output: &ProfileOutput,
    monitor_ids: &HashSet<&str>,
) -> anyhow::Result<()> {
    validate_identifier(&output.monitor_id, "output monitor id")?;
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
    if !(0..=7).contains(&output.transform) {
        bail!(
            "profile `{}` output `{}` has invalid transform {}; expected 0..=7",
            profile.name,
            output.monitor_id,
            output.transform
        );
    }
    let coordinate_range = -MAX_COORDINATE..=MAX_COORDINATE;
    if !coordinate_range.contains(&output.x) || !coordinate_range.contains(&output.y) {
        bail!(
            "profile `{}` output `{}` position must be within -{MAX_COORDINATE}..={MAX_COORDINATE}",
            profile.name,
            output.monitor_id
        );
    }
    if let Some((width, height, _)) = parse_mode(&output.mode) {
        validate_logical_size(profile, output, width, height)?;
    }
    Ok(())
}

fn validate_logical_size(
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

fn validate_identifier(value: &str, label: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.trim() != value {
        bail!("{label} must not be empty or have surrounding whitespace");
    }
    if value.len() > MAX_MONITOR_ID_BYTES {
        bail!("{label} must be at most {MAX_MONITOR_ID_BYTES} bytes");
    }
    reject_controls(value, label)
}

fn reject_controls(value: &str, label: &str) -> anyhow::Result<()> {
    if value.chars().any(char::is_control) {
        bail!("{label} must not contain control characters");
    }
    Ok(())
}
