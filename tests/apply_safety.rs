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

fn run_with_confirmation(
    result: ConfirmationResult,
) -> (ApplyOutcome, FakeController, FakeConfirmation) {
    let plan = changed_plan();
    let mut controller = FakeController::default();
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(result);
    let outcome = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
        true,
    )
    .unwrap();
    (outcome, controller, confirmation)
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
    let mut controller = FakeController::default();
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::read_failure();

    let error = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
        true,
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
    let mut controller = FakeController {
        fail_calls: HashSet::from([1]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(ConfirmationResult::TimedOut);

    let error = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
        true,
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
    let mut controller = FakeController {
        fail_calls: HashSet::from([0]),
        ..FakeController::default()
    };
    let mut wait = FakeWait::default();

    let error = apply_plan_safely_with_controller(&plan, &mut controller, &mut wait, None, true)
        .unwrap_err();

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
        false,
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
        false,
    )
    .unwrap_err();

    let error = format!("{error:#}");
    assert!(error.contains("rollback monitor state did not converge"));
    assert!(error.contains("confirmation timed out"));
}

#[test]
fn rollback_reconfigures_previously_disabled_output_before_disabling_it() {
    let plan = enabled_plan_from_inactive();
    let mut controller = FakeController::default();
    let mut wait = FakeWait::default();
    let mut confirmation = FakeConfirmation::result(ConfirmationResult::Rejected);

    let outcome = apply_plan_safely_with_controller(
        &plan,
        &mut controller,
        &mut wait,
        Some(&mut confirmation),
        true,
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
        false,
    )
    .unwrap_err();

    let error = format!("{error:#}");
    assert!(error.contains("rollback monitor state did not converge"));
    assert!(error.contains("position did not converge"));
}
