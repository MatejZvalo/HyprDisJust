use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::hyprland::monitor::MonitorState;
use hyprdisjust::profile::apply::{
    ensure_plan_safe_to_apply, plan_apply, replan_apply, ApplyWarning,
};
use hyprdisjust::profile::render::{
    render_hyprctl_batch, render_hyprland_lua, render_monitor_rules,
};
use hyprdisjust::profile::store::{Profile, ProfileMonitor, ProfileOutput};
use pretty_assertions::assert_eq;

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

#[test]
fn explicit_apply_and_rollback_use_same_connector_for_duplicate_monitors() {
    let current = parse_monitors_output(DUPLICATE_NO_SERIAL).unwrap();
    let profile = profile_from_monitors("twins", &current);

    let rendered = render_monitor_rules(&profile, &current).unwrap();
    assert_eq!(
        rendered
            .mappings
            .iter()
            .map(|mapping| mapping.output_name.as_str())
            .collect::<Vec<_>>(),
        vec!["DP-1", "DP-2"]
    );

    assert!(plan_apply(&profile, &current).is_ok());
}

#[test]
fn renamed_connectors_remain_ambiguous_for_duplicate_monitors() {
    let saved = parse_monitors_output(DUPLICATE_NO_SERIAL).unwrap();
    let current = parse_monitors_output(DUPLICATE_NO_SERIAL_RENAMED).unwrap();
    let profile = profile_from_monitors("twins", &saved);

    let error = render_monitor_rules(&profile, &current).unwrap_err();
    assert!(error
        .to_string()
        .contains("maps to multiple current outputs"));
}

#[test]
fn renders_exact_monitor_rules_from_saved_profile() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let profile = profile_from_monitors("desk", &monitors);

    let rendered = render_monitor_rules(&profile, &monitors).unwrap();

    assert_eq!(
        rendered.rules,
        vec![
            "hl.monitor({ output = \"DP-1\", disabled = false, mode = \"2560x1440@144\", position = \"0x0\", scale = 1, transform = 0 })",
            "hl.monitor({ output = \"eDP-1\", disabled = false, mode = \"1920x1200@60\", position = \"2560x240\", scale = 1, transform = 0 })",
        ]
    );
    assert_eq!(
        rendered.batch,
        "eval hl.monitor({ output = \"DP-1\", disabled = false, mode = \"2560x1440@144\", position = \"0x0\", scale = 1, transform = 0 }) ; eval hl.monitor({ output = \"eDP-1\", disabled = false, mode = \"1920x1200@60\", position = \"2560x240\", scale = 1, transform = 0 })"
    );
    assert_eq!(
        render_hyprland_lua(&rendered.rules).unwrap(),
        rendered.rules.join("\n")
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
            "hl.monitor({ output = \"HDMI-A-1\", disabled = false, mode = \"2560x1440@144\", position = \"0x0\", scale = 1, transform = 0 })",
            "hl.monitor({ output = \"eDP-2\", disabled = false, mode = \"1920x1200@60\", position = \"2560x240\", scale = 1, transform = 0 })",
        ]
    );
    assert_eq!(
        rendered.mappings[0].monitor_id,
        profile.outputs[0].monitor_id
    );
    assert_eq!(rendered.mappings[0].output_name, "HDMI-A-1");
    assert_eq!(rendered.mappings[1].output_name, "eDP-2");
}

#[test]
fn disables_connected_outputs_that_are_not_in_the_profile() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let profile = Profile::from_monitors(
        "laptop".to_owned(),
        &monitors[1..],
        "created".to_owned(),
        "updated".to_owned(),
    );

    let rendered = render_monitor_rules(&profile, &monitors).unwrap();

    assert_eq!(
        rendered.rules,
        vec![
            "hl.monitor({ output = \"eDP-1\", disabled = false, mode = \"1920x1200@60\", position = \"2560x240\", scale = 1, transform = 0 })",
            "hl.monitor({ output = \"DP-1\", disabled = true })",
        ]
    );
    assert_eq!(rendered.mappings[1].monitor_id, monitors[0].id);
    assert_eq!(rendered.mappings[1].output_name, "DP-1");
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
            "hl.monitor({ output = \"DP-3\", disabled = false, mode = \"1920x1080@60\", position = \"-1080x0\", scale = 1.5, transform = 1 })",
            "hl.monitor({ output = \"HDMI-A-2\", disabled = true })",
        ]
    );
}

