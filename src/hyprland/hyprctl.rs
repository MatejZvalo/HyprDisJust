use std::env;
use std::io::ErrorKind;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context};

use super::monitor::{
    normalize_monitors, parse_raw_monitors, validate_raw_monitors, MonitorState, RawHyprMonitor,
};
use crate::atomic::read_limited;
use crate::hyprland::ipc::hyprctl_instance_signature;
use crate::process::{output_details, run_bounded};
use crate::text::sanitize_multiline_text;

const MONITORS_JSON_ENV: &str = "HYPRDISJUST_MONITORS_JSON";
const HYPRCTL_OUTPUT_LIMIT: usize = 1024 * 1024;
const HYPRCTL_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const HYPRCTL_APPLY_TIMEOUT: Duration = Duration::from_secs(10);

pub struct HyprctlClient;

impl HyprctlClient {
    pub fn monitors_all(&self) -> anyhow::Result<Vec<MonitorState>> {
        let mut command = hyprctl_command()?;
        command.args(["-j", "monitors", "all"]);
        let output = run_bounded(
            &mut command,
            "Hyprland monitor query",
            HYPRCTL_QUERY_TIMEOUT,
            HYPRCTL_OUTPUT_LIMIT,
        )
        .map_err(|error| {
                if error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == ErrorKind::NotFound) {
                    anyhow!("hyprctl was not found in PATH; install Hyprland or run inside a Hyprland environment")
                } else {
                    error.context("failed to run hyprctl monitor query")
                }
            })?;

        if !output.status.success() {
            let details = output_details(&output);
            return Err(anyhow!(
                "failed to query Hyprland monitors with `hyprctl -j monitors all`: {}",
                if details.is_empty() {
                    format!("hyprctl exited with {}", output.status)
                } else {
                    details
                }
            ));
        }

        parse_monitors_bytes(&output.stdout)
    }

    pub fn apply_monitor_batch(&self, batch: &str) -> anyhow::Result<()> {
        self.run_monitor_batch(batch, "Hyprland monitor apply")
    }

    pub fn rollback_monitor_batch(&self, batch: &str) -> anyhow::Result<()> {
        self.run_monitor_batch(batch, "Hyprland monitor rollback")
    }

    fn run_monitor_batch(&self, batch: &str, operation: &str) -> anyhow::Result<()> {
        let mut command = hyprctl_command()?;
        command.args(["--batch", batch]);
        let output = run_bounded(
            &mut command,
            operation,
            HYPRCTL_APPLY_TIMEOUT,
            HYPRCTL_OUTPUT_LIMIT,
        )
        .map_err(|error| {
                if error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == ErrorKind::NotFound) {
                    anyhow!("hyprctl was not found in PATH; install Hyprland or run inside a Hyprland environment")
                } else {
                    error.context(format!("failed to run {operation}"))
                }
            })?;

        if !output.status.success() {
            let details = output_details(&output);
            return Err(anyhow!(
                "failed to apply Hyprland monitor rules with `hyprctl --batch`: {}",
                if details.is_empty() {
                    "hyprctl exited with a failure status"
                } else {
                    details.as_str()
                }
            ));
        }

        let stderr = sanitize_multiline_text(&String::from_utf8_lossy(&output.stderr));
        let stdout = sanitize_multiline_text(&String::from_utf8_lossy(&output.stdout));
        let unexpected_output = [stderr.as_ref(), stdout.as_ref()]
            .into_iter()
            .flat_map(str::lines)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| *line != "ok")
            .collect::<Vec<_>>();
        if !unexpected_output.is_empty() {
            return Err(anyhow!(
                "failed to apply Hyprland monitor rules with `hyprctl --batch`: {}",
                unexpected_output.join("\n")
            ));
        }

        Ok(())
    }
}

fn hyprctl_command() -> anyhow::Result<Command> {
    let mut command = Command::new("hyprctl");
    let signature = hyprctl_instance_signature()
        .context("refusing to run hyprctl without a validated Hyprland socket2 path")?;
    command.env("HYPRLAND_INSTANCE_SIGNATURE", signature);
    Ok(command)
}

pub fn live_monitors() -> anyhow::Result<Vec<MonitorState>> {
    HyprctlClient.monitors_all()
}

pub fn current_monitors() -> anyhow::Result<Vec<MonitorState>> {
    if let Some(path) = env::var_os(MONITORS_JSON_ENV) {
        let contents = read_limited(
            std::path::Path::new(&path),
            HYPRCTL_OUTPUT_LIMIT as u64,
            "monitor fixture",
        )
        .with_context(|| {
            format!(
                "failed to read monitor fixture at {}",
                path.to_string_lossy()
            )
        })?;
        return parse_monitors_bytes(&contents);
    }

    live_monitors()
}

pub fn parse_monitors_output(stdout: &str) -> anyhow::Result<Vec<MonitorState>> {
    let monitors = parse_raw_monitors(stdout).context("failed to parse hyprctl monitor JSON")?;
    Ok(normalize_monitors(monitors))
}

pub fn parse_monitors_bytes(stdout: &[u8]) -> anyhow::Result<Vec<MonitorState>> {
    let monitors: Vec<RawHyprMonitor> = serde_json::from_slice(stdout)
        .context("failed to parse hyprctl monitor JSON as valid UTF-8 JSON")?;
    validate_raw_monitors(&monitors).context("invalid Hyprland monitor topology")?;
    Ok(normalize_monitors(monitors))
}
