use std::collections::BTreeSet;
use std::env;
use std::path::Path;

use crate::config::{AppConfig, ConfigPaths};
use crate::hyprland::hyprctl::current_monitors;
use crate::hyprland::ipc::socket2_path_from_env;
use crate::hyprland::monitor::MonitorState;
use crate::profile::r#match::{best_profile_match, decide_auto_apply, format_auto_apply_decision};
use crate::profile::store::ProfileStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorSeverity {
    Ok,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub severity: DoctorSeverity,
    pub label: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    pub monitors: Vec<MonitorState>,
    pub profile_count: usize,
    pub best_profile_summary: Option<String>,
}

impl DoctorReport {
    pub fn push(
        &mut self,
        severity: DoctorSeverity,
        label: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.checks.push(DoctorCheck {
            severity,
            label: label.into(),
            message: message.into(),
        });
    }
}

pub fn build_doctor_report(paths: &ConfigPaths) -> DoctorReport {
    let mut report = DoctorReport {
        checks: Vec::new(),
        monitors: Vec::new(),
        profile_count: 0,
        best_profile_summary: None,
    };

    report.push(
        DoctorSeverity::Info,
        "config dir",
        paths.config_dir.display().to_string(),
    );
    report.push(
        DoctorSeverity::Info,
        "config file",
        paths.config_file_path().display().to_string(),
    );
    report.push(
        DoctorSeverity::Info,
        "profile store",
        paths.profile_store_path().display().to_string(),
    );
    report.push(
        DoctorSeverity::Info,
        "generated directory",
        paths.generated_dir_path().display().to_string(),
    );
    report.push(
        DoctorSeverity::Info,
        "generated lua",
        generated_path_status(&paths.generated_monitors_lua_path()),
    );

    check_session_env(&mut report);
    check_socket(&mut report);
    check_systemd(&mut report);

    let store = match ProfileStore::load(paths.profile_store_path()) {
        Ok(store) => {
            report.profile_count = store.profiles.len();
            if store.profiles.is_empty() {
                report.push(DoctorSeverity::Warning, "profiles", "no profiles saved");
            } else {
                report.push(
                    DoctorSeverity::Ok,
                    "profiles",
                    format!("{} saved", store.profiles.len()),
                );
            }
            Some(store)
        }
        Err(error) => {
            report.push(
                DoctorSeverity::Error,
                "profiles",
                format!("failed to load profile store: {error:#}"),
            );
            None
        }
    };

    let config = match AppConfig::load(paths.config_file_path()) {
        Ok(config) => {
            report.push(
                DoctorSeverity::Ok,
                "config",
                format!(
                    "loaded (debounce={}ms, apply_on_start={})",
                    config.debounce_ms, config.apply_on_start
                ),
            );
            Some(config)
        }
        Err(error) => {
            report.push(
                DoctorSeverity::Error,
                "config",
                format!("failed to load config: {error:#}"),
            );
            None
        }
    };

    match current_monitors() {
        Ok(monitors) => {
            report.push(
                DoctorSeverity::Ok,
                "hyprctl monitors",
                format!(
                    "detected {} monitor{}",
                    monitors.len(),
                    plural(monitors.len())
                ),
            );
            check_monitor_identities(&mut report, &monitors);
            if let Some(store) = &store {
                check_stale_output_names(&mut report, store, &monitors);
            }
            if let (Some(store), Some(config)) = (&store, &config) {
                let best_match = best_profile_match(store, &monitors);
                let decision =
                    decide_auto_apply(store, &best_match, config.fallback_profile.as_deref());
                let mut summary = format_auto_apply_decision(&decision, "Best profile");
                if let Some(candidate) = best_match.candidates.first() {
                    summary.push_str(&format!(
                        "\nScore: {} ({} of {} profile monitors matched)",
                        candidate.score, candidate.matched_monitors, candidate.profile_monitors
                    ));
                }
                report.best_profile_summary = Some(summary);
            }
            report.monitors = monitors;
        }
        Err(error) => report.push(
            DoctorSeverity::Error,
            "hyprctl monitors",
            format!("{error:#}"),
        ),
    }

    report
}