#[test]
fn renders_lua_export_as_deterministic_monitor_calls() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let profile = profile_from_monitors("desk", &monitors);

    let rendered = render_monitor_rules(&profile, &monitors).unwrap();

    assert_eq!(
        render_hyprland_lua(&rendered.rules).unwrap(),
        "hl.monitor({ output = \"DP-1\", disabled = false, mode = \"2560x1440@144\", position = \"0x0\", scale = 1, transform = 0 })\nhl.monitor({ output = \"eDP-1\", disabled = false, mode = \"1920x1200@60\", position = \"2560x240\", scale = 1, transform = 0 })"
    );
}

#[test]
fn escapes_lua_monitor_rule_values() {
    let current = vec![monitor("DP\"1", "acme:quoted:123")];
    let profile = Profile {
        name: "quoted".to_owned(),
        created_at: "created".to_owned(),
        updated_at: "updated".to_owned(),
        monitors: vec![profile_monitor("acme:quoted:123", "DP\"1")],
        outputs: vec![ProfileOutput {
            monitor_id: "acme:quoted:123".to_owned(),
            enabled: true,
            mode: "preferred\\mode".to_owned(),
            x: 0,
            y: 0,
            scale: 1.0,
            transform: 0,
        }],
    };
    let rendered = render_monitor_rules(&profile, &current).unwrap();

    assert_eq!(
        render_hyprland_lua(&rendered.rules).unwrap(),
        "hl.monitor({ output = \"DP\\\"1\", disabled = false, mode = \"preferred\\\\mode\", position = \"0x0\", scale = 1, transform = 0 })"
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
fn skips_a_missing_monitor_when_the_profile_wants_it_disabled() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut profile = profile_from_monitors("laptop", &monitors);
    profile.outputs[0].enabled = false;

    let rendered = render_monitor_rules(&profile, &monitors[1..]).unwrap();

    assert_eq!(rendered.rules.len(), 1);
    assert!(rendered.rules[0].contains("output = \"eDP-1\""));
    assert!(rendered.rules[0].contains("disabled = false"));
}

#[test]
fn enables_are_rendered_before_disables_regardless_of_profile_order() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut profile = profile_from_monitors("laptop", &monitors);
    profile.outputs[0].enabled = false;

    let rendered = render_monitor_rules(&profile, &monitors).unwrap();

    assert!(rendered.rules[0].contains("output = \"eDP-1\""));
    assert!(rendered.rules[0].contains("disabled = false"));
    assert!(rendered.rules[1].contains("output = \"DP-1\""));
    assert!(rendered.rules[1].contains("disabled = true"));
}

#[test]
fn refuses_batch_command_separators() {
    let error = render_hyprctl_batch(&[
        "hl.monitor({ output = \"DP-1\" }); hl.dispatch(\"exit\")".to_owned()
    ])
    .unwrap_err()
    .to_string();

    assert!(error.contains("command separators"));
}

#[test]
fn escapes_batch_eval_strings() {
    let batch = render_hyprctl_batch(&[
        "hl.monitor({ output = \"DP\\\"1\", mode = \"preferred\\\\mode\", position = \"auto\", scale = 1 })".to_owned(),
    ])
    .unwrap();

    assert_eq!(
        batch,
        "eval hl.monitor({ output = \"DP\\\"1\", mode = \"preferred\\\\mode\", position = \"auto\", scale = 1 })"
    );
}

