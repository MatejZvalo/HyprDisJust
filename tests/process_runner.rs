use std::process::Command;
use std::time::{Duration, Instant};

use hyprdisjust::process::run_bounded;

#[test]
fn bounded_runner_terminates_and_reaps_a_hanging_process_group() {
    let started = Instant::now();
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 30"]);

    let error = run_bounded(
        &mut command,
        "synthetic hanging query",
        Duration::from_millis(50),
        1024,
    )
    .unwrap_err();

    assert!(started.elapsed() < Duration::from_secs(2));
    let error = format!("{error:#}");
    assert!(error.contains("synthetic hanging query"));
    assert!(error.contains("terminated and reaped"));
}

#[test]
fn bounded_runner_caps_both_output_streams() {
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "i=0; while [ $i -lt 1000 ]; do printf x; printf y >&2; i=$((i+1)); done",
    ]);

    let output = run_bounded(
        &mut command,
        "synthetic noisy command",
        Duration::from_secs(2),
        64,
    )
    .unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 64);
    assert_eq!(output.stderr.len(), 64);
    assert!(output.stdout_truncated);
    assert!(output.stderr_truncated);
}

#[test]
fn bounded_runner_deadline_covers_descendant_held_pipes() {
    let started = Instant::now();
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 30 &"]);

    let error = run_bounded(
        &mut command,
        "synthetic inherited pipe",
        Duration::from_millis(100),
        1024,
    )
    .unwrap_err();

    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(format!("{error:#}").contains("exceeded"));
}

#[cfg(unix)]
#[test]
fn bounded_runner_does_not_wait_for_a_setsid_descendant_holding_a_pipe() {
    let started = Instant::now();
    let mut command = Command::new("sh");
    command.args(["-c", "setsid sh -c 'sleep 1' & exit 0"]);

    let error = run_bounded(
        &mut command,
        "synthetic escaped pipe holder",
        Duration::from_millis(100),
        1024,
    )
    .unwrap_err();

    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(format!("{error:#}").contains("exceeded"));
}