fn check_session_env(report: &mut DoctorReport) {
    let using_fixture = env::var_os("HYPRDISJUST_MONITORS_JSON").is_some();
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR");
    let signature = env::var_os("HYPRLAND_INSTANCE_SIGNATURE");

    if using_fixture {
        report.push(
            DoctorSeverity::Info,
            "Hyprland session",
            "using HYPRDISJUST_MONITORS_JSON fixture",
        );
        return;
    }

    match (runtime_dir, signature) {
        (Some(runtime_dir), Some(signature))
            if !runtime_dir.is_empty() && !signature.is_empty() =>
        {
            report.push(
                DoctorSeverity::Ok,
                "Hyprland session",
                "environment detected",
            );
        }
        _ => report.push(
            DoctorSeverity::Warning,
            "Hyprland session",
            "XDG_RUNTIME_DIR or HYPRLAND_INSTANCE_SIGNATURE is missing",
        ),
    }
}

fn check_socket(report: &mut DoctorReport) {
    match socket2_path_from_env() {
        Ok(path) if path.exists() => report.push(
            DoctorSeverity::Ok,
            "socket2",
            format!("available at {}", path.display()),
        ),
        Ok(path) => report.push(
            DoctorSeverity::Warning,
            "socket2",
            format!("not found at {}", path.display()),
        ),
        Err(error) => report.push(DoctorSeverity::Warning, "socket2", error.to_string()),
    }
}

fn check_systemd(report: &mut DoctorReport) {
    match crate::systemd::user_service_path() {
        Ok(path) if path.exists() => report.push(
            DoctorSeverity::Ok,
            "systemd user service",
            format!("installed at {}", path.display()),
        ),
        Ok(path) => report.push(
            DoctorSeverity::Info,
            "systemd user service",
            format!(
                "not installed at {}; run `hyprdisjust install-systemd-user --enable --start`",
                path.display()
            ),
        ),
        Err(error) => report.push(
            DoctorSeverity::Warning,
            "systemd user service",
            format!("could not resolve install path: {error:#}"),
        ),
    }
}

fn check_monitor_identities(report: &mut DoctorReport, monitors: &[MonitorState]) {
    let mut warned = false;
    for monitor in monitors {
        if monitor.id.starts_with("output:") {
            report.push(
                DoctorSeverity::Warning,
                "monitor identity",
                format!(
                    "{} only has output-name identity `{}`",
                    monitor.output_name, monitor.id
                ),
            );
            warned = true;
        } else if monitor.id.contains(":output:") {
            report.push(
                DoctorSeverity::Warning,
                "monitor identity",
                format!(
                    "{} needed output-name disambiguation as `{}`",
                    monitor.output_name, monitor.id
                ),
            );
            warned = true;
        }
    }

    if !warned {
        report.push(
            DoctorSeverity::Ok,
            "monitor identity",
            "stable identities look usable",
        );
    }
}

fn check_stale_output_names(
    report: &mut DoctorReport,
    store: &ProfileStore,
    monitors: &[MonitorState],
) {
    let mut stale = BTreeSet::new();
    for profile in &store.profiles {
        for saved_monitor in &profile.monitors {
            let Some(current) = monitors
                .iter()
                .find(|monitor| monitor.id == saved_monitor.id)
            else {
                continue;
            };
            if !saved_monitor.name_hint.trim().is_empty()
                && saved_monitor.name_hint != current.output_name
            {
                stale.insert(format!(
                    "`{}` saved {} but current output is {}",
                    profile.name, saved_monitor.name_hint, current.output_name
                ));
            }
        }
    }

    if stale.is_empty() {
        report.push(
            DoctorSeverity::Ok,
            "saved output names",
            "no stale hints detected",
        );
    } else {
        for message in stale {
            report.push(DoctorSeverity::Warning, "saved output names", message);
        }
    }
}

fn generated_path_status(path: &Path) -> String {
    if path.exists() {
        format!("{} (exists)", path.display())
    } else {
        format!("{} (not generated yet)", path.display())
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}
