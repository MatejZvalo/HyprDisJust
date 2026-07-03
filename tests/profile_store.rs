use std::fs;

use hyprdisjust::config::ConfigPaths;
use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::profile::store::ProfileStore;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

const DESK: &str = include_str!("fixtures/hyprctl-monitors-desk.json");

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
        paths.generated_monitors_conf_path().to_string_lossy(),
        "/tmp/hyprdisjust-test/generated/monitors.conf"
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

    let error = ProfileStore::load(&path).unwrap_err().to_string();

    assert!(error.contains("failed to parse profile store"));
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
