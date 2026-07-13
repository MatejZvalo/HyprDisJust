use std::fs;
use std::path::PathBuf;
use std::process::Command;

use hyprdisjust::cli::format_auto_apply_dry_run;
use hyprdisjust::config::AppConfig;
use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::hyprland::monitor::MonitorState;
use hyprdisjust::profile::r#match::{
    best_profile_match, decide_auto_apply, match_profile, profile_monitor_match_score,
    AutoApplyDecision, MatchConfidence,
};
use hyprdisjust::profile::store::{Profile, ProfileMonitor, ProfileStore};
use pretty_assertions::assert_eq;
use tempfile::tempdir;

const DESK: &str = include_str!("fixtures/hyprctl-monitors-desk.json");
const DOCK_RENAMED: &str = include_str!("fixtures/hyprctl-monitors-dock-renamed.json");

const DUPLICATE_NO_SERIAL: &str = r#"[
  {
    "id": 0,
    "name": "DP-1",
    "description": "Acme SamePanel",
    "make": "Acme",
    "model": "SamePanel",
    "serial": "",
    "width": 1920,
    "height": 1080,
    "refreshRate": 60.0,
    "x": 0,
    "y": 0,
    "scale": 1.0,
    "transform": 0
  },
  {
    "id": 1,
    "name": "DP-2",
    "description": "Acme SamePanel",
    "make": "Acme",
    "model": "SamePanel",
    "serial": "",
    "width": 1920,
    "height": 1080,
    "refreshRate": 60.0,
    "x": 1920,
    "y": 0,
    "scale": 1.0,
    "transform": 0
  }
]"#;

const DUPLICATE_NO_SERIAL_RENAMED: &str = r#"[
  {
    "id": 0,
    "name": "HDMI-A-1",
    "description": "Acme SamePanel",
    "make": "Acme",
    "model": "SamePanel",
    "serial": "",
    "width": 1920,
    "height": 1080,
    "refreshRate": 60.0,
    "x": 0,
    "y": 0,
    "scale": 1.0,
    "transform": 0
  },
  {
    "id": 1,
    "name": "HDMI-A-2",
    "description": "Acme SamePanel",
    "make": "Acme",
    "model": "SamePanel",
    "serial": "",
    "width": 1920,
    "height": 1080,
    "refreshRate": 60.0,
    "x": 1920,
    "y": 0,
    "scale": 1.0,
    "transform": 0
  }
]"#;

const SAME_MODEL_DIFFERENT_SERIALS: &str = r#"[
  {
    "id": 0,
    "name": "DP-1",
    "description": "Acme WorkView AAA",
    "make": "Acme",
    "model": "WorkView",
    "serial": "AAA",
    "width": 1920,
    "height": 1080,
    "refreshRate": 60.0,
    "x": 0,
    "y": 0,
    "scale": 1.0,
    "transform": 0
  },
  {
    "id": 1,
    "name": "DP-2",
    "description": "Acme WorkView BBB",
    "make": "Acme",
    "model": "WorkView",
    "serial": "BBB",
    "width": 1920,
    "height": 1080,
    "refreshRate": 60.0,
    "x": 1920,
    "y": 0,
    "scale": 1.0,
    "transform": 0
  }
]"#;

fn hyprdisjust() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hyprdisjust"))
}

fn desk_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hyprctl-monitors-desk.json")
}

#[test]
fn exact_match_from_saved_fixture() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let profile = profile_from_monitors("desk", &monitors);

    let profile_match = match_profile(&profile, &monitors);

    assert_eq!(profile_match.confidence, MatchConfidence::Exact);
    assert_eq!(profile_match.score, 200);
    assert_eq!(profile_match.matched_monitors, 2);
}

#[test]
fn dock_renamed_outputs_match_by_physical_identity() {
    let saved = parse_monitors_output(DESK).unwrap();
    let current = parse_monitors_output(DOCK_RENAMED).unwrap();
    let profile = profile_from_monitors("desk", &saved);

    let profile_match = match_profile(&profile, &current);

    assert_eq!(profile_match.confidence, MatchConfidence::Exact);
    assert_eq!(profile_match.matched_monitors, 2);
    assert!(profile_match
        .reasons
        .iter()
        .any(|reason| reason.contains("HDMI-A-1")));
}

#[test]
fn same_model_with_different_serials_remains_distinct() {
    let monitors = parse_monitors_output(SAME_MODEL_DIFFERENT_SERIALS).unwrap();
    let profile = profile_from_monitors("twins", &monitors);

    let profile_match = match_profile(&profile, &monitors);

    assert_eq!(profile_match.confidence, MatchConfidence::Exact);
    assert_eq!(profile_match.score, 200);
    assert_eq!(
        profile.monitors[0].id, "acme:workview:aaa",
        "first serial should have its own identity"
    );
    assert_eq!(
        profile.monitors[1].id, "acme:workview:bbb",
        "second serial should have its own identity"
    );
}

