use std::io::ErrorKind;
use std::process::Command;

use anyhow::{anyhow, Context};

use super::monitor::{normalize_monitors, parse_raw_monitors, MonitorState};

pub struct HyprctlClient;

impl HyprctlClient {
    pub fn monitors_all(&self) -> anyhow::Result<Vec<MonitorState>> {
        let output = Command::new("hyprctl")
            .args(["-j", "monitors", "all"])
            .output()
            .map_err(|error| {
                if error.kind() == ErrorKind::NotFound {
                    anyhow!("hyprctl was not found in PATH; install Hyprland or run inside a Hyprland environment")
                } else {
                    anyhow!(error).context("failed to run hyprctl")
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "failed to query Hyprland monitors with `hyprctl -j monitors all`: {}",
                stderr.trim()
            ));
        }

        parse_monitors_output(&String::from_utf8_lossy(&output.stdout))
    }
}

pub fn parse_monitors_output(stdout: &str) -> anyhow::Result<Vec<MonitorState>> {
    let monitors = parse_raw_monitors(stdout).context("failed to parse hyprctl monitor JSON")?;
    Ok(normalize_monitors(monitors))
}
