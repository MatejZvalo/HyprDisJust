use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::profile::store::ProfileStore;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

fn hyprdisjust() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hyprdisjust"))
}

fn desk_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hyprctl-monitors-desk.json")
}

fn dock_renamed_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hyprctl-monitors-dock-renamed.json")
}

#[test]
fn help_lists_bootstrap_command_surface() {
    let output = hyprdisjust().arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for command in [
        "doctor",
        "list",
        "save",
        "rename",
        "delete",
        "copy",
        "apply",
        "daemon",
        "export",
        "tui",
        "install-systemd-user",
        "completions",
    ] {
        assert!(
            stdout.contains(command),
            "expected help output to include `{command}`:\n{stdout}"
        );
    }
}

#[test]
fn completions_prints_shell_completion_script() {
    let output = hyprdisjust()
        .args(["completions", "bash"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("complete -F _hyprdisjust"));
    assert!(stdout.contains("install-systemd-user"));
}

#[test]
fn profile_lifecycle_commands_rename_copy_and_delete() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");

    let renamed = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .args(["rename", "desk", "work"])
        .output()
        .unwrap();
    assert!(
        renamed.status.success(),
        "{}",
        String::from_utf8_lossy(&renamed.stderr)
    );

    let copied = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .args(["copy", "work", "backup"])
        .output()
        .unwrap();
    assert!(
        copied.status.success(),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );

    let deleted = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .args(["delete", "backup", "--yes"])
        .output()
        .unwrap();
    assert!(
        deleted.status.success(),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );

    let list = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .arg("list")
        .output()
        .unwrap();
    let stdout = String::from_utf8(list.stdout).unwrap();
    assert!(stdout.contains("- work (2 monitors)"));
    assert!(!stdout.contains("desk"));
    assert!(!stdout.contains("backup"));
}

#[test]
fn profile_lifecycle_reports_clear_failures() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");

    let duplicate = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .args(["copy", "desk", "desk"])
        .output()
        .unwrap();
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(String::from_utf8(duplicate.stderr)
        .unwrap()
        .contains("already exists"));

    let delete_without_yes = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .args(["delete", "desk"])
        .output()
        .unwrap();
    assert_eq!(delete_without_yes.status.code(), Some(1));
    assert!(String::from_utf8(delete_without_yes.stderr)
        .unwrap()
        .contains("requires --yes"));
}

#[test]
fn doctor_reports_paths_profiles_socket_and_stale_output_names() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", dock_renamed_fixture())
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
        .arg("doctor")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("HyprDisJust doctor"));
    assert!(!stdout.contains("generated conf"));
    assert!(stdout.contains("[info] generated lua:"));
    assert!(stdout.contains("[warning] socket2:"));
    assert!(stdout.contains("[ok] profiles: 1 saved"));
    assert!(stdout.contains("[warning] saved output names:"));
    assert!(stdout.contains("Hyprland: detected"));
    assert!(stdout.contains("Best profile: desk"));
    assert!(stdout.contains("Score: 200"));
    assert!(stdout.contains("systemd user service"));
}

#[test]
fn doctor_reports_empty_store_without_failing() {
    let config_dir = tempdir().unwrap();

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .arg("doctor")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("[warning] profiles: no profiles saved"));
    assert!(stdout.contains("Monitors: 2"));
}

