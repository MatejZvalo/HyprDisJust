use std::env;
use std::fs;
use std::io::ErrorKind;
use std::process::Command;

use anyhow::{anyhow, Context};

use super::monitor::{normalize_monitors, parse_raw_monitors, MonitorState};

const MONITORS_JSON_ENV: &str = "HYPRDISJUST_MONITORS_JSON";

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

    pub fn apply_monitor_batch(&self, batch: &str) -> anyhow::Result<()> {
        let output = Command::new("hyprctl")
            .args(["--batch", batch])
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
            let stdout = String::from_utf8_lossy(&output.stdout);
            let details = [stderr.trim(), stdout.trim()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(anyhow!(
                "failed to apply Hyprland monitor rules with `hyprctl --batch`: {}",
                if details.is_empty() {
                    "hyprctl exited with a failure status"
                } else {
                    details.as_str()
                }
            ));
        }

        Ok(())
    }
}

pub fn current_monitors() -> anyhow::Result<Vec<MonitorState>> {
    if let Some(path) = env::var_os(MONITORS_JSON_ENV) {
        let contents = fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read monitor fixture at {}",
                path.to_string_lossy()
            )
        })?;
        return parse_monitors_output(&contents);
    }

    let client = HyprctlClient;
    client.monitors_all()
}

pub fn parse_monitors_output(stdout: &str) -> anyhow::Result<Vec<MonitorState>> {
    let monitors = parse_raw_monitors(stdout).context("failed to parse hyprctl monitor JSON")?;
    Ok(normalize_monitors(monitors))
}
