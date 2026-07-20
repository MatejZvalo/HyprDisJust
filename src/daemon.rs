use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::atomic::{ensure_private_directory, open_private_append};
use crate::config::{AppConfig, ConfigPaths};
use crate::hyprland::hyprctl::current_monitors;
use crate::hyprland::ipc::MonitorSocketEvent;
use crate::hyprland::ipc::Socket2EventReader;
use crate::hyprland::monitor::MonitorState;
use crate::profile::apply::{
    execute_apply_transaction_if, plan_apply_automatic, ApplyExecution, ApplyOutcome,
    ApplyTransactionRequest, ApplyTransactionState, TerminalConfirmation, CONFIRMATION_TIMEOUT,
};
use crate::profile::r#match::{
    best_profile_match, decide_auto_apply, format_auto_apply_decision, AutoApplyDecision,
};
use crate::profile::render::format_hyprctl_batch_command;
use crate::profile::store::{Profile, ProfileStore};
use crate::text::{sanitize_multiline_text, write_stdout_line};

const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const MAX_DAEMON_LOG_BYTES: u64 = 1024 * 1024;

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
    if !dry_run {
        return run_live_auto_apply(paths, config, unattended, state, logger);
    }
    let store = ProfileStore::load(paths.profile_store_path())?;
    let monitors = current_monitors()?;
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
    let plan = plan_apply_automatic(profile, &monitors)?;
    logger.log(&format!(
        "Command: {}",
        format_hyprctl_batch_command(&plan.batch)
    ))?;
    log_apply_warnings(&plan.warnings, logger)?;

    if plan.is_noop {
        state.clear_suppression();
        logger.log("No changes: selected profile is already active")?;
        logger.log("Dry run: monitor layout was not changed")?;
        return Ok(AutoApplyRun {
            decision,
            batch: Some(plan.batch),
            applied: false,
            skipped_noop: true,
        });
    }

    logger.log("Dry run: monitor layout was not changed")?;
    Ok(AutoApplyRun {
        decision,
        batch: Some(plan.batch),
        applied: false,
        skipped_noop: false,
    })
}