#[test]
fn install_systemd_user_dry_run_prints_service() {
    let systemd_dir = tempdir().unwrap();

    let output = hyprdisjust()
        .env("HYPRDISJUST_SYSTEMD_USER_DIR", systemd_dir.path())
        .args(["install-systemd-user", "--dry-run", "--enable", "--start"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Would write systemd user service"));
    assert!(stdout.contains("ExecStart="));
    assert!(stdout.contains("hyprdisjust"));
    assert!(!stdout.contains("daemon --unattended"));
    assert!(!systemd_dir.path().join("hyprdisjust.service").exists());
}

#[test]
fn install_systemd_user_can_enable_and_start_with_fake_systemctl() {
    let systemd_dir = tempdir().unwrap();
    let fake_bin = fake_systemctl();

    let output = hyprdisjust()
        .env("HYPRDISJUST_SYSTEMD_USER_DIR", systemd_dir.path())
        .env("PATH", fake_bin.path())
        .args([
            "install-systemd-user",
            "--enable",
            "--start",
            "--unattended",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let service = fs::read_to_string(systemd_dir.path().join("hyprdisjust.service")).unwrap();
    assert!(service.contains("Restart=on-failure"));
    assert!(service.contains("daemon --unattended"));
    let calls = fs::read_to_string(fake_bin.path().join("calls")).unwrap();
    assert!(calls.contains("--user enable hyprdisjust.service"));
    assert!(calls.contains("--user start hyprdisjust.service"));
    assert!(calls.contains("--user daemon-reload"));
}

#[test]
fn tui_snapshot_uses_current_monitors_and_profiles() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .arg("tui")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("HyprDisJust TUI"));
    assert!(stdout.contains("Previewing profile `desk`"));
    assert!(stdout.contains("Monitors:"));
    assert!(stdout.contains("DP-1 2560x1440@144"));
    assert!(stdout.contains("* desk (2 monitors)"));
    assert!(stdout.contains("Preview command:"));
}

#[test]
fn tui_snapshot_reports_apply_plan_warnings() {
    let config_dir = tempdir().unwrap();
    save_overlapping_profile(config_dir.path(), "overlap");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .arg("tui")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Previewing profile `overlap` with 1 warning"));
    assert!(stdout.contains("Warnings:"));
    assert!(stdout.contains("overlap"));
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
    assert!(stdout.contains("No changes: selected profile is already active"));
    assert!(stdout.contains(
        "hyprctl --batch \"eval hl.monitor({ output = \\\"DP-1\\\", disabled = false, mode = \\\"2560x1440@144\\\", position = \\\"0x0\\\", scale = 1, transform = 0 }) ; eval hl.monitor({ output = \\\"eDP-1\\\", disabled = false, mode = \\\"1920x1200@60\\\", position = \\\"2560x240\\\", scale = 1, transform = 0 })\""
    ));
}

#[test]
fn named_apply_skips_hyprctl_when_layout_is_already_active() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");
    let fake_bin = fake_hyprctl(0, "ok\n", "");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .env("PATH", fake_bin.path())
        .args(["apply", "desk"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("No changes: profile `desk` is already active"));
    assert!(!fs::read_to_string(fake_bin.path().join("calls"))
        .unwrap()
        .contains("--batch"));
}

#[test]
fn live_apply_ignores_the_monitor_fixture_environment() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");
    let fake_bin = fake_hyprctl(0, "ok\n", "");
    let laptop_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hyprctl-monitors-laptop.json");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", laptop_fixture)
        .env("PATH", fake_bin.path())
        .args(["apply", "desk", "--unattended"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("No changes: profile `desk` is already active"));
    assert!(!fs::read_to_string(fake_bin.path().join("calls"))
        .unwrap()
        .contains("--batch"));
}

#[test]
fn noninteractive_changed_apply_requires_explicit_unattended_flag() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");
    make_profile_non_noop(config_dir.path(), "desk");
    let fake_bin = fake_hyprctl(0, "ok\n", "");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .env("PATH", fake_bin.path())
        .args(["apply", "desk"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("interactive confirmation requires terminal"));
    assert!(stderr.contains("--unattended"));
}

#[test]
fn export_rejects_conf_format() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["export", "desk", "--format", "conf"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("invalid value 'conf'"));
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
        "hl.monitor({ output = \"DP-1\", disabled = false, mode = \"2560x1440@144\", position = \"0x0\", scale = 1, transform = 0 })\nhl.monitor({ output = \"eDP-1\", disabled = false, mode = \"1920x1200@60\", position = \"2560x240\", scale = 1, transform = 0 })"
    );
}

#[test]
fn export_without_name_auto_selects_best_match() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["export", "--format", "lua"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(config_dir.path().join("generated").join("monitors.lua")).unwrap(),
        "hl.monitor({ output = \"DP-1\", disabled = false, mode = \"2560x1440@144\", position = \"0x0\", scale = 1, transform = 0 })\nhl.monitor({ output = \"eDP-1\", disabled = false, mode = \"1920x1200@60\", position = \"2560x240\", scale = 1, transform = 0 })"
    );
}

