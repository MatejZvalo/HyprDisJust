use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::config::{AppConfig, ConfigPaths};
use crate::hyprland::hyprctl::{current_monitors, live_monitors};
use crate::hyprland::ipc::MonitorSocketEvent;
use crate::hyprland::ipc::Socket2EventReader;
use crate::hyprland::monitor::MonitorState;
use crate::profile::apply::{
    apply_plan_safely, plan_apply, ApplyOutcome, TerminalConfirmation, CONFIRMATION_TIMEOUT,
};
use crate::profile::r#match::{
    best_profile_match, decide_auto_apply, format_auto_apply_decision, AutoApplyDecision,
};
use crate::profile::render::format_hyprctl_batch_command;
use crate::profile::store::{Profile, ProfileStore};

const RECONNECT_DELAY: Duration = Duration::from_secs(2);

pub trait MonitorEventInput {
    fn read_monitor_event(&mut self) -> anyhow::Result<Option<MonitorSocketEvent>>;
    fn read_monitor_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> anyhow::Result<Option<MonitorSocketEvent>>;
}

pub trait MonitorEventConnector {
    fn connect(&mut self) -> anyhow::Result<Box<dyn MonitorEventInput>>;
}

#[derive(Debug, Default)]
struct Socket2Connector;

impl MonitorEventConnector for Socket2Connector {
    fn connect(&mut self) -> anyhow::Result<Box<dyn MonitorEventInput>> {
        Ok(Box::new(Socket2EventReader::connect_from_env()?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionCycleOutcome {
    ConnectFailed(String),
    Disconnected(String),
}

pub fn run_connection_cycle(
    connector: &mut dyn MonitorEventConnector,
    process: &mut dyn FnMut(&mut dyn MonitorEventInput) -> anyhow::Result<()>,
) -> ConnectionCycleOutcome {
    match connector.connect() {
        Ok(mut reader) => match process(reader.as_mut()) {
            Ok(()) => ConnectionCycleOutcome::Disconnected(
                "Hyprland socket2 event input ended unexpectedly".to_owned(),
            ),
            Err(error) => ConnectionCycleOutcome::Disconnected(format!("{error:#}")),
        },
        Err(error) => ConnectionCycleOutcome::ConnectFailed(format!("{error:#}")),
    }
}

impl MonitorEventInput for Socket2EventReader {
    fn read_monitor_event(&mut self) -> anyhow::Result<Option<MonitorSocketEvent>> {
        Socket2EventReader::read_monitor_event(self)
    }

    fn read_monitor_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> anyhow::Result<Option<MonitorSocketEvent>> {
        Socket2EventReader::read_monitor_event_timeout(self, timeout)
    }
}

pub trait DaemonClock {
    fn now(&self) -> Instant;
    fn sleep(&mut self, duration: Duration);
}

#[derive(Debug, Default)]
struct SystemDaemonClock;

impl DaemonClock for SystemDaemonClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonOptions {
    pub once: bool,
    pub dry_run: bool,
    pub log_file: Option<PathBuf>,
    pub unattended: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AutoSwitchState {
    suppressed_attempt: Option<SuppressedAttempt>,
    connected_once: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuppressedAttempt {
    profile_name: String,
    batch: String,
    observed_layout: String,
}

impl AutoSwitchState {
    pub fn consume_suppressed_attempt(
        &mut self,
        profile_name: &str,
        batch: &str,
        monitors: &[MonitorState],
    ) -> bool {
        let observed_layout = monitor_layout_fingerprint(monitors);
        let suppress = self.suppressed_attempt.as_ref().is_some_and(|attempt| {
            attempt.profile_name == profile_name
                && attempt.batch == batch
                && attempt.observed_layout == observed_layout
        });
        self.suppressed_attempt = None;
        suppress
    }

    pub fn suppress_attempt(&mut self, profile_name: &str, batch: &str, monitors: &[MonitorState]) {
        self.suppressed_attempt = Some(SuppressedAttempt {
            profile_name: profile_name.to_owned(),
            batch: batch.to_owned(),
            observed_layout: monitor_layout_fingerprint(monitors),
        });
    }

    pub fn clear_suppression(&mut self) {
        self.suppressed_attempt = None;
    }

    pub fn begin_socket_session(&mut self) -> bool {
        let reconnecting = self.connected_once;
        self.connected_once = true;
        reconnecting
    }
}

fn monitor_layout_fingerprint(monitors: &[MonitorState]) -> String {
    let mut monitors = monitors.iter().collect::<Vec<_>>();
    monitors.sort_by(|left, right| left.output_name.cmp(&right.output_name));
    let mut fingerprint = String::new();
    for monitor in monitors {
        fingerprint.push_str(&format!(
            "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\n",
            monitor.output_name,
            monitor.id,
            monitor.enabled,
            monitor.x,
            monitor.y,
            monitor.width,
            monitor.height,
            monitor.refresh_rate.to_bits(),
            monitor.scale.to_bits(),
            monitor.transform,
        ));
    }
    fingerprint
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoApplyRun {
    pub decision: AutoApplyDecision,
    pub batch: Option<String>,
    pub applied: bool,
    pub skipped_noop: bool,
}

pub fn run(options: DaemonOptions) -> anyhow::Result<()> {
    let mut logger = DaemonLogger::new(options.log_file.as_ref())?;
    let paths = ConfigPaths::resolve()?;
    let config = AppConfig::load(paths.config_file_path())?;

    let mut state = AutoSwitchState::default();
    if options.once {
        run_once_with_paths(
            &paths,
            &config,
            options.dry_run,
            options.unattended,
            &mut state,
            &mut logger,
        )?;
        return Ok(());
    }
    if config.apply_on_start {
        if let Err(error) = run_once_with_paths(
            &paths,
            &config,
            options.dry_run,
            options.unattended,
            &mut state,
            &mut logger,
        ) {
            logger.log(&format!("Startup auto-apply failed: {error:#}"))?;
        }
    }

    logger.log("HyprDisJust daemon started")?;
    logger.log(&format!("Debounce: {}ms", config.debounce_ms))?;
    logger.log(if options.unattended {
        "Safety: unattended applies explicitly enabled"
    } else {
        "Safety: each apply requires `y` confirmation within 15 seconds"
    })?;
    run_socket_loop(
        &paths,
        &config,
        options.dry_run,
        options.unattended,
        &mut state,
        &mut logger,
    )
}

fn run_socket_loop(
    paths: &ConfigPaths,
    config: &AppConfig,
    dry_run: bool,
    unattended: bool,
    state: &mut AutoSwitchState,
    logger: &mut DaemonLogger,
) -> anyhow::Result<()> {
    let mut clock = SystemDaemonClock;
    let mut connector = Socket2Connector;
    loop {
        let outcome = run_connection_cycle(&mut connector, &mut |reader| {
            logger.log("Connected to Hyprland socket2")?;
            if state.begin_socket_session() {
                logger.log("Reconciling monitor topology after socket2 reconnect")?;
                debounce_monitor_events(reader, config.debounce_ms, logger)?;
                if let Err(error) =
                    run_once_with_paths(paths, config, dry_run, unattended, state, logger)
                {
                    logger.log(&format!("Reconnect auto-apply failed: {error:#}"))?;
                }
            }
            process_socket_events(reader, paths, config, dry_run, unattended, state, logger)
        });
        match outcome {
            ConnectionCycleOutcome::ConnectFailed(error) => {
                logger.log(&format!("Could not connect to Hyprland socket2: {error}"))?;
            }
            ConnectionCycleOutcome::Disconnected(error) => {
                logger.log(&format!("Socket2 disconnected: {error}"))?;
            }
        }

        logger.log(&format!(
            "Retrying Hyprland socket2 connection in {}s",
            RECONNECT_DELAY.as_secs()
        ))?;
        wait_for_reconnect(&mut clock);
    }
}

fn process_socket_events(
    reader: &mut dyn MonitorEventInput,
    paths: &ConfigPaths,
    config: &AppConfig,
    dry_run: bool,
    unattended: bool,
    state: &mut AutoSwitchState,
    logger: &mut DaemonLogger,
) -> anyhow::Result<()> {
    loop {
        let Some(event) = reader.read_monitor_event()? else {
            continue;
        };

        logger.log(&format!("Monitor event: {}", event.as_str()))?;
        debounce_monitor_events(reader, config.debounce_ms, logger)?;
        if let Err(error) = run_once_with_paths(paths, config, dry_run, unattended, state, logger) {
            logger.log(&format!("Auto-apply failed: {error:#}"))?;
        }
    }
}

fn debounce_monitor_events(
    reader: &mut dyn MonitorEventInput,
    debounce_ms: u64,
    logger: &mut DaemonLogger,
) -> anyhow::Result<()> {
    let clock = SystemDaemonClock;
    let events = debounce_monitor_event_burst(reader, Duration::from_millis(debounce_ms), &clock)?;
    for event in events {
        logger.log(&format!("Monitor event: {}", event.as_str()))?;
    }
    Ok(())
}

pub fn debounce_monitor_event_burst(
    reader: &mut dyn MonitorEventInput,
    debounce: Duration,
    clock: &dyn DaemonClock,
) -> anyhow::Result<Vec<MonitorSocketEvent>> {
    let mut last_monitor_event = clock.now();
    let mut events = Vec::new();
    loop {
        let Some(remaining) = debounce_remaining(last_monitor_event, clock.now(), debounce) else {
            return Ok(events);
        };
        match reader.read_monitor_event_timeout(remaining)? {
            Some(event) => {
                events.push(event);
                last_monitor_event = clock.now();
            }
            None => return Ok(events),
        }
    }
}

pub fn wait_for_reconnect(clock: &mut dyn DaemonClock) {
    clock.sleep(RECONNECT_DELAY);
}

pub fn debounce_remaining(
    last_monitor_event: Instant,
    now: Instant,
    debounce: Duration,
) -> Option<Duration> {
    let elapsed = now.saturating_duration_since(last_monitor_event);
    if elapsed >= debounce {
        None
    } else {
        Some(debounce - elapsed)
    }
}

fn run_once_with_paths(
    paths: &ConfigPaths,
    config: &AppConfig,
    dry_run: bool,
    unattended: bool,
    state: &mut AutoSwitchState,
    logger: &mut DaemonLogger,
) -> anyhow::Result<AutoApplyRun> {
    let store = ProfileStore::load(paths.profile_store_path())?;
    let monitors = if dry_run {
        current_monitors()?
    } else {
        live_monitors()?
    };
    let best_match = best_profile_match(&store, &monitors);
    let decision = decide_auto_apply(&store, &best_match, config.fallback_profile.as_deref());

    logger.log("Auto-apply decision")?;
    logger.log(&format_auto_apply_decision(&decision, "Selected profile"))?;

    let Some(profile_name) = decision.profile_name() else {
        state.clear_suppression();
        return Ok(AutoApplyRun {
            decision,
            batch: None,
            applied: false,
            skipped_noop: false,
        });
    };

    let profile = profile_by_name(&store, profile_name)?;
    let plan = plan_apply(profile, &monitors)?;
    logger.log(&format!(
        "Command: {}",
        format_hyprctl_batch_command(&plan.batch)
    ))?;
    log_apply_warnings(&plan.warnings, logger)?;

    if plan.is_noop {
        state.clear_suppression();
        logger.log("No changes: selected profile is already active")?;
        if dry_run {
            logger.log("Dry run: monitor layout was not changed")?;
        }
        return Ok(AutoApplyRun {
            decision,
            batch: Some(plan.batch),
            applied: false,
            skipped_noop: true,
        });
    }

    if dry_run {
        logger.log("Dry run: monitor layout was not changed")?;
        return Ok(AutoApplyRun {
            decision,
            batch: Some(plan.batch),
            applied: false,
            skipped_noop: false,
        });
    }

    if state.consume_suppressed_attempt(&plan.profile_name, &plan.batch, &monitors) {
        logger.log("No changes: identical failed or rejected apply remains suppressed")?;
        return Ok(AutoApplyRun {
            decision,
            batch: Some(plan.batch),
            applied: false,
            skipped_noop: true,
        });
    }

    let outcome_result = if unattended {
        apply_plan_safely(&plan, None)
    } else {
        logger.log(&format!(
            "Waiting {}s for `y` confirmation after apply",
            CONFIRMATION_TIMEOUT.as_secs()
        ))?;
        let mut confirmation = TerminalConfirmation;
        apply_plan_safely(&plan, Some(&mut confirmation))
    }
    .map_err(|error| {
        anyhow::anyhow!(
            "{error:#}\nPrevious layout:\n{}",
            format_previous_layout(&monitors)
        )
    });
    let outcome = match outcome_result {
        Ok(outcome) => outcome,
        Err(error) => {
            state.suppress_attempt(&plan.profile_name, &plan.batch, &monitors);
            return Err(error);
        }
    };

    let applied = matches!(outcome, ApplyOutcome::Confirmed | ApplyOutcome::Unattended);
    match &outcome {
        ApplyOutcome::Confirmed => logger.log(&format!(
            "Confirmed profile `{}` with {} monitor rule{}",
            plan.profile_name,
            plan.rules.len(),
            if plan.rules.len() == 1 { "" } else { "s" }
        ))?,
        ApplyOutcome::Unattended => logger.log(&format!(
            "Applied profile `{}` with {} monitor rule{} without confirmation (--unattended)",
            plan.profile_name,
            plan.rules.len(),
            if plan.rules.len() == 1 { "" } else { "s" }
        ))?,
        ApplyOutcome::RolledBack { reason } => logger.log(&format!(
            "Profile `{}` was not confirmed ({reason}); previous monitor layout restored",
            plan.profile_name
        ))?,
        ApplyOutcome::Noop => logger.log("No changes: selected profile is already active")?,
    }
    if matches!(outcome, ApplyOutcome::RolledBack { .. }) {
        state.suppress_attempt(&plan.profile_name, &plan.batch, &monitors);
    } else {
        state.clear_suppression();
    }

    Ok(AutoApplyRun {
        decision,
        batch: Some(plan.batch),
        applied,
        skipped_noop: false,
    })
}

fn profile_by_name<'a>(store: &'a ProfileStore, name: &str) -> anyhow::Result<&'a Profile> {
    store
        .profiles
        .iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| anyhow::anyhow!("profile `{name}` does not exist"))
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
