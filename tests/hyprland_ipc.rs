use std::cell::Cell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use hyprdisjust::daemon::{
    debounce_monitor_event_burst, debounce_remaining, run_connection_cycle, wait_for_reconnect,
    AutoSwitchState, ConnectionCycleOutcome, DaemonClock, MonitorEventConnector, MonitorEventInput,
};
use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::hyprland::ipc::{
    discover_socket2_path_with, parse_monitor_event, resolve_socket2_path_with, socket2_path,
    MonitorSocketEvent,
};

#[test]
fn parses_socket2_monitor_events() {
    assert_eq!(
        parse_monitor_event("monitoradded>>DP-1\n"),
        Some(MonitorSocketEvent::Added)
    );
    assert_eq!(
        parse_monitor_event("monitorremoved>>DP-1\n"),
        Some(MonitorSocketEvent::Removed)
    );
    assert_eq!(
        parse_monitor_event("monitoraddedv2>>1,DP-1,Acme Panel\n"),
        Some(MonitorSocketEvent::AddedV2)
    );
    assert_eq!(
        parse_monitor_event("monitorremovedv2>>1,DP-1,Acme Panel\r\n"),
        Some(MonitorSocketEvent::RemovedV2)
    );
}

#[test]
fn ignores_unrelated_socket2_events() {
    assert_eq!(parse_monitor_event("workspace>>2\n"), None);
    assert_eq!(parse_monitor_event("monitoradded"), None);
    assert_eq!(parse_monitor_event(""), None);
}

#[test]
fn malformed_monitor_events_are_ignored_deterministically() {
    assert_eq!(parse_monitor_event("monitoradded>>\n"), None);
    assert_eq!(parse_monitor_event("monitorremovedv2>>bad-id,DP-1\n"), None);
    assert_eq!(
        parse_monitor_event("monitoraddedv2>>1,,description\n"),
        None
    );
}

#[test]
fn builds_socket2_path() {
    let path = socket2_path(PathBuf::from("/run/user/1000"), "signature");

    assert_eq!(
        path,
        PathBuf::from("/run/user/1000/hypr/signature/.socket2.sock")
    );
}

#[test]
fn discovers_single_socket2_path_from_runtime_dir() {
    let temp = tempfile::tempdir().unwrap();
    let instance = temp.path().join("hypr").join("signature");
    std::fs::create_dir_all(&instance).unwrap();
    let socket = instance.join(".socket2.sock");
    std::fs::write(&socket, "").unwrap();

    assert_eq!(
        discover_socket2_path_with(temp.path(), |path| path.exists()).unwrap(),
        socket
    );
}

#[test]
fn refuses_ambiguous_socket2_discovery() {
    let temp = tempfile::tempdir().unwrap();
    for signature in ["one", "two"] {
        let instance = temp.path().join("hypr").join(signature);
        std::fs::create_dir_all(&instance).unwrap();
        std::fs::write(instance.join(".socket2.sock"), "").unwrap();
    }

    let error = discover_socket2_path_with(temp.path(), |path| path.exists())
        .unwrap_err()
        .to_string();

    assert!(error.contains("multiple socket2 sockets"));
}

#[test]
fn stale_instance_signature_falls_back_to_the_unique_live_socket() {
    let temp = tempfile::tempdir().unwrap();
    let live_instance = temp.path().join("hypr").join("new-signature");
    std::fs::create_dir_all(&live_instance).unwrap();
    let live_socket = live_instance.join(".socket2.sock");
    std::fs::write(&live_socket, "").unwrap();

    let resolved = resolve_socket2_path_with(
        temp.path(),
        Some(std::ffi::OsStr::new("stale-signature")),
        |path| path.exists(),
    )
    .unwrap();

    assert_eq!(resolved, live_socket);
}

#[test]
fn debounce_decision_waits_from_the_last_monitor_event() {
    let event = Instant::now();
    let debounce = Duration::from_millis(900);

    assert_eq!(
        debounce_remaining(event, event + Duration::from_millis(250), debounce),
        Some(Duration::from_millis(650))
    );
    assert_eq!(
        debounce_remaining(event, event + Duration::from_millis(900), debounce),
        None
    );
    assert_eq!(
        debounce_remaining(event, event + Duration::from_millis(901), debounce),
        None
    );
}

#[derive(Clone)]
struct FakeClock {
    start: Instant,
    elapsed_ms: Rc<Cell<u64>>,
    sleeps: Rc<Cell<u64>>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            elapsed_ms: Rc::new(Cell::new(0)),
            sleeps: Rc::new(Cell::new(0)),
        }
    }
}

impl DaemonClock for FakeClock {
    fn now(&self) -> Instant {
        self.start + Duration::from_millis(self.elapsed_ms.get())
    }