#[test]
fn duplicate_no_serial_monitors_do_not_collapse_when_outputs_are_renamed() {
    let saved = parse_monitors_output(DUPLICATE_NO_SERIAL).unwrap();
    let current = parse_monitors_output(DUPLICATE_NO_SERIAL_RENAMED).unwrap();
    let profile = profile_from_monitors("twins", &saved);

    let profile_match = match_profile(&profile, &current);

    assert_eq!(profile_match.confidence, MatchConfidence::Ambiguous);
    assert_eq!(profile_match.matched_monitors, 2);
}

#[test]
fn output_name_only_match_is_partial() {
    let current = parse_monitors_output(DESK).unwrap();
    let profile = Profile {
        name: "weak".to_owned(),
        created_at: "created".to_owned(),
        updated_at: "updated".to_owned(),
        monitors: vec![ProfileMonitor {
            id: "unknown-physical-monitor".to_owned(),
            name_hint: "DP-1".to_owned(),
            description: "Different display".to_owned(),
            make: "Different".to_owned(),
            model: "Display".to_owned(),
            serial: "different".to_owned(),
            physical_width: 0,
            physical_height: 0,
        }],
        outputs: vec![],
    };

    let profile_match = match_profile(&profile, &current);

    assert_eq!(profile_match.confidence, MatchConfidence::Partial);
    assert_eq!(profile_match.score, 20);
}

#[test]
fn exact_description_is_the_explicit_high_confidence_threshold() {
    let current = parse_monitors_output(DESK).unwrap();
    let mut profile = profile_from_monitors("desk", &current);
    for monitor in &mut profile.monitors {
        monitor.id = format!("stale:{}", monitor.id);
        monitor.serial = "stale".to_owned();
    }

    let profile_match = match_profile(&profile, &current);

    assert_eq!(profile_match.confidence, MatchConfidence::High);
    assert_eq!(profile_match.score, 120);
}

#[test]
fn physical_dimensions_survive_as_a_matching_fallback() {
    let current = parse_monitors_output(DESK).unwrap().remove(0);
    let mut saved = ProfileMonitor::from(&current);
    saved.id = "stale-id".to_owned();
    saved.serial = "stale-serial".to_owned();
    saved.description = "stale description".to_owned();

    let (score, reason) = profile_monitor_match_score(&saved, &current);

    assert_eq!(score, 50);
    assert_eq!(reason, "make/model/physical size");
}

#[test]
fn monitor_ambiguity_blocks_configured_fallback() {
    let saved = parse_monitors_output(DUPLICATE_NO_SERIAL).unwrap();
    let current = parse_monitors_output(DUPLICATE_NO_SERIAL_RENAMED).unwrap();
    let mut store = ProfileStore::default();
    store.profiles.push(profile_from_monitors("twins", &saved));
    store
        .profiles
        .push(profile_from_monitors("fallback", &[unknown_monitor()]));

    let best_match = best_profile_match(&store, &current);
    let decision = decide_auto_apply(&store, &best_match, Some("fallback"));

    assert!(matches!(decision, AutoApplyDecision::Ambiguous { .. }));
}

#[test]
fn global_mapping_preserves_an_exact_match_instead_of_greedily_consuming_it() {
    let mut first = unknown_monitor();
    first.output_name = "Virtual-1".to_owned();
    first.id = "virtual:shared:one".to_owned();
    first.description = "Shared panel".to_owned();
    let mut second = first.clone();
    second.output_name = "Virtual-2".to_owned();
    second.id = "virtual:shared:two".to_owned();
    let current = vec![first.clone(), second];
    let profile = Profile {
        name: "global".to_owned(),
        created_at: "created".to_owned(),
        updated_at: "updated".to_owned(),
        monitors: vec![
            ProfileMonitor {
                id: "stale".to_owned(),
                name_hint: String::new(),
                description: "Shared panel".to_owned(),
                make: String::new(),
                model: String::new(),
                serial: String::new(),
                physical_width: 0,
                physical_height: 0,
            },
            ProfileMonitor::from(&first),
        ],
        outputs: vec![],
    };

    let profile_match = match_profile(&profile, &current);

    assert_eq!(profile_match.confidence, MatchConfidence::High);
    assert!(profile_match
        .reasons
        .iter()
        .any(|reason| reason.contains("matched Virtual-2 by exact description")));
    assert!(profile_match
        .reasons
        .iter()
        .any(|reason| reason.contains("matched Virtual-1 by exact monitor id")));
}

