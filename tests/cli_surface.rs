use std::fs;
use std::path::PathBuf;
use std::process::Command;

use pretty_assertions::assert_eq;
use tempfile::tempdir;

fn hyprdisjust() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hyprdisjust"))
}

fn desk_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hyprctl-monitors-desk.json")
}

#[test]
fn help_lists_bootstrap_command_surface() {
    let output = hyprdisjust().arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for command in ["doctor", "list", "save", "apply", "daemon", "export"] {
        assert!(
            stdout.contains(command),
            "expected help output to include `{command}`:\n{stdout}"
        );
    }
}

#[test]
fn daemon_once_dry_run_explains_selected_profile() {
    let config_dir = tempdir().unwrap();

    let save_output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["save", "desk"])
        .output()
        .unwrap();
    assert!(
        save_output.status.success(),
        "{}",
        String::from_utf8_lossy(&save_output.stderr)
    );

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["daemon", "--once", "--dry-run"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Auto-apply decision"));
    assert!(stdout.contains("Selected profile: desk"));
    assert!(stdout.contains("Confidence: exact"));
    assert!(stdout.contains("Dry run: monitor layout was not changed"));
    assert!(stdout.contains(
        "hyprctl --batch \"keyword monitor DP-1,2560x1440@144,0x0,1 ; keyword monitor eDP-1,1920x1200@60,2560x240,1\""
    ));
}

#[test]
fn export_named_profile_writes_generated_conf() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["export", "desk", "--format", "conf"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = config_dir.path().join("generated").join("monitors.conf");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "monitor = DP-1,2560x1440@144,0x0,1\nmonitor = eDP-1,1920x1200@60,2560x240,1"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Exported profile `desk` to "));
    assert!(stdout.contains(&path.to_string_lossy().to_string()));
}

#[test]
fn export_named_profile_writes_generated_lua() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["export", "desk", "--format", "lua"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(config_dir.path().join("generated").join("monitors.lua")).unwrap(),
        "hyprland.keyword(\"monitor\", \"DP-1,2560x1440@144,0x0,1\")\nhyprland.keyword(\"monitor\", \"eDP-1,1920x1200@60,2560x240,1\")"
    );
}

#[test]
fn export_without_name_auto_selects_best_match() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["export", "--format", "conf"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(config_dir.path().join("generated").join("monitors.conf")).unwrap(),
        "monitor = DP-1,2560x1440@144,0x0,1\nmonitor = eDP-1,1920x1200@60,2560x240,1"
    );
}

#[test]
fn export_unknown_profile_returns_clear_error() {
    let config_dir = tempdir().unwrap();

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["export", "missing", "--format", "conf"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("profile `missing` does not exist"));
}

#[test]
fn apply_unknown_profile_returns_clear_error() {
    let config_dir = tempdir().unwrap();

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .args(["apply", "missing", "--dry-run"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("profile `missing` does not exist"));
}

#[test]
fn list_reports_empty_profile_store() {
    let config_dir = tempdir().unwrap();

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .arg("list")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.trim(), "No profiles saved yet.");
}

#[test]
fn save_and_list_profile_from_fixture() {
    let config_dir = tempdir().unwrap();

    let save_output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["save", "desk"])
        .output()
        .unwrap();

    assert!(
        save_output.status.success(),
        "{}",
        String::from_utf8_lossy(&save_output.stderr)
    );
    assert!(config_dir.path().join("profiles.toml").exists());

    let list_output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .arg("list")
        .output()
        .unwrap();

    assert!(list_output.status.success());
    let stdout = String::from_utf8(list_output.stdout).unwrap();
    assert!(stdout.contains("Profiles: 1"));
    assert!(stdout.contains("- desk (2 monitors)"));
}

#[test]
fn save_requires_replace_for_existing_profile() {
    let config_dir = tempdir().unwrap();

    let first = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["save", "desk"])
        .output()
        .unwrap();
    assert!(first.status.success());

    let second = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["save", "desk"])
        .output()
        .unwrap();

    assert_eq!(second.status.code(), Some(1));
    let stderr = String::from_utf8(second.stderr).unwrap();
    assert!(stderr.contains("already exists"));

    let replaced = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["save", "desk", "--replace"])
        .output()
        .unwrap();

    assert!(
        replaced.status.success(),
        "{}",
        String::from_utf8_lossy(&replaced.stderr)
    );
}

fn save_desk_profile(config_dir: &std::path::Path, name: &str) {
    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir)
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["save", name])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