    fn sleep(&mut self, duration: Duration) {
        self.sleeps.set(duration.as_millis() as u64);
        self.elapsed_ms
            .set(self.elapsed_ms.get() + duration.as_millis() as u64);
    }
}

struct FakeEvents {
    clock: FakeClock,
    events: VecDeque<(u64, Option<MonitorSocketEvent>)>,
    timeouts: Vec<Duration>,
}

impl MonitorEventInput for FakeEvents {
    fn read_monitor_event(&mut self) -> anyhow::Result<Option<MonitorSocketEvent>> {
        self.read_monitor_event_timeout(Duration::MAX)
    }

    fn read_monitor_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> anyhow::Result<Option<MonitorSocketEvent>> {
        self.timeouts.push(timeout);
        let (advance_ms, event) = self.events.pop_front().unwrap();
        self.clock
            .elapsed_ms
            .set(self.clock.elapsed_ms.get() + advance_ms);
        Ok(event)
    }
}

#[test]
fn daemon_debounces_a_burst_into_one_topology_decision() {
    let clock = FakeClock::new();
    let mut events = FakeEvents {
        clock: clock.clone(),
        events: VecDeque::from([
            (100, Some(MonitorSocketEvent::Added)),
            (250, Some(MonitorSocketEvent::RemovedV2)),
            (900, None),
        ]),
        timeouts: Vec::new(),
    };

    let drained =
        debounce_monitor_event_burst(&mut events, Duration::from_millis(900), &clock).unwrap();

    assert_eq!(
        drained,
        vec![MonitorSocketEvent::Added, MonitorSocketEvent::RemovedV2]
    );
    assert_eq!(events.timeouts.len(), 3);
    assert_eq!(events.timeouts[0], Duration::from_millis(900));
    assert_eq!(events.timeouts[1], Duration::from_millis(900));
}

#[test]
fn daemon_suppresses_rejected_attempt_only_while_observed_layout_is_unchanged() {
    let monitors =
        parse_monitors_output(include_str!("fixtures/hyprctl-monitors-desk.json")).unwrap();
    let mut state = AutoSwitchState::default();
    assert!(!state.consume_suppressed_attempt("desk", "batch-a", &monitors));

    state.suppress_attempt("desk", "batch-a", &monitors);
    assert!(state.consume_suppressed_attempt("desk", "batch-a", &monitors));
    assert!(!state.consume_suppressed_attempt("desk", "batch-a", &monitors));

    state.suppress_attempt("desk", "batch-a", &monitors);
    let mut changed = monitors.clone();
    changed[0].x += 10;
    assert!(!state.consume_suppressed_attempt("desk", "batch-a", &changed));

    state.suppress_attempt("desk", "batch-a", &monitors);
    state.clear_suppression();
    assert!(!state.consume_suppressed_attempt("desk", "batch-a", &monitors));
}

#[test]
fn daemon_marks_only_later_socket_sessions_as_reconnects() {
    let mut state = AutoSwitchState::default();

    assert!(!state.begin_socket_session());
    assert!(state.begin_socket_session());
    assert!(state.begin_socket_session());
}

#[test]
fn socket_disconnects_use_the_deterministic_reconnect_delay() {
    let mut clock = FakeClock::new();

    wait_for_reconnect(&mut clock);

    assert_eq!(clock.sleeps.get(), 2_000);
}

struct FakeConnector {
    attempts: usize,
}

impl MonitorEventConnector for FakeConnector {
    fn connect(&mut self) -> anyhow::Result<Box<dyn MonitorEventInput>> {
        self.attempts += 1;
        if self.attempts == 1 {
            anyhow::bail!("synthetic connect failure");
        }

        let clock = FakeClock::new();
        Ok(Box::new(FakeEvents {
            clock,
            events: VecDeque::from([(0, None)]),
            timeouts: Vec::new(),
        }))
    }
}

#[test]
fn socket_reconnect_cycle_recovers_after_connect_failure() {
    let mut connector = FakeConnector { attempts: 0 };
    let mut processed_connections = 0;
    let first = run_connection_cycle(&mut connector, &mut |_| {
        processed_connections += 1;
        Ok(())
    });
    assert!(matches!(first, ConnectionCycleOutcome::ConnectFailed(_)));

    let mut clock = FakeClock::new();
    wait_for_reconnect(&mut clock);
    let second = run_connection_cycle(&mut connector, &mut |_| {
        processed_connections += 1;
        anyhow::bail!("synthetic disconnect")
    });

    assert!(matches!(second, ConnectionCycleOutcome::Disconnected(_)));
    assert_eq!(connector.attempts, 2);
    assert_eq!(processed_connections, 1);
    assert_eq!(clock.sleeps.get(), 2_000);
}