fn run_live_auto_apply(
    paths: &ConfigPaths,
    config: &AppConfig,
    unattended: bool,
    state: &mut AutoSwitchState,
    logger: &mut DaemonLogger,
) -> anyhow::Result<AutoApplyRun> {
    let request = ApplyTransactionRequest::Automatic {
        fallback_profile: config.fallback_profile.clone(),
    };
    let mut attempted = None;
    let mut suppressed = false;
    let transaction = {
        let mut before_apply = |plan: &crate::profile::apply::ApplyPlan,
                                snapshot: &[MonitorState],
                                decision: Option<&AutoApplyDecision>|
         -> anyhow::Result<bool> {
            if let Some(decision) = decision {
                logger.log("Auto-apply decision")?;
                logger.log(&format_auto_apply_decision(decision, "Selected profile"))?;
            }
            logger.log(&format!(
                "Command: {}",
                format_hyprctl_batch_command(&plan.batch)
            ))?;
            log_apply_warnings(&plan.warnings, logger)?;
            if plan.is_noop {
                return Ok(true);
            }
            if state.consume_suppressed_attempt(&plan.profile_name, &plan.batch, snapshot) {
                suppressed = true;
                return Ok(false);
            }
            attempted = Some((
                plan.profile_name.clone(),
                plan.batch.clone(),
                snapshot.to_vec(),
            ));
            if !unattended {
                logger.log(&format!(
                    "Waiting {}s for `y` confirmation after apply",
                    CONFIRMATION_TIMEOUT.as_secs()
                ))?;
            }
            Ok(true)
        };

        if unattended {
            execute_apply_transaction_if(
                paths.profile_store_path(),
                request,
                None,
                &mut before_apply,
            )
        } else {
            let mut confirmation = TerminalConfirmation;
            execute_apply_transaction_if(
                paths.profile_store_path(),
                request,
                Some(&mut confirmation),
                &mut before_apply,
            )
        }
    };
    let transaction = match transaction {
        Ok(transaction) => transaction,
        Err(error) => {
            if let Some((profile_name, batch, snapshot)) = attempted {
                state.suppress_attempt(&profile_name, &batch, &snapshot);
            }
            return Err(error);
        }
    };
    match transaction {
        ApplyTransactionState::NoAutomaticMatch { decision, snapshot } => {
            logger.log("Auto-apply decision")?;
            logger.log(&format_auto_apply_decision(&decision, "Selected profile"))?;
            logger.log(&format!("Observed monitors: {}", snapshot.len()))?;
            state.clear_suppression();
            Ok(AutoApplyRun {
                decision,
                batch: None,
                applied: false,
                skipped_noop: false,
            })
        }
        ApplyTransactionState::Completed(transaction) => {
            let decision = transaction.automatic_decision.ok_or_else(|| {
                anyhow::anyhow!("automatic transaction did not return its selection decision")
            })?;
            let plan = transaction.plan;
            if transaction.execution == ApplyExecution::Suppressed {
                debug_assert!(suppressed);
                logger.log("No changes: identical failed or rejected apply remains suppressed")?;
                return Ok(AutoApplyRun {
                    decision,
                    batch: Some(plan.batch),
                    applied: false,
                    skipped_noop: true,
                });
            }
            let outcome = transaction.outcome;
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
                ApplyOutcome::Noop => {
                    logger.log("No changes: selected profile is already active")?
                }
            }
            if matches!(outcome, ApplyOutcome::RolledBack { .. }) {
                state.suppress_attempt(&plan.profile_name, &plan.batch, &transaction.snapshot);
            } else {
                state.clear_suppression();
            }
            Ok(AutoApplyRun {
                decision,
                batch: Some(plan.batch),
                applied,
                skipped_noop: matches!(outcome, ApplyOutcome::Noop),
            })
        }
    }
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

#[derive(Debug)]
struct DaemonLogger {
    log_file: Option<File>,
    log_path: Option<PathBuf>,
    bytes_written: u64,
}

impl DaemonLogger {
    fn new(path: Option<&PathBuf>) -> anyhow::Result<Self> {
        let Some(path) = path else {
            return Ok(Self {
                log_file: None,
                log_path: None,
                bytes_written: 0,
            });
        };
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        ensure_private_directory(parent)?;
        let mut log_file = open_private_append(path, "daemon log file")?;
        let mut bytes_written = log_file
            .metadata()
            .with_context(|| format!("failed to inspect daemon log file {}", path.display()))?
            .len();
        if bytes_written >= MAX_DAEMON_LOG_BYTES {
            log_file.flush().context("failed to flush daemon log")?;
            drop(log_file);
            rotate_daemon_log(path)?;
            log_file = open_private_append(path, "daemon log file")?;
            bytes_written = 0;
        }

        Ok(Self {
            log_file: Some(log_file),
            log_path: Some(path.clone()),
            bytes_written,
        })
    }

    fn log(&mut self, message: &str) -> anyhow::Result<()> {
        let mut message = sanitize_log_message(message);
        truncate_utf8(&mut message, (MAX_DAEMON_LOG_BYTES - 1) as usize);
        write_stdout_line(&message).context("failed to write daemon output")?;
        let added_bytes = u64::try_from(message.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        if self.log_file.is_some()
            && self.bytes_written > 0
            && self.bytes_written.saturating_add(added_bytes) > MAX_DAEMON_LOG_BYTES
        {
            self.rotate()?;
        }
        if let Some(file) = &mut self.log_file {
            writeln!(file, "{message}").context("failed to write daemon log")?;
            file.flush().context("failed to flush daemon log")?;
            self.bytes_written = self.bytes_written.saturating_add(added_bytes);
        }
        Ok(())
    }

    fn rotate(&mut self) -> anyhow::Result<()> {
        let path = self
            .log_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("daemon log path is unavailable"))?;
        if let Some(mut file) = self.log_file.take() {
            file.flush().context("failed to flush daemon log")?;
            file.sync_all().context("failed to sync daemon log")?;
        }
        rotate_daemon_log(path)?;
        self.log_file = Some(open_private_append(path, "daemon log file")?);
        self.bytes_written = 0;
        Ok(())
    }
}

