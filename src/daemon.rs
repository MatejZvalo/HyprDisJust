use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::Context;

use crate::config::{AppConfig, ConfigPaths};
use crate::hyprland::hyprctl::current_monitors;
use crate::hyprland::ipc::Socket2EventReader;
use crate::hyprland::monitor::MonitorState;
use crate::profile::apply::{apply_plan, plan_apply};
use crate::profile::r#match::{best_profile_match, BestProfileMatch};
use crate::profile::render::format_hyprctl_batch_command;
use crate::profile::store::{Profile, ProfileStore};

const RECONNECT_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonOptions {
    pub once: bool,
    pub dry_run: bool,
    pub log_file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoApplyDecision {
    Apply {
        profile_name: String,
        confidence: String,
        reason: String,
    },
    Ambiguous {
        reason: String,
    },
    MissingFallback {
        profile_name: String,
    },
    NoProfiles,
    NotEligible {
        reason: String,
    },
    NoMatch,
}

impl AutoApplyDecision {
    pub fn profile_name(&self) -> Option<&str> {
        match self {
            Self::Apply { profile_name, .. } => Some(profile_name),
            Self::Ambiguous { .. }
            | Self::MissingFallback { .. }
            | Self::NoProfiles
            | Self::NotEligible { .. }
            | Self::NoMatch => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoApplyRun {
    pub decision: AutoApplyDecision,
    pub batch: Option<String>,
    pub applied: bool,
}

pub fn run(options: DaemonOptions) -> anyhow::Result<()> {
    let mut logger = DaemonLogger::new(options.log_file.as_ref())?;
    let paths = ConfigPaths::resolve()?;
    let config = AppConfig::load(paths.config_file_path())?;

    if options.once || config.apply_on_start {
        run_once_with_paths(&paths, &config, options.dry_run, &mut logger)?;
        if options.once {
            return Ok(());
        }
    }

    logger.log("HyprDisJust daemon started")?;
    logger.log(&format!("Debounce: {}ms", config.debounce_ms))?;
    run_socket_loop(&paths, &config, options.dry_run, &mut logger)
}

pub fn decide_auto_apply(
    store: &ProfileStore,
    best_match: &BestProfileMatch,
    fallback_profile: Option<&str>,
) -> AutoApplyDecision {
    if store.profiles.is_empty() {
        return AutoApplyDecision::NoProfiles;
    }

    if let Some(selected) = best_match
        .selected
        .as_ref()
        .filter(|selected| selected.confidence.is_auto_apply_eligible())
    {
        return AutoApplyDecision::Apply {
            profile_name: selected.profile_name.clone(),
            confidence: selected.confidence.as_str().to_owned(),
            reason: first_reason(&selected.reasons, "profile matched"),
        };
    }

    if best_match.ambiguous && has_auto_eligible_candidate(best_match) {
        return AutoApplyDecision::Ambiguous {
            reason: best_ambiguous_reason(best_match),
        };
    }

    if let Some(fallback_profile) = normalized_fallback_profile(fallback_profile) {
        if store.has_profile(fallback_profile) {
            return AutoApplyDecision::Apply {
                profile_name: fallback_profile.to_owned(),
                confidence: "fallback".to_owned(),
                reason: "no exact or high-confidence match; fallback_profile is configured"
                    .to_owned(),
            };
        }

        return AutoApplyDecision::MissingFallback {
            profile_name: fallback_profile.to_owned(),
        };
    }

    if best_match.ambiguous {
        return AutoApplyDecision::Ambiguous {
            reason: best_ambiguous_reason(best_match),
        };
    }

    if let Some(selected) = &best_match.selected {
        return AutoApplyDecision::NotEligible {
            reason: first_reason(
                &selected.reasons,
                "profile match is not eligible for automatic apply",
            ),
        };
    }

    AutoApplyDecision::NoMatch
}

pub fn format_auto_apply_decision(decision: &AutoApplyDecision, profile_label: &str) -> String {
    match decision {
        AutoApplyDecision::Apply {
            profile_name,
            confidence,
            reason,
        } => {
            format!("{profile_label}: {profile_name}\nConfidence: {confidence}\nReason: {reason}")
        }
        AutoApplyDecision::Ambiguous { reason } => {
            format!("{profile_label}: ambiguous\nConfidence: ambiguous\nReason: {reason}")
        }
        AutoApplyDecision::MissingFallback { profile_name } => format!(
            "{profile_label}: none\nConfidence: none\nReason: fallback_profile `{profile_name}` does not exist"
        ),
        AutoApplyDecision::NoProfiles => {
            format!("{profile_label}: none\nConfidence: none\nReason: no profiles saved")
        }
        AutoApplyDecision::NotEligible { reason } => {
            format!("{profile_label}: none\nConfidence: none\nReason: {reason}")
        }
        AutoApplyDecision::NoMatch => {
            format!("{profile_label}: none\nConfidence: none\nReason: no useful profile match")
        }
    }
}

fn run_socket_loop(
    paths: &ConfigPaths,
    config: &AppConfig,
    dry_run: bool,
    logger: &mut DaemonLogger,
) -> anyhow::Result<()> {
    loop {
        match Socket2EventReader::connect_from_env() {
            Ok(mut reader) => {
                logger.log("Connected to Hyprland socket2")?;
                if let Err(error) =
                    process_socket_events(&mut reader, paths, config, dry_run, logger)
                {
                    logger.log(&format!("Socket2 disconnected: {error}"))?;
                }
            }
            Err(error) => {
                logger.log(&format!("Could not connect to Hyprland socket2: {error}"))?;
            }
        }

        logger.log(&format!(
            "Retrying Hyprland socket2 connection in {}s",
            RECONNECT_DELAY.as_secs()
        ))?;
        thread::sleep(RECONNECT_DELAY);
    }
}

fn process_socket_events(
    reader: &mut Socket2EventReader,
    paths: &ConfigPaths,
    config: &AppConfig,
    dry_run: bool,
    logger: &mut DaemonLogger,
) -> anyhow::Result<()> {
    loop {
        let Some(event) = reader.read_monitor_event()? else {
            continue;
        };

        logger.log(&format!("Monitor event: {}", event.as_str()))?;
        debounce_monitor_events(reader, config.debounce_ms, logger)?;
        run_once_with_paths(paths, config, dry_run, logger)?;
    }
}

fn debounce_monitor_events(
    reader: &mut Socket2EventReader,
    debounce_ms: u64,
    logger: &mut DaemonLogger,
) -> anyhow::Result<()> {
    let debounce = Duration::from_millis(debounce_ms);
    loop {
        match reader.read_monitor_event_timeout(debounce)? {
            Some(event) => logger.log(&format!("Monitor event: {}", event.as_str()))?,
            None => return Ok(()),
        }
    }
}

fn run_once_with_paths(
    paths: &ConfigPaths,
    config: &AppConfig,
    dry_run: bool,
    logger: &mut DaemonLogger,
) -> anyhow::Result<AutoApplyRun> {
    let store = ProfileStore::load(paths.profile_store_path())?;
    let monitors = current_monitors()?;
    let best_match = best_profile_match(&store, &monitors);
    let decision = decide_auto_apply(&store, &best_match, config.fallback_profile.as_deref());

    logger.log("Auto-apply decision")?;
    logger.log(&format_auto_apply_decision(&decision, "Selected profile"))?;

    let Some(profile_name) = decision.profile_name() else {
        return Ok(AutoApplyRun {
            decision,
            batch: None,
            applied: false,
        });
    };

    let profile = profile_by_name(&store, profile_name)?;
    let plan = plan_apply(profile, &monitors)?;
    logger.log(&format!(
        "Command: {}",
        format_hyprctl_batch_command(&plan.batch)
    ))?;
    log_apply_warnings(&plan.warnings, logger)?;

    if dry_run {
        logger.log("Dry run: monitor layout was not changed")?;
        return Ok(AutoApplyRun {
            decision,
            batch: Some(plan.batch),
            applied: false,
        });
    }

    if let Err(error) = apply_plan(&plan) {
        anyhow::bail!(
            "{error:#}\nPrevious layout:\n{}",
            format_previous_layout(&monitors)
        );
    }
    logger.log(&format!(
        "Applied profile `{}` with {} monitor rule{}",
        plan.profile_name,
        plan.rules.len(),
        if plan.rules.len() == 1 { "" } else { "s" }
    ))?;

    Ok(AutoApplyRun {
        decision,
        batch: Some(plan.batch),
        applied: true,
    })
}

fn profile_by_name<'a>(store: &'a ProfileStore, name: &str) -> anyhow::Result<&'a Profile> {
    store
        .profiles
        .iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| anyhow::anyhow!("profile `{name}` does not exist"))
}

fn has_auto_eligible_candidate(best_match: &BestProfileMatch) -> bool {
    best_match
        .candidates
        .iter()
        .any(|candidate| candidate.confidence.is_auto_apply_eligible())
}

fn normalized_fallback_profile(fallback_profile: Option<&str>) -> Option<&str> {
    fallback_profile
        .map(str::trim)
        .filter(|fallback_profile| !fallback_profile.is_empty())
}

fn best_ambiguous_reason(best_match: &BestProfileMatch) -> String {
    best_match
        .candidates
        .first()
        .and_then(|candidate| candidate.reasons.first())
        .map(String::as_str)
        .unwrap_or("multiple profiles or monitors matched equally")
        .to_owned()
}

fn first_reason(reasons: &[String], fallback: &str) -> String {
    reasons
        .first()
        .map(String::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

fn log_apply_warnings(
    warnings: &[crate::profile::apply::ApplyWarning],
    logger: &mut DaemonLogger,
) -> anyhow::Result<()> {
    for warning in warnings {
        logger.log(&format!("Warning: {}", warning.message()))?;
    }

    Ok(())
}

fn format_previous_layout(monitors: &[MonitorState]) -> String {
    let mut output = format!("Monitors: {}", monitors.len());
    for monitor in monitors {
        output.push_str(&format!(
            "\n- {} {}x{}@{} at {}x{} scale {}",
            monitor.output_name,
            monitor.width,
            monitor.height,
            format_number(monitor.refresh_rate),
            monitor.x,
            monitor.y,
            format_number(monitor.scale)
        ));
    }

    output
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

struct DaemonLogger {
    log_file: Option<File>,
}

impl DaemonLogger {
    fn new(path: Option<&PathBuf>) -> anyhow::Result<Self> {
        let log_file = path
            .map(|path| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .with_context(|| format!("failed to open daemon log file {}", path.display()))
            })
            .transpose()?;

        Ok(Self { log_file })
    }

    fn log(&mut self, message: &str) -> anyhow::Result<()> {
        println!("{message}");
        if let Some(file) = &mut self.log_file {
            writeln!(file, "{message}").context("failed to write daemon log")?;
            file.flush().context("failed to flush daemon log")?;
        }
        Ok(())
    }
}
