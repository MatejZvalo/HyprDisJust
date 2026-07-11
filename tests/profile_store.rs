use std::fs;

use hyprdisjust::config::ConfigPaths;
use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::profile::store::ProfileStore;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

const DESK: &str = include_str!("fixtures/hyprctl-monitors-desk.json");
const INACTIVE: &str = include_str!("fixtures/hyprctl-monitors-inactive.json");

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

#[test]
fn config_paths_use_config_dir_override() {
    let paths = ConfigPaths::from_config_dir("/tmp/hyprdisjust-test").unwrap();

    assert_eq!(paths.config_dir.to_string_lossy(), "/tmp/hyprdisjust-test");
    assert_eq!(
        paths.profile_store.to_string_lossy(),
        "/tmp/hyprdisjust-test/profiles.toml"
    );
}

#[test]
fn config_paths_keep_generated_files_under_config_dir() {
    let paths = ConfigPaths::from_config_dir("/tmp/hyprdisjust-test").unwrap();

    assert_eq!(
        paths.generated_dir_path().to_string_lossy(),
        "/tmp/hyprdisjust-test/generated"
    );
    assert_eq!(
        paths.generated_monitors_lua_path().to_string_lossy(),
        "/tmp/hyprdisjust-test/generated/monitors.lua"
    );
}

#[test]
fn profile_store_roundtrips_as_toml() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("nested").join("profiles.toml");
    let monitors = parse_monitors_output(DESK).unwrap();

    let mut store = ProfileStore::default();
    let name = store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    assert_eq!(name, "desk");
    store.save_atomic(&path).unwrap();

    let loaded = ProfileStore::load(&path).unwrap();
    assert_eq!(loaded.profiles.len(), 1);
    assert_eq!(loaded.profiles[0].name, "desk");
    assert_eq!(loaded.profiles[0].monitors.len(), 2);
    assert_eq!(loaded.profiles[0].outputs[0].mode, "2560x1440@144");
}

#[test]
fn inactive_zero_geometry_is_saved_with_a_safe_mode() {
    let monitors = parse_monitors_output(INACTIVE).unwrap();
    let profile = hyprdisjust::profile::store::Profile::from_monitors(
        "inactive".to_owned(),
        &monitors,
        "created".to_owned(),
        "updated".to_owned(),
    );

    assert!(!profile.outputs[0].enabled);
    assert_ne!(profile.outputs[0].mode, "0x0@0");
    assert!(profile.outputs[0].mode == "preferred" || profile.outputs[0].mode.contains('x'));
}

#[test]
fn missing_profile_store_loads_empty() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("profiles.toml");

    let store = ProfileStore::load(path).unwrap();

    assert!(store.profiles.is_empty());
}

#[test]
fn malformed_profile_store_has_context() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("profiles.toml");
    fs::write(&path, "not valid = [").unwrap();

    let error = format!("{:#}", ProfileStore::load(&path).unwrap_err());

    assert!(error.contains("failed to parse profile store"));
}

#[test]
fn unsupported_profile_fields_are_rejected_instead_of_dropped() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("profiles.toml");
    fs::write(&path, "unsupported = true\n").unwrap();

    let error = format!("{:#}", ProfileStore::load(&path).unwrap_err());

    assert!(error.contains("failed to parse profile store"));
    assert!(error.contains("unknown field"));
}

#[test]
fn duplicate_profile_names_are_rejected_on_load() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("profiles.toml");
    fs::write(
        &path,
        "[[profiles]]\nname = \"desk\"\ncreated_at = \"a\"\nupdated_at = \"a\"\n\n[[profiles]]\nname = \"desk\"\ncreated_at = \"b\"\nupdated_at = \"b\"\n",
    )
    .unwrap();

    assert!(
        format!("{:#}", ProfileStore::load(&path).unwrap_err()).contains("duplicate profile name")
    );
}

#[test]
fn generated_profile_names_avoid_collisions() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();

    let first = store.save_current_profile(None, &monitors, false).unwrap();
    let second = store.save_current_profile(None, &monitors, false).unwrap();

    assert_eq!(first, "desk-2-monitors");
    assert_eq!(second, "desk-2-monitors-2");
}

#[test]
fn saved_profile_outputs_keep_duplicate_no_serial_monitors_distinct() {
    let monitors = parse_monitors_output(DUPLICATE_NO_SERIAL).unwrap();
    let profile = hyprdisjust::profile::store::Profile::from_monitors(
        "twins".to_owned(),
        &monitors,
        "created".to_owned(),
        "updated".to_owned(),
    );

    assert_eq!(
        profile.monitors[0].id,
        "acme:samepanel:no-serial:output:dp-1"
    );
    assert_eq!(
        profile.monitors[1].id,
        "acme:samepanel:no-serial:output:dp-2"
    );
    assert_eq!(
        profile.outputs[0].monitor_id,
        "acme:samepanel:no-serial:output:dp-1"
    );
    assert_eq!(
        profile.outputs[1].monitor_id,
        "acme:samepanel:no-serial:output:dp-2"
    );
}

#[test]
fn profile_store_renames_deletes_and_copies_profiles() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();

    store.rename_profile("desk", "work").unwrap();
    assert!(store.has_profile("work"));
    assert!(!store.has_profile("desk"));

    store.copy_profile("work", "backup", false).unwrap();
    assert!(store.has_profile("backup"));
    assert_eq!(store.profiles.len(), 2);

    let deleted = store.delete_profile("backup").unwrap();
    assert_eq!(deleted.name, "backup");
    assert_eq!(store.profiles.len(), 1);
}

#[test]
fn profile_store_lifecycle_refuses_ambiguous_mutations() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    store
        .save_current_profile(Some("laptop"), &monitors, false)
        .unwrap();

    assert!(store.rename_profile("desk", "laptop").is_err());
    assert!(store.rename_profile("missing", "other").is_err());
    assert!(store.copy_profile("missing", "other", false).is_err());
    assert!(store.copy_profile("desk", "laptop", false).is_err());
    assert!(store.delete_profile("missing").is_err());
}
