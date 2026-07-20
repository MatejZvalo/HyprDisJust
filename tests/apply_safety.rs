use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use anyhow::anyhow;
use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::hyprland::monitor::MonitorState;
use hyprdisjust::profile::apply::{
    apply_plan_safely_with_controller, plan_apply, ApplyConfirmation, ApplyOutcome, ApplyWait,
    ConfirmationResult, MonitorController,
};
use hyprdisjust::profile::store::Profile;

const DESK: &str = include_str!("fixtures/hyprctl-monitors-desk.json");
const INACTIVE: &str = include_str!("fixtures/hyprctl-monitors-inactive.json");

#[derive(Default)]
struct FakeController {
    batches: Vec<String>,
    fail_calls: HashSet<usize>,
    monitor_results: VecDeque<anyhow::Result<Vec<MonitorState>>>,
}

impl MonitorController for FakeController {
    fn apply_monitor_batch(&mut self, batch: &str) -> anyhow::Result<()> {
        let call = self.batches.len();
        self.batches.push(batch.to_owned());
        if self.fail_calls.contains(&call) {
            Err(anyhow!("synthetic batch failure {call}"))
        } else {
            Ok(())
        }
    }

    fn monitors_all(&mut self) -> anyhow::Result<Vec<MonitorState>> {
        self.monitor_results
            .pop_front()
            .unwrap_or_else(|| Err(anyhow!("no scripted monitor result")))
    }
}

#[derive(Default)]
struct FakeWait {
    waits: Vec<Duration>,
}

impl ApplyWait for FakeWait {
    fn wait(&mut self, duration: Duration) {
        self.waits.push(duration);
    }
}

struct FakeConfirmation {
    result: anyhow::Result<ConfirmationResult>,
    prepared: bool,
    requested_timeout: Option<Duration>,
}

impl FakeConfirmation {
    fn result(result: ConfirmationResult) -> Self {
        Self {
            result: Ok(result),
            prepared: false,
            requested_timeout: None,
        }
    }

    fn read_failure() -> Self {
        Self {
            result: Err(anyhow!("synthetic input failure")),
            prepared: false,
            requested_timeout: None,
        }
    }
}

impl ApplyConfirmation for FakeConfirmation {
    fn prepare(&mut self) -> anyhow::Result<()> {
        self.prepared = true;
        Ok(())
    }

    fn confirm(
        &mut self,
        _profile_name: &str,
        timeout: Duration,
    ) -> anyhow::Result<ConfirmationResult> {
        self.requested_timeout = Some(timeout);
        std::mem::replace(&mut self.result, Ok(ConfirmationResult::EndOfInput))
    }
}

fn changed_plan() -> hyprdisjust::profile::apply::ApplyPlan {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut profile = Profile::from_monitors(
        "desk".to_owned(),
        &monitors,
        "created".to_owned(),
        "updated".to_owned(),
    );
    profile.outputs[0].x += 20;
    plan_apply(&profile, &monitors).unwrap()
}

fn enabled_plan_from_inactive() -> hyprdisjust::profile::apply::ApplyPlan {
    let monitors = parse_monitors_output(INACTIVE).unwrap();
    let mut profile = Profile::from_monitors(
        "external".to_owned(),
        &monitors,
        "created".to_owned(),
        "updated".to_owned(),
    );
    profile.outputs[0].enabled = true;
    plan_apply(&profile, &monitors).unwrap()
}

fn changed_states() -> (Vec<MonitorState>, Vec<MonitorState>) {
    let previous = parse_monitors_output(DESK).unwrap();
    let mut applied = previous.clone();
    applied[0].x += 20;
    (previous, applied)
}

fn run_with_confirmation(
    result: ConfirmationResult,
) -> (ApplyOutcome, FakeController, FakeConfirmation) {
    let plan = changed_plan();
    let (previous, applied) = changed_states();
    let mut monitor_results = VecDeque::from([Ok(applied.clone())]);
    if result == ConfirmationResult::Confirmed {
        monitor_results.push_back(Ok(applied));
    } else {
        monitor_results.push_back(Ok(previous));
    }
    let mut controller = FakeController {
        monitor_results,
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(result);
    let outcome = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
    )
    .unwrap();
    (outcome, controller, confirmation)
}

