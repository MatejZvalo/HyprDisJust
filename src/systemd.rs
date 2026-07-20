use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context};
use directories::BaseDirs;

use crate::atomic::{atomic_write, PUBLIC_FILE_MODE};
use crate::process::{output_details, run_bounded};

const SERVICE_NAME: &str = "hyprdisjust.service";
const SYSTEMD_USER_DIR_ENV: &str = "HYPRDISJUST_SYSTEMD_USER_DIR";
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(15);
const SYSTEMCTL_OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemdInstallOptions {
    pub enable: bool,
    pub start: bool,
    pub dry_run: bool,
    pub unattended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemdInstallResult {
    pub service_path: PathBuf,
    pub service_contents: String,
    pub wrote_file: bool,
    pub enabled: bool,
    pub started: bool,
}

pub fn install_user_service(
    options: &SystemdInstallOptions,
) -> anyhow::Result<SystemdInstallResult> {
    let service_dir = systemd_user_dir()?;
    let service_path = service_dir.join(SERVICE_NAME);
    let exe = env::current_exe().context("failed to determine current executable path")?;
    let service_contents = render_user_service(&exe, options.unattended)?;
    verify_service_contents(&service_contents)?;

    if !options.dry_run {
        atomic_write(&service_path, service_contents.as_bytes(), PUBLIC_FILE_MODE).with_context(
            || {
                format!(
                    "failed to install systemd service {}",
                    service_path.display()
                )
            },
        )?;
        run_systemctl(&["daemon-reload"])?;
    }

    if options.enable && !options.dry_run {
        run_systemctl(&["enable", SERVICE_NAME])?;
    }
    if options.start && !options.dry_run {
        run_systemctl(&["start", SERVICE_NAME])?;
    }

    Ok(SystemdInstallResult {
        service_path,
        service_contents,
        wrote_file: !options.dry_run,
        enabled: options.enable && !options.dry_run,
        started: options.start && !options.dry_run,
    })
}

pub fn render_user_service(exe: &Path, unattended: bool) -> anyhow::Result<String> {
    let unattended_arg = if unattended { " --unattended" } else { "" };
    Ok(format!(
        "[Unit]\nDescription=HyprDisJust monitor profile daemon\nAfter=graphical-session.target\n\n[Service]\nExecStart={} daemon{}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
        quote_systemd_arg(exe)?,
        unattended_arg
    ))
}

pub fn user_service_path() -> anyhow::Result<PathBuf> {
    Ok(systemd_user_dir()?.join(SERVICE_NAME))
}

fn systemd_user_dir() -> anyhow::Result<PathBuf> {
    if let Some(path) = env::var_os(SYSTEMD_USER_DIR_ENV) {
        if path.is_empty() {
            return Err(anyhow!("{SYSTEMD_USER_DIR_ENV} must not be empty"));
        }
        return Ok(PathBuf::from(path));
    }

    let base_dirs =
        BaseDirs::new().ok_or_else(|| anyhow!("could not determine the user config directory"))?;
    Ok(base_dirs.config_dir().join("systemd").join("user"))
}

fn run_systemctl(args: &[&str]) -> anyhow::Result<()> {
    let mut command = Command::new("systemctl");
    command.arg("--user").args(args);
    let operation = format!("systemctl --user {}", args.join(" "));
    let output = run_bounded(
        &mut command,
        &operation,
        SYSTEMCTL_TIMEOUT,
        SYSTEMCTL_OUTPUT_LIMIT,
    )?;
    if !output.status.success() {
        let details = output_details(&output);
        anyhow::bail!(
            "`{operation}` failed with status {}{}{}",
            output.status,
            if details.is_empty() { "" } else { ": " },
            details
        );
    }
    Ok(())
}

fn quote_systemd_arg(path: &Path) -> anyhow::Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow!("systemd executable path must be valid UTF-8"))?;
    if value.chars().any(char::is_control) {
        anyhow::bail!("systemd executable path must not contain control characters");
    }
    if value.contains(['%', '$', '"', '\\']) {
        anyhow::bail!(
            "systemd executable path contains unsupported `%`, `$`, quote, or backslash characters"
        );
    }
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "/._+-".contains(character))
    {
        return Ok(value.to_owned());
    }

    Ok(format!("\"{value}\""))
}

fn verify_service_contents(contents: &str) -> anyhow::Result<()> {
    if contents
        .chars()
        .any(|character| character == '\0' || character == '\r')
    {
        anyhow::bail!("generated systemd unit contains unsupported control characters");
    }
    for required in ["[Unit]", "[Service]", "ExecStart=", "[Install]"] {
        if !contents.lines().any(|line| line.starts_with(required)) {
            anyhow::bail!("generated systemd unit is missing `{required}`");
        }
    }
    Ok(())
}