#[test]
fn apply_plan_reports_overlapping_outputs() {
    let current = vec![
        monitor("DP-1", "acme:left:123"),
        monitor("DP-2", "acme:right:456"),
    ];
    let profile = Profile {
        name: "overlap".to_owned(),
        created_at: "created".to_owned(),
        updated_at: "updated".to_owned(),
        monitors: vec![
            profile_monitor("acme:left:123", "DP-1"),
            profile_monitor("acme:right:456", "DP-2"),
        ],
        outputs: vec![
            ProfileOutput {
                monitor_id: "acme:left:123".to_owned(),
                enabled: true,
                mode: "1920x1080@60".to_owned(),
                x: 0,
                y: 0,
                scale: 1.0,
                transform: 0,
            },
            ProfileOutput {
                monitor_id: "acme:right:456".to_owned(),
                enabled: true,
                mode: "1920x1080@60".to_owned(),
                x: 100,
                y: 0,
                scale: 1.0,
                transform: 0,
            },
        ],
    };

    let plan = plan_apply(&profile, &current).unwrap();

    assert_eq!(
        plan.warnings,
        vec![ApplyWarning::OverlappingOutputs {
            left: "acme:left:123".to_owned(),
            right: "acme:right:456".to_owned(),
        }]
    );
}

#[test]
fn apply_plan_refuses_to_apply_profiles_that_disable_every_output() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut profile = profile_from_monitors("blackout", &monitors);
    for output in &mut profile.outputs {
        output.enabled = false;
    }

    let plan = plan_apply(&profile, &monitors).unwrap();
    assert_eq!(plan.warnings, vec![ApplyWarning::DisablesAllOutputs]);

    let error = ensure_plan_safe_to_apply(&plan).unwrap_err();
    assert!(format!("{error:#}").contains("disables every saved output"));
}

#[test]
fn apply_plan_uses_scaled_transformed_logical_size_for_overlap_warnings() {
    let current = vec![
        monitor("DP-1", "hp-inc:hp-u32-4k-hdr:1cr35100lf"),
        monitor("DP-3", "hp-inc:hp-u32-4k-hdr:1cr351006k"),
    ];
    let profile = Profile {
        name: "desk".to_owned(),
        created_at: "created".to_owned(),
        updated_at: "updated".to_owned(),
        monitors: vec![
            profile_monitor("hp-inc:hp-u32-4k-hdr:1cr35100lf", "DP-1"),
            profile_monitor("hp-inc:hp-u32-4k-hdr:1cr351006k", "DP-3"),
        ],
        outputs: vec![
            ProfileOutput {
                monitor_id: "hp-inc:hp-u32-4k-hdr:1cr35100lf".to_owned(),
                enabled: true,
                mode: "3840x2160@59.997".to_owned(),
                x: 5200,
                y: 0,
                scale: 1.2,
                transform: 3,
            },
            ProfileOutput {
                monitor_id: "hp-inc:hp-u32-4k-hdr:1cr351006k".to_owned(),
                enabled: true,
                mode: "3840x2160@59.997".to_owned(),
                x: 2000,
                y: 960,
                scale: 1.2,
                transform: 0,
            },
        ],
    };

    let plan = plan_apply(&profile, &current).unwrap();

    assert_eq!(plan.warnings, vec![]);
}

#[test]
fn apply_plan_refuses_invalid_scale_even_while_disabled() {
    let current = vec![monitor("DP-1", "acme:bad:123")];
    let mut profile = Profile {
        name: "bad".to_owned(),
        created_at: "created".to_owned(),
        updated_at: "updated".to_owned(),
        monitors: vec![profile_monitor("acme:bad:123", "DP-1")],
        outputs: vec![ProfileOutput {
            monitor_id: "acme:bad:123".to_owned(),
            enabled: true,
            mode: "1920x1080@60".to_owned(),
            x: 0,
            y: 0,
            scale: 0.0,
            transform: 0,
        }],
    };

    let error = plan_apply(&profile, &current).unwrap_err().to_string();
    assert!(error.contains("invalid scale"));

    profile.outputs[0].enabled = false;
    assert!(plan_apply(&profile, &current)
        .unwrap_err()
        .to_string()
        .contains("invalid scale"));
}