#[test]
fn topology_drift_during_confirmation_restores_captured_layout() {
    let plan = changed_plan();
    let (previous, applied) = changed_states();
    let mut drifted = applied.clone();
    drifted[0].x += 100;
    let mut monitor_results = VecDeque::from([Ok(applied)]);
    for _ in 0..9 {
        monitor_results.push_back(Ok(drifted.clone()));
    }
    monitor_results.push_back(Ok(previous));
    let mut controller = FakeController {
        monitor_results,
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(ConfirmationResult::Confirmed);

    let outcome = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
    )
    .unwrap();

    assert!(matches!(outcome, ApplyOutcome::RolledBack { .. }));
    assert_eq!(controller.batches.len(), 2);
}

#[test]
fn already_disabled_unmatched_output_remains_part_of_expected_topology() {
    let mut current = parse_monitors_output(DESK).unwrap();
    current.extend(parse_monitors_output(INACTIVE).unwrap());
    let mut profile = Profile::from_monitors(
        "desk".to_owned(),
        &current[..2],
        "created".to_owned(),
        "updated".to_owned(),
    );
    profile.outputs[0].x += 20;
    let plan = plan_apply(&profile, &current).unwrap();
    let mut applied = current.clone();
    applied[0].x += 20;
    let mut controller = FakeController {
        monitor_results: VecDeque::from([Ok(applied)]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();

    let outcome =
        apply_plan_safely_with_controller(&plan, &mut controller, &mut wait, None).unwrap();

    assert_eq!(outcome, ApplyOutcome::Unattended);
    assert_eq!(controller.batches.len(), 1);
}

#[test]
fn confirmation_within_fifteen_seconds_keeps_layout() {
    let (outcome, controller, confirmation) = run_with_confirmation(ConfirmationResult::Confirmed);

    assert_eq!(outcome, ApplyOutcome::Confirmed);
    assert!(confirmation.prepared);
    assert_eq!(
        confirmation.requested_timeout,
        Some(Duration::from_secs(15))
    );
    assert_eq!(controller.batches.len(), 1);
}

#[test]
fn timeout_rejection_and_eof_restore_captured_layout() {
    for result in [
        ConfirmationResult::TimedOut,
        ConfirmationResult::Rejected,
        ConfirmationResult::EndOfInput,
    ] {
        let (outcome, controller, _) = run_with_confirmation(result);
        assert!(matches!(outcome, ApplyOutcome::RolledBack { .. }));
        assert_eq!(controller.batches.len(), 2);
        assert_ne!(controller.batches[0], controller.batches[1]);
    }
}

#[test]
fn confirmation_read_failure_rolls_back_and_returns_actionable_error() {
    let plan = changed_plan();
    let (previous, applied) = changed_states();
    let mut controller = FakeController {
        monitor_results: VecDeque::from([Ok(applied), Ok(previous)]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::read_failure();

    let error = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
    )
    .unwrap_err();

    assert_eq!(controller.batches.len(), 2);
    let error = format!("{error:#}");
    assert!(error.contains("confirmation failed"));
    assert!(error.contains("previous monitor layout was restored"));
    assert!(error.contains("synthetic input failure"));
}

#[test]
fn failed_confirmation_rollback_reports_both_causes() {
    let plan = changed_plan();
    let (_, applied) = changed_states();
    let mut controller = FakeController {
        fail_calls: HashSet::from([1]),
        monitor_results: VecDeque::from([Ok(applied)]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(ConfirmationResult::TimedOut);

    let error = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
    )
    .unwrap_err();

    let error = format!("{error:#}");
    assert!(error.contains("confirmation timed out"));
    assert!(error.contains("rollback command was rejected"));
    assert!(error.contains("synthetic batch failure 1"));
}

#[test]
fn partial_apply_failure_attempts_recovery() {
    let plan = changed_plan();
    let (previous, _) = changed_states();
    let mut controller = FakeController {
        fail_calls: HashSet::from([0]),
        monitor_results: VecDeque::from([Ok(previous)]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();

    let error =
        apply_plan_safely_with_controller(&plan, &mut controller, &mut wait, None).unwrap_err();

    assert_eq!(controller.batches.len(), 2);
    let error = format!("{error:#}");
    assert!(error.contains("failed to apply profile `desk`"));
    assert!(error.contains("previous monitor layout was restored"));
    assert!(error.contains("synthetic batch failure 0"));
}

#[test]
fn rollback_success_is_verified_against_the_exact_captured_state() {
    let previous = parse_monitors_output(DESK).unwrap();
    let mut applied = previous.clone();
    applied[0].x += 20;
    let plan = changed_plan();
    let mut controller = FakeController {
        monitor_results: VecDeque::from([Ok(applied), Ok(previous)]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(ConfirmationResult::Rejected);

    let outcome = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
    )
    .unwrap();

    assert!(matches!(outcome, ApplyOutcome::RolledBack { .. }));
    assert_eq!(controller.batches.len(), 2);
}

#[test]
fn rollback_verification_failure_is_reported() {
    let previous = parse_monitors_output(DESK).unwrap();
    let mut applied = previous.clone();
    applied[0].x += 20;
    let plan = changed_plan();
    let mut controller = FakeController {
        monitor_results: VecDeque::from([
            Ok(applied.clone()),
            Ok(applied.clone()),
            Ok(applied.clone()),
            Ok(applied),
        ]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(ConfirmationResult::TimedOut);

    let error = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
    )
    .unwrap_err();

    let error = format!("{error:#}");
    assert!(error.contains("rollback monitor state did not converge"));
    assert!(error.contains("confirmation timed out"));
}

#[test]
fn rollback_reconfigures_previously_disabled_output_before_disabling_it() {
    let plan = enabled_plan_from_inactive();
    let previous = parse_monitors_output(INACTIVE).unwrap();
    let mut applied = previous.clone();
    applied[0].enabled = true;
    let mut controller = FakeController {
        monitor_results: VecDeque::from([Ok(applied), Ok(previous)]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(ConfirmationResult::Rejected);

    let outcome = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
    )
    .unwrap();

    assert!(matches!(outcome, ApplyOutcome::RolledBack { .. }));
    assert_eq!(controller.batches.len(), 2);
    assert_eq!(
        controller.batches[1]
            .matches("output = \"HDMI-A-1\"")
            .count(),
        2,
        "rollback should restore settings and then disable the output"
    );
}

#[test]
fn rollback_verifies_details_of_previously_disabled_output() {
    let previous = parse_monitors_output(INACTIVE).unwrap();
    let mut applied = previous.clone();
    applied[0].enabled = true;
    let mut incorrectly_restored = previous.clone();
    incorrectly_restored[0].x += 100;
    let plan = enabled_plan_from_inactive();
    let mut controller = FakeController {
        monitor_results: VecDeque::from([
            Ok(applied),
            Ok(incorrectly_restored.clone()),
            Ok(incorrectly_restored.clone()),
            Ok(incorrectly_restored),
        ]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(ConfirmationResult::TimedOut);

    let error = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
    )
    .unwrap_err();

    let error = format!("{error:#}");
    assert!(error.contains("rollback monitor state did not converge"));
    assert!(error.contains("position did not converge"));
}

#[test]
fn delayed_convergence_beyond_the_old_three_attempt_window_succeeds() {
    let previous = parse_monitors_output(DESK).unwrap();
    let mut applied = previous.clone();
    applied[0].x += 20;
    let plan = changed_plan();
    let mut results = VecDeque::new();
    for _ in 0..4 {
        results.push_back(Ok(previous.clone()));
    }
    results.push_back(Ok(applied));
    let mut controller = FakeController {
        monitor_results: results,
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();

    apply_plan_safely_with_controller(&plan, &mut controller, &mut wait, None).unwrap();

    assert_eq!(controller.batches.len(), 1);
    assert_eq!(wait.waits.len(), 4);
}

#[test]
fn an_output_added_during_apply_is_reported_as_topology_drift() {
    let previous = parse_monitors_output(DESK).unwrap();
    let mut applied = previous.clone();
    applied[0].x += 20;
    let mut added = applied.clone();
    let mut unexpected = applied[0].clone();
    unexpected.output_name = "DP-99".to_owned();
    unexpected.id = "output:dp-99".to_owned();
    added.push(unexpected);
    let plan = changed_plan();
    let mut results = VecDeque::new();
    for _ in 0..9 {
        results.push_back(Ok(added.clone()));
    }
    results.push_back(Ok(previous));
    let mut controller = FakeController {
        monitor_results: results,
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();

    let error =
        apply_plan_safely_with_controller(&plan, &mut controller, &mut wait, None).unwrap_err();

    let error = format!("{error:#}");
    assert!(error.contains("topology drifted"));
    assert!(error.contains("DP-99"));
    assert!(error.contains("previous monitor layout was restored"));
}