#[test]
fn best_profile_match_reports_ambiguous_exact_tie() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .profiles
        .push(profile_from_monitors("desk-a", &monitors));
    store
        .profiles
        .push(profile_from_monitors("desk-b", &monitors));

    let best_match = best_profile_match(&store, &monitors);

    assert!(best_match.ambiguous);
    assert!(best_match.selected.is_none());
}

#[test]
fn fallback_profile_is_used_only_without_high_confidence_match() {
    let desk = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store.profiles.push(profile_from_monitors("desk", &desk));
    store
        .profiles
        .push(profile_from_monitors("fallback", &[unknown_monitor()]));

    let exact = best_profile_match(&store, &desk);
    let exact_output = format_auto_apply_dry_run(&exact, &store, Some("fallback"));
    assert!(exact_output.contains("Would select profile: desk"));
    assert!(exact_output.contains("Confidence: exact"));

    let no_match_current = vec![other_unknown_monitor()];
    let no_match = best_profile_match(&store, &no_match_current);
    let fallback_output = format_auto_apply_dry_run(&no_match, &store, Some("fallback"));
    assert!(fallback_output.contains("Would select profile: fallback"));
    assert!(fallback_output.contains("Confidence: fallback"));
}

#[test]
fn auto_apply_decision_selects_exact_match() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .profiles
        .push(profile_from_monitors("desk", &monitors));

    let best_match = best_profile_match(&store, &monitors);
    let decision = decide_auto_apply(&store, &best_match, Some("fallback"));

    assert_eq!(
        decision,
        AutoApplyDecision::Apply {
            profile_name: "desk".to_owned(),
            confidence: "exact".to_owned(),
            reason: "2/2 monitor identities matched exactly".to_owned(),
        }
    );
}

#[test]
fn auto_apply_decision_uses_fallback_without_high_confidence_match() {
    let desk = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store.profiles.push(profile_from_monitors("desk", &desk));
    store
        .profiles
        .push(profile_from_monitors("fallback", &[unknown_monitor()]));

    let best_match = best_profile_match(&store, &[other_unknown_monitor()]);
    let decision = decide_auto_apply(&store, &best_match, Some("fallback"));

    assert_eq!(
        decision,
        AutoApplyDecision::Apply {
            profile_name: "fallback".to_owned(),
            confidence: "fallback".to_owned(),
            reason: "no exact or high-confidence match; fallback_profile is configured".to_owned(),
        }
    );
}

#[test]
fn auto_apply_decision_refuses_ambiguous_exact_match() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .profiles
        .push(profile_from_monitors("desk-a", &monitors));
    store
        .profiles
        .push(profile_from_monitors("desk-b", &monitors));

    let best_match = best_profile_match(&store, &monitors);
    let decision = decide_auto_apply(&store, &best_match, Some("fallback"));

    assert_eq!(
        decision,
        AutoApplyDecision::Ambiguous {
            reason: "2/2 monitor identities matched exactly".to_owned(),
        }
    );
}

#[test]
fn auto_apply_decision_reports_missing_fallback() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .profiles
        .push(profile_from_monitors("desk", &monitors));

    let best_match = best_profile_match(&store, &[unknown_monitor()]);
    let decision = decide_auto_apply(&store, &best_match, Some("missing"));

    assert_eq!(
        decision,
        AutoApplyDecision::MissingFallback {
            profile_name: "missing".to_owned(),
        }
    );
}

#[test]
fn fallback_profile_is_used_when_best_candidate_is_not_auto_eligible() {
    let desk = parse_monitors_output(DESK).unwrap();
    let laptop =
        parse_monitors_output(include_str!("fixtures/hyprctl-monitors-laptop.json")).unwrap();
    let mut store = ProfileStore::default();
    store.profiles.push(profile_from_monitors("desk", &desk));

    let best_match = best_profile_match(&store, &laptop);
    let output = format_auto_apply_dry_run(&best_match, &store, Some("desk"));

    assert!(output.contains("Would select profile: desk"));
    assert!(output.contains("Confidence: fallback"));
}

#[test]
fn missing_fallback_profile_is_reported() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .profiles
        .push(profile_from_monitors("desk", &monitors));

    let best_match = best_profile_match(&store, &[unknown_monitor()]);
    let output = format_auto_apply_dry_run(&best_match, &store, Some("missing"));

    assert!(output.contains("fallback_profile `missing` does not exist"));
}

#[test]
fn app_config_loads_optional_fallback_profile() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "fallback_profile = \"laptop\"\n").unwrap();

    let config = AppConfig::load(&config_path).unwrap();

    assert_eq!(config.fallback_profile.as_deref(), Some("laptop"));
    assert_eq!(config.debounce_ms, 900);
    assert!(!config.apply_on_start);
    assert_eq!(config.tui_move_step, 20);
}