#[test]
fn apply_plan_refuses_invalid_transform_and_zero_sized_enabled_mode() {
    let current = vec![monitor("DP-1", "acme:bad:123")];
    let mut profile = profile_from_monitors("bad", &current);
    profile.outputs[0].transform = 8;
    assert!(plan_apply(&profile, &current)
        .unwrap_err()
        .to_string()
        .contains("invalid transform"));

    profile.outputs[0].transform = 0;
    profile.outputs[0].mode = "0x0@0".to_owned();
    assert!(plan_apply(&profile, &current)
        .unwrap_err()
        .to_string()
        .contains("invalid mode"));
}

#[test]
fn apply_plan_detects_already_active_and_changed_layouts() {
    let current = parse_monitors_output(DESK).unwrap();
    let mut profile = profile_from_monitors("desk", &current);

    assert!(plan_apply(&profile, &current).unwrap().is_noop);

    profile.outputs[0].x += 20;
    assert!(!plan_apply(&profile, &current).unwrap().is_noop);
}

#[test]
fn immediate_replan_rebinds_rules_to_the_latest_output_names() {
    let saved = parse_monitors_output(DESK).unwrap();
    let current = parse_monitors_output(DOCK_RENAMED).unwrap();
    let profile = profile_from_monitors("desk", &saved);
    let original = plan_apply(&profile, &saved).unwrap();

    let refreshed = replan_apply(&original, &current).unwrap();

    assert!(original.batch.contains("output = \"DP-1\""));
    assert!(refreshed.batch.contains("output = \"HDMI-A-1\""));
    assert!(refreshed.batch.contains("output = \"eDP-2\""));
    assert!(!refreshed.batch.contains("output = \"DP-1\""));
}

#[test]
fn special_modes_are_verified_for_noop_detection() {
    let mut current = parse_monitors_output(DESK).unwrap();
    let mut profile = profile_from_monitors("desk", &current);

    for mode in ["preferred", "highres", "highrr", "maxwidth"] {
        profile.outputs[0].mode = mode.to_owned();
        assert!(
            plan_apply(&profile, &current).unwrap().is_noop,
            "{mode} should match the current highest advertised mode"
        );
    }

    current[0].width = 1920;
    current[0].height = 1080;
    current[0].refresh_rate = 60.0;
    profile.outputs[0].mode = "highres".to_owned();
    assert!(!plan_apply(&profile, &current).unwrap().is_noop);
}

#[test]
fn mode_validation_matches_hyprland_055_special_values() {
    let current = parse_monitors_output(DESK).unwrap();
    let mut profile = profile_from_monitors("desk", &current);

    profile.outputs[0].mode = "maxwidth".to_owned();
    assert!(plan_apply(&profile, &current).is_ok());

    for invalid in ["current", " preferred "] {
        profile.outputs[0].mode = invalid.to_owned();
        assert!(plan_apply(&profile, &current)
            .unwrap_err()
            .to_string()
            .contains("invalid mode"));
    }
}

#[test]
fn scale_validation_rejects_impractical_and_fractional_logical_sizes() {
    let current = parse_monitors_output(DESK).unwrap();
    let mut profile = profile_from_monitors("desk", &current);

    profile.outputs[0].scale = 0.0004;
    assert!(plan_apply(&profile, &current)
        .unwrap_err()
        .to_string()
        .contains("invalid scale"));

    profile.outputs[0].scale = 1.4;
    assert!(plan_apply(&profile, &current)
        .unwrap_err()
        .to_string()
        .contains("logical dimensions must be whole pixels"));

    profile.outputs[0].scale = 0.125;
    let plan = plan_apply(&profile, &current).unwrap();
    assert!(plan.batch.contains("scale = 0.125"));
}

#[test]
fn apply_plan_refuses_duplicate_outputs_and_malformed_modes() {
    let current = parse_monitors_output(DESK).unwrap();
    let mut profile = profile_from_monitors("desk", &current);
    profile.outputs.push(profile.outputs[0].clone());
    assert!(plan_apply(&profile, &current)
        .unwrap_err()
        .to_string()
        .contains("duplicate output"));

    profile.outputs.pop();
    profile.outputs[0].mode = "1920-by-1080".to_owned();
    assert!(plan_apply(&profile, &current)
        .unwrap_err()
        .to_string()
        .contains("invalid mode"));
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
        physical_width: 0,
        physical_height: 0,
    }
}
