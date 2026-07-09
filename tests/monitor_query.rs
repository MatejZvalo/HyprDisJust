use hyprdisjust::cli::format_doctor;
use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::hyprland::monitor::{parse_raw_monitors, slug_component, stable_monitor_id};
use pretty_assertions::assert_eq;

const LAPTOP: &str = include_str!("fixtures/hyprctl-monitors-laptop.json");
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

const MISSING_SCALE: &str = r#"[
  {
    "id": 0,
    "name": "DP-1",
    "description": "Acme Missing Scale",
    "make": "Acme",
    "model": "Missing Scale",
    "serial": "123",
    "width": 1920,
    "height": 1080,
    "refreshRate": 60.0,
    "x": 0,
    "y": 0,
    "transform": 0
  }
]"#;

#[test]
fn active_laptop_fixture_parses() {
    let monitors = parse_monitors_output(LAPTOP).unwrap();

    assert_eq!(monitors.len(), 1);
    assert_eq!(monitors[0].output_name, "eDP-1");
    assert_eq!(
        monitors[0].id,
        "chimei-innolux-corporation:0x13c0:no-serial"
    );
    assert_eq!(monitors[0].width, 1920);
    assert_eq!(monitors[0].height, 1280);
    assert_eq!(monitors[0].refresh_rate, 60.003);
    assert_eq!(monitors[0].x, 0);
    assert_eq!(monitors[0].y, 0);
    assert_eq!(monitors[0].scale, 1.0);
    assert_eq!(monitors[0].transform, 0);
    assert!(monitors[0].enabled);
    assert_eq!(monitors[0].available_modes, vec!["1920x1280@60.00Hz"]);
}

#[test]
fn desk_fixture_parses_two_monitors() {
    let monitors = parse_monitors_output(DESK).unwrap();

    assert_eq!(monitors.len(), 2);
    assert_eq!(monitors[0].id, "lg-electronics:lg-ultragear:12345");
    assert_eq!(monitors[1].id, "boe:internal-panel:no-serial");
    assert_eq!(monitors[1].x, 2560);
    assert_eq!(monitors[1].y, 240);
}

#[test]
fn inactive_fixture_normalizes_enabled_false() {
    let monitors = parse_monitors_output(INACTIVE).unwrap();

    assert_eq!(monitors.len(), 1);
    assert!(!monitors[0].enabled);
    assert_eq!(monitors[0].x, -1920);
}

#[test]
fn missing_scale_defaults_to_one() {
    let monitors = parse_monitors_output(MISSING_SCALE).unwrap();

    assert_eq!(monitors[0].scale, 1.0);
}

#[test]
fn duplicate_no_serial_monitor_ids_are_disambiguated_by_output_name() {
    let monitors = parse_monitors_output(DUPLICATE_NO_SERIAL).unwrap();

    assert_eq!(monitors.len(), 2);
    assert_eq!(monitors[0].id, "acme:samepanel:no-serial:output:dp-1");
    assert_eq!(monitors[1].id, "acme:samepanel:no-serial:output:dp-2");
}

#[test]
fn stable_id_with_serial() {
    let id = stable_monitor_id("LG Electronics", "LG ULTRAGEAR", "12345", "...", "DP-1");

    assert_eq!(id, "lg-electronics:lg-ultragear:12345");
}

#[test]
fn stable_id_without_serial() {
    let id = stable_monitor_id(
        "Chimei Innolux Corporation",
        "0x13C0",
        "",
        "Chimei Innolux Corporation 0x13C0",
        "eDP-1",
    );

    assert_eq!(id, "chimei-innolux-corporation:0x13c0:no-serial");
}

#[test]
fn slug_generation() {
    assert_eq!(slug_component(" LG  ULTRAGEAR!! "), "lg-ultragear");
    assert_eq!(slug_component("0x13C0"), "0x13c0");
    assert_eq!(slug_component(""), "unknown");
}

#[test]
fn doctor_format_from_fixture() {
    let monitors = parse_monitors_output(LAPTOP).unwrap();
    let output = format_doctor(&monitors);

    assert!(output.contains("Hyprland: detected"));
    assert!(output.contains("Monitors: 1"));
    assert!(output.contains("id: chimei-innolux-corporation:0x13c0:no-serial"));
    assert!(output.contains("mode: 1920x1280@60.003"));
}

#[test]
fn doctor_format_includes_disabled_status() {
    let monitors = parse_monitors_output(INACTIVE).unwrap();
    let output = format_doctor(&monitors);

    assert!(output.contains("status: disabled"));
}

#[test]
fn parse_tolerates_extra_fields() {
    let raw = parse_raw_monitors(LAPTOP).unwrap();

    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].name, "eDP-1");
}
