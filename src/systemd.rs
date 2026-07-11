use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context};
use directories::BaseDirs;

const SERVICE_NAME: &str = "hyprdisjust.service";
const SYSTEMD_USER_DIR_ENV: &str = "HYPRDISJUST_SYSTEMD_USER_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemdInstallOptions {
    pub enable: bool,
    pub start: bool,
    pub dry_run: bool,
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
    let service_contents = render_user_service(&exe);

    if !options.dry_run {
        fs::create_dir_all(&service_dir).with_context(|| {
            format!(
                "failed to create systemd user service directory {}",
                service_dir.display()
            )
        })?;
        fs::write(&service_path, &service_contents).with_context(|| {
            format!("failed to write systemd service {}", service_path.display())
        })?;
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

pub fn render_user_service(exe: &Path) -> String {
    format!(
        "[Unit]\nDescription=HyprDisJust monitor profile daemon\nAfter=graphical-session.target\n\n[Service]\nExecStart={} daemon\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
        quote_systemd_arg(exe)
    )
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
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .with_context(|| format!("failed to run `systemctl --user {}`", args.join(" ")))?;
    if !status.success() {
        anyhow::bail!(
            "`systemctl --user {}` failed with status {}",
            args.join(" "),
            status
        );
    }
    Ok(())
}

fn quote_systemd_arg(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "/._+-".contains(character))
    {
        return value.into_owned();
    }

    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