#[test]
fn app_config_loads_daemon_fields() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    fs::write(
        &config_path,
        "fallback_profile = \"laptop\"\ndebounce_ms = 1250\napply_on_start = true\ntui_move_step = 80\n",
    )
    .unwrap();

    let config = AppConfig::load(&config_path).unwrap();

    assert_eq!(config.fallback_profile.as_deref(), Some("laptop"));
    assert_eq!(config.debounce_ms, 1250);
    assert!(config.apply_on_start);
    assert_eq!(config.tui_move_step, 80);
}

#[test]
fn missing_app_config_loads_defaults() {
    let temp = tempdir().unwrap();
    let config = AppConfig::load(temp.path().join("config.toml")).unwrap();

    assert_eq!(config, AppConfig::default());
    assert_eq!(config.debounce_ms, 900);
    assert!(!config.apply_on_start);
    assert_eq!(config.tui_move_step, 20);
}

#[test]
fn malformed_app_config_has_context() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "fallback_profile = [").unwrap();

    let error = format!("{:#}", AppConfig::load(&config_path).unwrap_err());

    assert!(error.contains("failed to parse config"));
    assert!(error.contains("config.toml"));
}

#[test]
fn unknown_app_config_fields_are_rejected() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "debouce_ms = 100\n").unwrap();

    let error = format!("{:#}", AppConfig::load(&config_path).unwrap_err());

    assert!(error.contains("failed to parse config"));
    assert!(error.contains("unknown field"));
}

#[test]
fn invalid_tui_move_step_is_rejected() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");
    fs::write(&config_path, "tui_move_step = 0\n").unwrap();

    assert!(AppConfig::load(&config_path)
        .unwrap_err()
        .to_string()
        .contains("tui_move_step must be between"));
}

#[test]
fn doctor_prints_best_profile_summary_from_fixture() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path().join("profiles.toml"));

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture_path())
        .arg("doctor")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Best profile: desk"));
    assert!(stdout.contains("Confidence: exact"));
}

#[test]
fn apply_auto_dry_run_explains_selected_profile() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path().join("profiles.toml"));

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture_path())
        .args(["apply", "--auto", "--dry-run"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Auto-apply dry run"));
    assert!(stdout.contains("Would select profile: desk"));
    assert!(stdout.contains("Confidence: exact"));
    assert!(stdout.contains(
        "hyprctl --batch \"eval hl.monitor({ output = \\\"DP-1\\\", disabled = false, mode = \\\"2560x1440@144\\\", position = \\\"0x0\\\", scale = 1, transform = 0 }) ; eval hl.monitor({ output = \\\"eDP-1\\\", disabled = false, mode = \\\"1920x1200@60\\\", position = \\\"2560x240\\\", scale = 1, transform = 0 })\""
    ));
}

#[test]
fn apply_named_dry_run_prints_exact_hyprctl_batch() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path().join("profiles.toml"));

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture_path())
        .args(["apply", "desk", "--dry-run"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Profile: desk"));
    assert!(stdout.contains(
        "hyprctl --batch \"eval hl.monitor({ output = \\\"DP-1\\\", disabled = false, mode = \\\"2560x1440@144\\\", position = \\\"0x0\\\", scale = 1, transform = 0 }) ; eval hl.monitor({ output = \\\"eDP-1\\\", disabled = false, mode = \\\"1920x1200@60\\\", position = \\\"2560x240\\\", scale = 1, transform = 0 })\""
    ));
}

fn profile_from_monitors(name: &str, monitors: &[MonitorState]) -> Profile {
    Profile::from_monitors(
        name.to_owned(),
        monitors,
        "created".to_owned(),
        "updated".to_owned(),
    )
}

fn save_desk_profile(path: PathBuf) {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    store.save_atomic(path).unwrap();
}

fn unknown_monitor() -> MonitorState {
    monitor(
        "Virtual-1",
        "virtual:fallback:one",
        "Fallback",
        "Virtual",
        "Fallback",
    )
}

fn other_unknown_monitor() -> MonitorState {
    monitor(
        "Virtual-2",
        "virtual:other:two",
        "Other",
        "Virtual",
        "Other",
    )
}

fn monitor(
    output_name: &str,
    id: &str,
    description: &str,
    make: &str,
    model: &str,
) -> MonitorState {
    MonitorState {
        output_name: output_name.to_owned(),
        id: id.to_owned(),
        description: description.to_owned(),
        make: make.to_owned(),
        model: model.to_owned(),
        serial: String::new(),
        enabled: true,
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
        refresh_rate: 60.0,
        scale: 1.0,
        transform: 0,
        available_modes: vec![],
        physical_width: 0,
        physical_height: 0,
    }
}
