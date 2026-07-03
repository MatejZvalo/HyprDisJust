use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::hyprland::monitor::MonitorState;
use hyprdisjust::profile::render::{
    render_hyprctl_batch, render_hyprland_conf, render_hyprland_lua, render_monitor_rules,
};
use hyprdisjust::profile::store::{Profile, ProfileMonitor, ProfileOutput};
use pretty_assertions::assert_eq;

const DESK: &str = include_str!("fixtures/hyprctl-monitors-desk.json");
const DOCK_RENAMED: &str = include_str!("fixtures/hyprctl-monitors-dock-renamed.json");

#[test]
fn renders_exact_monitor_rules_from_saved_profile() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let profile = profile_from_monitors("desk", &monitors);

    let rendered = render_monitor_rules(&profile, &monitors).unwrap();

    assert_eq!(
        rendered.rules,
        vec![
            "monitor DP-1,2560x1440@144,0x0,1",
            "monitor eDP-1,1920x1200@60,2560x240,1",
        ]
    );
    assert_eq!(
        rendered.batch,
        "keyword monitor DP-1,2560x1440@144,0x0,1 ; keyword monitor eDP-1,1920x1200@60,2560x240,1"
    );
    assert_eq!(
        render_hyprland_conf(&rendered.rules).unwrap(),
        "monitor = DP-1,2560x1440@144,0x0,1\nmonitor = eDP-1,1920x1200@60,2560x240,1"
    );
}

#[test]
fn maps_saved_monitor_ids_to_current_output_names() {
    let saved = parse_monitors_output(DESK).unwrap();
    let current = parse_monitors_output(DOCK_RENAMED).unwrap();
    let profile = profile_from_monitors("desk", &saved);

    let rendered = render_monitor_rules(&profile, &current).unwrap();

    assert_eq!(
        rendered.rules,
        vec![
            "monitor HDMI-A-1,2560x1440@144,0x0,1",
            "monitor eDP-2,1920x1200@60,2560x240,1",
        ]
    );
}

#[test]
fn renders_disabled_outputs_and_transform_args() {
    let mut current = vec![monitor("DP-3", "acme:pivot:123")];
    current[0].transform = 1;
    let profile = Profile {
        name: "pivot".to_owned(),
        created_at: "created".to_owned(),
        updated_at: "updated".to_owned(),
        monitors: vec![profile_monitor("acme:pivot:123", "DP-3")],
        outputs: vec![
            ProfileOutput {
                monitor_id: "acme:pivot:123".to_owned(),
                enabled: true,
                mode: "1920x1080@60".to_owned(),
                x: -1080,
                y: 0,
                scale: 1.5,
                transform: 1,
            },
            ProfileOutput {
                monitor_id: "acme:disabled:456".to_owned(),
                enabled: false,
                mode: "preferred".to_owned(),
                x: 0,
                y: 0,
                scale: 1.0,
                transform: 0,
            },
        ],
    };
    current.push(monitor("HDMI-A-2", "acme:disabled:456"));

    let rendered = render_monitor_rules(&profile, &current).unwrap();

    assert_eq!(
        rendered.rules,
        vec![
            "monitor DP-3,1920x1080@60,-1080x0,1.5,transform,1",
            "monitor HDMI-A-2,disable",
        ]
    );
    assert_eq!(
        render_hyprland_conf(&rendered.rules).unwrap(),
        "monitor = DP-3,1920x1080@60,-1080x0,1.5,transform,1\nmonitor = HDMI-A-2,disable"
    );
}

#[test]
fn renders_lua_export_as_deterministic_keyword_calls() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let profile = profile_from_monitors("desk", &monitors);

    let rendered = render_monitor_rules(&profile, &monitors).unwrap();

    assert_eq!(
        render_hyprland_lua(&rendered.rules).unwrap(),
        "hyprland.keyword(\"monitor\", \"DP-1,2560x1440@144,0x0,1\")\nhyprland.keyword(\"monitor\", \"eDP-1,1920x1200@60,2560x240,1\")"
    );
}

#[test]
fn escapes_lua_monitor_rule_values() {
    let rules = vec!["monitor DP\"1,preferred\\mode,0x0,1".to_owned()];

    assert_eq!(
        render_hyprland_lua(&rules).unwrap(),
        "hyprland.keyword(\"monitor\", \"DP\\\"1,preferred\\\\mode,0x0,1\")"
    );
}

#[test]
fn refuses_profiles_when_required_monitor_is_missing() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let profile = profile_from_monitors("desk", &monitors);

    let error = render_monitor_rules(&profile, &monitors[..1])
        .unwrap_err()
        .to_string();

    assert!(error.contains("failed to map profile `desk` monitor"));
}

#[test]
fn refuses_batch_command_separators() {
    let error = render_hyprctl_batch(&["monitor DP-1,preferred,auto,1 ; dispatch exit".to_owned()])
        .unwrap_err()
        .to_string();

    assert!(error.contains("command separators"));
}

fn profile_from_monitors(name: &str, monitors: &[MonitorState]) -> Profile {
    Profile::from_monitors(
        name.to_owned(),
        monitors,
        "created".to_owned(),
        "updated".to_owned(),
    )
}

fn monitor(output_name: &str, id: &str) -> MonitorState {
    MonitorState {
        output_name: output_name.to_owned(),
        id: id.to_owned(),
        description: "Acme Display".to_owned(),
        make: "Acme".to_owned(),
        model: "Display".to_owned(),
        serial: "123".to_owned(),
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

fn profile_monitor(id: &str, name_hint: &str) -> ProfileMonitor {
    ProfileMonitor {
        id: id.to_owned(),
        name_hint: name_hint.to_owned(),
        description: "Acme Display".to_owned(),
        make: "Acme".to_owned(),
        model: "Display".to_owned(),
        serial: "123".to_owned(),
    }
}