#[test]
fn export_reports_layout_warnings() {
    let config_dir = tempdir().unwrap();
    save_overlapping_profile(config_dir.path(), "overlap");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["export", "overlap", "--format", "lua"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Warnings:"));
    assert!(stdout.contains("overlap"));
    assert!(stdout.contains("Exported profile `overlap`"));
}

#[test]
fn unsafe_blackout_is_previewed_as_refused_and_cannot_be_exported() {
    let config_dir = tempdir().unwrap();
    save_blackout_profile(config_dir.path(), "blackout");

    let preview = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["apply", "blackout", "--dry-run"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    let stdout = String::from_utf8(preview.stdout).unwrap();
    assert!(stdout.contains("Operation: refused:"));
    assert!(stdout.contains("disables every saved output"));

    let export = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["export", "blackout", "--format", "lua"])
        .output()
        .unwrap();
    assert_eq!(export.status.code(), Some(1));
    assert!(String::from_utf8(export.stderr)
        .unwrap()
        .contains("disables every saved output"));
    assert!(!config_dir
        .path()
        .join("generated")
        .join("monitors.lua")
        .exists());
}

#[test]
fn apply_reports_warnings_before_live_apply_success() {
    let config_dir = tempdir().unwrap();
    save_overlapping_profile(config_dir.path(), "overlap");
    let after = include_str!("fixtures/hyprctl-monitors-desk.json")
        .replacen("\"x\": 2560", "\"x\": 100", 1)
        .replacen("\"y\": 240", "\"y\": 0", 1);
    let fake_bin = fake_hyprctl_stateful(&after);

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .env("PATH", fake_bin.path())
        .args(["apply", "overlap", "--unattended"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let warning_index = stdout.find("Warnings:").expect("missing warnings");
    let applied_index = stdout
        .find("Applied profile `overlap`")
        .expect("missing apply success");
    assert!(
        warning_index < applied_index,
        "warnings should be printed before apply success:\n{stdout}"
    );
}

#[test]
fn apply_failure_preserves_hyprctl_stderr_details() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");
    make_profile_non_noop(config_dir.path(), "desk");
    let fake_bin = fake_hyprctl(1, "", "synthetic hyprctl failure");

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .env("PATH", fake_bin.path())
        .args(["apply", "desk", "--unattended"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("failed to apply profile `desk`"));
    assert!(stderr.contains("synthetic hyprctl failure"));
    assert!(stderr.contains("Previous layout:"));
}

#[test]
fn apply_failure_preserves_hyprctl_stdout_errors() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");
    make_profile_non_noop(config_dir.path(), "desk");
    let fake_bin = fake_hyprctl(
        0,
        "keyword can't work with non-legacy parsers. Use eval.\n",
        "",
    );

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .env("PATH", fake_bin.path())
        .args(["apply", "desk", "--unattended"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("failed to apply profile `desk`"));
    assert!(stderr.contains("keyword can't work"));
    assert!(stderr.contains("Previous layout:"));
}

#[test]
fn apply_failure_attempts_and_reports_successful_rollback() {
    let config_dir = tempdir().unwrap();
    save_desk_profile(config_dir.path(), "desk");
    make_profile_non_noop(config_dir.path(), "desk");
    let fake_bin = fake_hyprctl_fail_then_ok();

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .env("PATH", fake_bin.path())
        .args(["apply", "desk", "--unattended"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("synthetic first apply failure"));
    assert!(stderr.contains("previous monitor layout was restored"));
    assert_eq!(
        fs::read_to_string(fake_bin.path().join("count"))
            .unwrap()
            .trim(),
        "2"
    );
}

#[test]
fn export_unknown_profile_returns_clear_error() {
    let config_dir = tempdir().unwrap();

    let output = hyprdisjust()
        .env("HYPRDISJUST_CONFIG_DIR", config_dir.path())
        .env("HYPRDISJUST_MONITORS_JSON", desk_fixture())
        .args(["export", "missing", "--format", "lua"])
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

fn save_overlapping_profile(config_dir: &std::path::Path, name: &str) {
    let monitors =
        parse_monitors_output(include_str!("fixtures/hyprctl-monitors-desk.json")).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some(name), &monitors, false)
        .unwrap();
    store.profiles[0].outputs[1].x = 100;
    store.profiles[0].outputs[1].y = 0;
    store.save_atomic(config_dir.join("profiles.toml")).unwrap();
}

fn save_blackout_profile(config_dir: &std::path::Path, name: &str) {
    let monitors =
        parse_monitors_output(include_str!("fixtures/hyprctl-monitors-desk.json")).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some(name), &monitors, false)
        .unwrap();
    for output in &mut store.profiles[0].outputs {
        output.enabled = false;
    }
    store.save_atomic(config_dir.join("profiles.toml")).unwrap();
}

fn make_profile_non_noop(config_dir: &std::path::Path, name: &str) {
    let path = config_dir.join("profiles.toml");
    let mut store = ProfileStore::load(&path).unwrap();
    let profile = store
        .profiles
        .iter_mut()
        .find(|profile| profile.name == name)
        .unwrap();
    profile.outputs[0].x += 1;
    store.save_atomic(path).unwrap();
}

fn fake_hyprctl(exit_code: i32, stdout: &str, stderr: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let path = dir.path().join("hyprctl");
    let monitors = dir.path().join("monitors.json");
    let command_stdout = dir.path().join("stdout");
    let command_stderr = dir.path().join("stderr");
    let calls = dir.path().join("calls");
    fs::write(
        &monitors,
        include_str!("fixtures/hyprctl-monitors-desk.json"),
    )
    .unwrap();
    fs::write(&command_stdout, stdout).unwrap();
    fs::write(&command_stderr, stderr).unwrap();
    let mut script = fs::File::create(&path).unwrap();
    writeln!(script, "#!/bin/sh").unwrap();
    writeln!(script, "printf '%s\\n' \"$*\" >> {:?}", calls).unwrap();
    writeln!(script, "if [ \"$1\" = '-j' ]; then").unwrap();
    writeln!(script, "  /bin/cat {:?}", monitors).unwrap();
    writeln!(script, "  exit 0").unwrap();
    writeln!(script, "fi").unwrap();
    writeln!(script, "/bin/cat {:?}", command_stdout).unwrap();
    writeln!(script, "/bin/cat {:?} >&2", command_stderr).unwrap();
    writeln!(script, "exit {exit_code}").unwrap();
    drop(script);
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    dir
}

fn fake_hyprctl_stateful(after_monitors: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let path = dir.path().join("hyprctl");
    let before = dir.path().join("before.json");
    let after = dir.path().join("after.json");
    let applied = dir.path().join("applied");
    fs::write(&before, include_str!("fixtures/hyprctl-monitors-desk.json")).unwrap();
    fs::write(&after, after_monitors).unwrap();

    let mut script = fs::File::create(&path).unwrap();
    writeln!(script, "#!/bin/sh").unwrap();
    writeln!(script, "if [ \"$1\" = '-j' ]; then").unwrap();
    writeln!(script, "  if [ -f {:?} ]; then", applied).unwrap();
    writeln!(script, "    /bin/cat {:?}", after).unwrap();
    writeln!(script, "  else").unwrap();
    writeln!(script, "    /bin/cat {:?}", before).unwrap();
    writeln!(script, "  fi").unwrap();
    writeln!(script, "  exit 0").unwrap();
    writeln!(script, "fi").unwrap();
    writeln!(script, ": > {:?}", applied).unwrap();
    writeln!(script, "printf '%s\\n' ok").unwrap();
    drop(script);
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    dir
}

fn fake_systemctl() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let path = dir.path().join("systemctl");
    let calls = dir.path().join("calls");
    let mut script = fs::File::create(&path).unwrap();
    writeln!(script, "#!/bin/sh").unwrap();
    writeln!(script, "printf '%s\\n' \"$*\" >> {:?}", calls).unwrap();
    writeln!(script, "exit 0").unwrap();
    drop(script);
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    dir
}

fn fake_hyprctl_fail_then_ok() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let path = dir.path().join("hyprctl");
    let count = dir.path().join("count");
    let monitors = dir.path().join("monitors.json");
    fs::write(
        &monitors,
        include_str!("fixtures/hyprctl-monitors-desk.json"),
    )
    .unwrap();
    let mut script = fs::File::create(&path).unwrap();
    writeln!(script, "#!/bin/sh").unwrap();
    writeln!(script, "if [ \"$1\" = '-j' ]; then").unwrap();
    writeln!(script, "  /bin/cat {:?}", monitors).unwrap();
    writeln!(script, "  exit 0").unwrap();
    writeln!(script, "fi").unwrap();
    writeln!(script, "count=0").unwrap();
    writeln!(script, "[ ! -f {:?} ] || read count < {:?}", count, count).unwrap();
    writeln!(script, "count=$((count + 1))").unwrap();
    writeln!(script, "printf '%s\\n' \"$count\" > {:?}", count).unwrap();
    writeln!(script, "if [ \"$count\" -eq 1 ]; then").unwrap();
    writeln!(
        script,
        "  printf '%s\\n' 'synthetic first apply failure' >&2"
    )
    .unwrap();
    writeln!(script, "  exit 1").unwrap();
    writeln!(script, "fi").unwrap();
    writeln!(script, "printf '%s\\n' ok").unwrap();
    drop(script);
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    dir
}