fn truncate_utf8(value: &mut String, maximum_bytes: usize) {
    if value.len() <= maximum_bytes {
        return;
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn rotate_daemon_log(path: &Path) -> anyhow::Result<()> {
    let rotated = path.with_extension("log.1");
    std::fs::rename(path, &rotated).with_context(|| {
        format!(
            "failed to rotate daemon log {} to {}",
            path.display(),
            rotated.display()
        )
    })?;
    cap_rotated_daemon_log(&rotated)
}

fn cap_rotated_daemon_log(path: &Path) -> anyhow::Result<()> {
    let length = std::fs::metadata(path)
        .with_context(|| format!("failed to inspect rotated daemon log {}", path.display()))?
        .len();
    if length <= MAX_DAEMON_LOG_BYTES {
        return Ok(());
    }

    let mut source = File::open(path)
        .with_context(|| format!("failed to open rotated daemon log {}", path.display()))?;
    source
        .seek(SeekFrom::Start(length - MAX_DAEMON_LOG_BYTES))
        .with_context(|| format!("failed to seek rotated daemon log {}", path.display()))?;
    let mut tail = Vec::with_capacity(MAX_DAEMON_LOG_BYTES as usize);
    (&mut source)
        .take(MAX_DAEMON_LOG_BYTES)
        .read_to_end(&mut tail)
        .with_context(|| format!("failed to read rotated daemon log {}", path.display()))?;
    drop(source);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut archive = options
        .open(path)
        .with_context(|| format!("failed to rewrite rotated daemon log {}", path.display()))?;
    archive
        .write_all(&tail)
        .with_context(|| format!("failed to cap rotated daemon log {}", path.display()))?;
    archive
        .sync_all()
        .with_context(|| format!("failed to sync rotated daemon log {}", path.display()))?;
    Ok(())
}

fn sanitize_log_message(message: &str) -> String {
    sanitize_multiline_text(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_log_rotates_when_crossing_limit_during_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon.log");
        std::fs::write(&path, vec![b'x'; MAX_DAEMON_LOG_BYTES as usize - 4]).unwrap();
        let mut logger = DaemonLogger::new(Some(&path)).unwrap();

        logger.log("cross-limit").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "cross-limit\n");
        assert_eq!(
            std::fs::metadata(path.with_extension("log.1"))
                .unwrap()
                .len(),
            MAX_DAEMON_LOG_BYTES - 4
        );
    }

    #[test]
    fn startup_rotation_caps_an_oversized_archive() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("daemon.log");
        std::fs::write(&path, vec![b'x'; MAX_DAEMON_LOG_BYTES as usize + 128]).unwrap();

        let logger = DaemonLogger::new(Some(&path)).unwrap();
        drop(logger);

        assert_eq!(
            std::fs::metadata(path.with_extension("log.1"))
                .unwrap()
                .len(),
            MAX_DAEMON_LOG_BYTES
        );
    }

    #[cfg(unix)]
    #[test]
    fn daemon_log_rejects_symlink_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let victim = temp.path().join("victim");
        let path = temp.path().join("daemon.log");
        std::fs::write(&victim, "unchanged").unwrap();
        symlink(&victim, &path).unwrap();

        let error = DaemonLogger::new(Some(&path)).unwrap_err();

        assert!(format!("{error:#}").contains("daemon log file"));
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "unchanged");
    }
}
