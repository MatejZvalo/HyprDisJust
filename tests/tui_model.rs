use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::profile::store::{ProfileOutput, ProfileStore};
use hyprdisjust::tui::geometry::{
    move_output, output_rect, output_rect_with_monitors, snap_output, CanvasTransform,
    SnapDirection, TERMINAL_CELL_ASPECT_RATIO,
};
use hyprdisjust::tui::{
    format_snapshot, initial_model, render, TuiAction, TuiApp, TuiEffect, TuiInputMode,
};
use hyprdisjust::{
    config::AppConfig,
    config::ConfigPaths,
    profile::validation::{MAX_PROFILE_NAME_BYTES, MAX_SCALE, MIN_SCALE},
};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

const DESK: &str = include_str!("fixtures/hyprctl-monitors-desk.json");

#[test]
fn initial_model_selects_best_matching_profile() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();

    let model = initial_model(&store, &monitors, None).unwrap();

    assert_eq!(model.selected_profile.as_deref(), Some("desk"));
    assert_eq!(model.monitors.len(), 2);
    assert_eq!(model.profiles.len(), 1);
    assert!(model.apply_plan.is_some());
    assert_eq!(model.status, "Previewing profile `desk`");
}

#[test]
fn snapshot_handles_empty_profile_store() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let store = ProfileStore::default();

    let model = initial_model(&store, &monitors, None).unwrap();
    let snapshot = format_snapshot(&model);

    assert_eq!(model.selected_profile, None);
    assert!(snapshot.contains("No profiles saved yet"));
    assert!(snapshot.contains("Profiles:\n- none"));
}

#[test]
fn renderer_draws_initial_model_to_terminal_buffer() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let model = initial_model(&store, &monitors, None).unwrap();
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| render(frame, &model)).unwrap();

    let buffer = terminal.backend().buffer();
    let rendered = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("HyprDisJust"));
    assert!(rendered.contains("Monitors"));
    assert!(rendered.contains("Profiles"));
    assert!(rendered.contains("Preview"));
}

#[test]
fn selecting_profile_rebuilds_the_draft_preview() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let mut shifted = monitors.clone();
    shifted[0].x = 120;
    store
        .save_current_profile(Some("shifted"), &shifted, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);

    app.handle_action(TuiAction::SelectProfile(1)).unwrap();

    assert_eq!(
        app.view_model().selected_profile.as_deref(),
        Some("shifted")
    );
    assert!(app.draft_apply_plan().unwrap().batch.contains("120x0"));
    assert!(!app.dirty);
}

#[test]
fn new_draft_starts_from_current_monitors_even_when_profiles_exist() {
    let saved_monitors = parse_monitors_output(DESK).unwrap();
    let mut current_monitors = saved_monitors.clone();
    current_monitors[0].x = 120;

    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &saved_monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, current_monitors);

    app.handle_action(TuiAction::NewDraft).unwrap();

    let model = app.view_model();
    assert_eq!(model.selected_profile, None);
    assert!(app.dirty);
    assert!(app.draft_apply_plan().unwrap().batch.contains("120x0"));
    assert!(app.status.contains("from current monitors"));
}

#[test]
fn movement_marks_draft_dirty_and_apply_returns_shared_plan_effect() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);

    app.handle_action(TuiAction::MoveSelected(-40, 20)).unwrap();
    let effect = app.handle_action(TuiAction::ApplyDraft).unwrap();

    assert!(app.dirty);
    assert!(matches!(effect, TuiEffect::Apply(_)));
}

#[test]
fn tui_replans_noop_decision_from_fresh_monitor_state() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        store,
        ConfigPaths::from_config_dir(temp.path()).unwrap(),
        AppConfig::default(),
        monitors.clone(),
    )
    .unwrap();
    assert!(app.draft_apply_plan().unwrap().is_noop);

    let mut changed = monitors;
    changed[0].x += 20;
    let fresh_plan = app.draft_apply_plan_for_monitors(changed).unwrap();

    assert!(!fresh_plan.is_noop);
}

#[test]
fn apply_with_warnings_requires_confirmation() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("overlap"), &monitors, false)
        .unwrap();
    store.profiles[0].outputs[1].x = 100;
    store.profiles[0].outputs[1].y = 0;
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);

    let effect = app.handle_action(TuiAction::ApplyDraft).unwrap();

    assert_eq!(effect, TuiEffect::None);
    assert_eq!(app.input_mode, TuiInputMode::ConfirmApply);
    assert!(app.status.contains("warning"));
    assert!(matches!(
        app.handle_action(TuiAction::Confirm).unwrap(),
        TuiEffect::Apply(_)
    ));
}

#[test]
fn nudge_uses_configured_tui_move_step() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        store,
        ConfigPaths::from_config_dir(temp.path()).unwrap(),
        AppConfig {
            tui_move_step: 80,
            ..AppConfig::default()
        },
        monitors,
    )
    .unwrap();

    app.handle_action(TuiAction::NudgeRight).unwrap();

    assert_eq!(app.draft.outputs[0].x, 80);
}

#[test]
fn snapshot_and_renderer_show_configured_move_step() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let app = TuiApp::new(
        store,
        ConfigPaths::from_config_dir(temp.path()).unwrap(),
        AppConfig {
            tui_move_step: 80,
            ..AppConfig::default()
        },
        monitors,
    )
    .unwrap();
    let model = app.view_model();

    assert!(format_snapshot(&model).contains("Move step: 80"));

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(frame, &model)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("move step: 80"));
}

#[test]
fn mode_selection_is_explicit_before_changing_mode() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);

    app.handle_action(TuiAction::CycleSelectedMode).unwrap();
    assert!(matches!(
        app.input_mode,
        TuiInputMode::ModeSelect { ref modes, cursor: 0 } if modes.len() == 2
    ));
    assert_eq!(app.draft.outputs[0].mode, "2560x1440@144");

    app.handle_action(TuiAction::SelectNextMode).unwrap();
    app.handle_action(TuiAction::Submit).unwrap();

    assert_eq!(app.input_mode, TuiInputMode::Normal);
    assert_eq!(app.draft.outputs[0].mode, "1920x1080@60.00");
    assert!(app.dirty);
}

#[test]
fn snap_target_selection_is_explicit_when_multiple_targets_exist() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);
    let mut third = app.draft.outputs[1].clone();
    third.monitor_id = "third".to_owned();
    third.x = 5000;
    app.draft.outputs.push(third);

    app.handle_action(TuiAction::SnapSelected(SnapDirection::Left))
        .unwrap();

    assert!(matches!(
        app.input_mode,
        TuiInputMode::SnapTarget { ref targets, cursor: 0, .. } if targets.len() == 2
    ));
    app.handle_action(TuiAction::SelectNextMode).unwrap();
    app.handle_action(TuiAction::Submit).unwrap();
    assert_eq!(app.input_mode, TuiInputMode::Normal);
    assert!(app.dirty);
}

#[test]
fn save_as_writes_profile_store_and_selects_saved_profile() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let store = ProfileStore::default();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);

    app.handle_action(TuiAction::BeginSaveAs).unwrap();
    app.input_mode = TuiInputMode::SaveAs {
        name: "desk".to_owned(),
    };
    app.handle_action(TuiAction::Submit).unwrap();

    let saved = ProfileStore::load(temp.path().join("profiles.toml")).unwrap();
    assert_eq!(saved.profiles.len(), 1);
    assert_eq!(saved.profiles[0].name, "desk");
    assert_eq!(app.view_model().selected_profile.as_deref(), Some("desk"));
    assert!(!app.dirty);
}

#[test]
fn save_as_existing_profile_requires_replace_confirmation() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);

    app.input_mode = TuiInputMode::SaveAs {
        name: "desk".to_owned(),
    };
    app.handle_action(TuiAction::Submit).unwrap();

    assert!(matches!(
        app.input_mode,
        TuiInputMode::ConfirmReplace { ref name } if name == "desk"
    ));
}

#[test]
fn profile_name_editing_is_bounded_by_utf8_byte_length() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), ProfileStore::default(), monitors);
    app.input_mode = TuiInputMode::SaveAs {
        name: String::new(),
    };

    for _ in 0..MAX_PROFILE_NAME_BYTES {
        app.handle_action(TuiAction::SaveNameChar('a')).unwrap();
    }
    app.handle_action(TuiAction::SaveNameChar('é')).unwrap();

    assert!(matches!(
        app.input_mode,
        TuiInputMode::SaveAs { ref name } if name.len() == MAX_PROFILE_NAME_BYTES
    ));
}

#[test]
fn tui_scale_editor_uses_shared_validation_range() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), ProfileStore::default(), monitors);

    app.draft.outputs[0].scale = MIN_SCALE;
    app.handle_action(TuiAction::AdjustSelectedScale(-1.0))
        .unwrap();
    assert_eq!(app.draft.outputs[0].scale, MIN_SCALE);

    app.draft.outputs[0].scale = MAX_SCALE;
    app.handle_action(TuiAction::AdjustSelectedScale(1.0))
        .unwrap();
    assert_eq!(app.draft.outputs[0].scale, MAX_SCALE);
}

#[test]
fn moving_a_disabled_output_does_not_mark_the_draft_dirty() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), ProfileStore::default(), monitors);
    app.draft.outputs[0].enabled = false;

    app.handle_action(TuiAction::MoveSelected(20, 20)).unwrap();

    assert!(!app.dirty);
}

#[test]
fn tui_can_rename_and_delete_saved_profiles() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);

    app.handle_action(TuiAction::BeginRename).unwrap();
    app.input_mode = TuiInputMode::RenameProfile {
        name: "work".to_owned(),
    };
    app.handle_action(TuiAction::Submit).unwrap();
    assert_eq!(app.view_model().selected_profile.as_deref(), Some("work"));
    assert!(ProfileStore::load(temp.path().join("profiles.toml"))
        .unwrap()
        .has_profile("work"));

    app.handle_action(TuiAction::BeginDelete).unwrap();
    assert!(matches!(
        app.input_mode,
        TuiInputMode::ConfirmDelete { ref name } if name == "work"
    ));
    app.handle_action(TuiAction::Confirm).unwrap();
    let saved = ProfileStore::load(temp.path().join("profiles.toml")).unwrap();
    assert!(saved.profiles.is_empty());
    assert_eq!(app.view_model().selected_profile, None);
}

#[test]
fn tui_can_copy_a_saved_profile_with_a_collision_safe_name() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);

    app.handle_action(TuiAction::BeginCopy).unwrap();
    assert!(matches!(
        app.input_mode,
        TuiInputMode::CopyProfile { ref source, ref name }
            if source == "desk" && name == "desk-copy"
    ));
    app.handle_action(TuiAction::Submit).unwrap();

    let saved = ProfileStore::load(temp.path().join("profiles.toml")).unwrap();
    assert!(saved.has_profile("desk"));
    assert!(saved.has_profile("desk-copy"));
    assert_eq!(
        app.view_model().selected_profile.as_deref(),
        Some("desk-copy")
    );
}

#[test]
fn tui_refresh_and_auto_selection_are_explicit_actions() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors.clone());

    assert_eq!(
        app.handle_action(TuiAction::RefreshMonitors).unwrap(),
        TuiEffect::RefreshMonitors
    );
    let mut refreshed = monitors;
    refreshed[0].x = 40;
    app.update_monitors(refreshed);
    assert!(app.status.contains("Refreshed 2 current monitors"));

    app.handle_action(TuiAction::AutoSelect).unwrap();
    assert!(app.status.contains("Automatic selection: desk"));
    assert!(app.status.contains("Confidence: exact"));
    assert!(app.status.contains("Reason:"));
}

#[test]
fn tui_validation_errors_stay_in_the_editor() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);
    app.draft.outputs[0].scale = 0.0;

    let effect = app.handle_action(TuiAction::ApplyDraft).unwrap();

    assert_eq!(effect, TuiEffect::None);
    assert!(app.status.contains("Draft cannot be applied"));
    assert!(app.status.contains("invalid scale"));
}

#[test]
fn modified_draft_cannot_be_silently_replaced_by_profile_navigation() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    store
        .save_current_profile(Some("other"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);
    app.handle_action(TuiAction::MoveSelected(25, 0)).unwrap();
    let before = app.draft.clone();

    app.handle_action(TuiAction::NextProfile).unwrap();

    assert_eq!(app.draft, before);
    assert!(app.status.contains("Save the modified draft"));
}

#[test]
fn help_overlay_and_small_terminal_render_without_panicking() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let store = ProfileStore::default();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);
    app.handle_action(TuiAction::ShowHelp).unwrap();
    let model = app.view_model();

    for (width, height) in [(100, 30), (12, 4), (1, 1)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &model)).unwrap();
    }
}

#[test]
fn dirty_quit_requires_confirmation() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors);

    app.handle_action(TuiAction::MoveSelected(10, 0)).unwrap();
    let effect = app.handle_action(TuiAction::RequestQuit).unwrap();

    assert_eq!(effect, TuiEffect::None);
    assert_eq!(app.input_mode, TuiInputMode::ConfirmQuit);
    assert_eq!(
        app.handle_action(TuiAction::Confirm).unwrap(),
        TuiEffect::Quit
    );
}

#[test]
fn geometry_moves_and_snaps_outputs() {
    let mut outputs = vec![
        output("left", 0, 0, "1920x1080@60", 1.0, 0),
        output("right", 2500, 100, "1920x1080@60", 1.0, 0),
    ];

    move_output(&mut outputs[0], 100, 50);
    assert_eq!((outputs[0].x, outputs[0].y), (100, 50));

    assert!(snap_output(&mut outputs, 0, 1, SnapDirection::Left));
    assert_eq!((outputs[0].x, outputs[0].y), (580, 100));
}

#[test]
fn canvas_transform_moves_single_monitor_without_resizing_it() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };
    let mut outputs = vec![output("laptop", 0, 0, "1920x1080@60", 1.0, 0)];
    let before =
        CanvasTransform::new(&outputs, area).to_cell_rect(output_rect(&outputs[0]).unwrap());

    move_output(&mut outputs[0], 200, 0);
    let after =
        CanvasTransform::new(&outputs, area).to_cell_rect(output_rect(&outputs[0]).unwrap());

    assert!(after.x > before.x);
    assert_eq!(after.width, before.width);
    assert_eq!(after.height, before.height);
}

#[test]
fn canvas_transform_preserves_visual_monitor_proportions() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };
    let output = output("wide", 0, 0, "1920x1080@60", 1.0, 0);
    let cell_rect = CanvasTransform::new(std::slice::from_ref(&output), area)
        .to_cell_rect(output_rect(&output).unwrap());

    let visual_ratio =
        f64::from(cell_rect.width) / (f64::from(cell_rect.height) * TERMINAL_CELL_ASPECT_RATIO);

    assert!((visual_ratio - (16.0 / 9.0)).abs() < 0.1);
}

#[test]
fn canvas_transform_clips_rectangles_to_the_canvas_area() {
    let area = Rect {
        x: 10,
        y: 2,
        width: 20,
        height: 8,
    };
    let outputs = vec![
        output("left", 0, 0, "1920x1080@60", 1.0, 0),
        output("far", 12_000, 4_000, "3840x2160@60", 1.0, 0),
    ];

    for output in &outputs {
        let cell_rect =
            CanvasTransform::new(&outputs, area).to_cell_rect(output_rect(output).unwrap());

        assert!(cell_rect.right() <= area.right());
        assert!(cell_rect.bottom() <= area.bottom());
        assert!(cell_rect.width <= area.width);
        assert!(cell_rect.height <= area.height);
    }
}

#[test]
fn geometry_uses_scaled_transformed_logical_size() {
    let output = output("pivot", 0, 0, "3840x2160@60", 1.2, 1);

    let rect = output_rect(&output).unwrap();

    assert_eq!(rect.width, 1800);
    assert_eq!(rect.height, 3200);
}

#[test]
fn geometry_ignores_disabled_snap_targets() {
    let mut outputs = vec![
        output("selected", 0, 0, "1920x1080@60", 1.0, 0),
        output("disabled", 2500, 0, "1920x1080@60", 1.0, 0),
    ];
    outputs[1].enabled = false;

    assert!(!snap_output(&mut outputs, 0, 1, SnapDirection::Right));
}

#[test]
fn geometry_resolves_supported_special_modes_from_current_monitor() {
    let monitors = parse_monitors_output(DESK).unwrap();
    for mode in ["preferred", "highres", "highrr", "maxwidth"] {
        let output = output(&monitors[0].id, 0, 0, mode, 1.0, 0);
        let rect = output_rect_with_monitors(&output, &monitors).unwrap();
        assert!(rect.width > 0);
        assert!(rect.height > 0);
    }
}

#[test]
fn canvas_bounds_include_large_positive_gaps() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };
    let outputs = vec![
        output("left", 0, 0, "1920x1080@60", 1.0, 0),
        output("far", 12_000, 0, "1920x1080@60", 1.0, 0),
    ];
    let transform = CanvasTransform::new(&outputs, area);
    let left = transform.to_cell_rect(output_rect(&outputs[0]).unwrap());
    let far = transform.to_cell_rect(output_rect(&outputs[1]).unwrap());

    assert!(far.x > left.right());
}

#[test]
fn successful_apply_updates_current_monitor_snapshot() {
    let monitors = parse_monitors_output(DESK).unwrap();
    let mut store = ProfileStore::default();
    store
        .save_current_profile(Some("desk"), &monitors, false)
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), store, monitors.clone());
    let mut updated = monitors;
    updated[0].x += 20;

    app.mark_applied(updated.clone());

    assert_eq!(app.monitors, updated);
}

fn app_for(
    config_dir: &std::path::Path,
    store: ProfileStore,
    monitors: Vec<hyprdisjust::hyprland::monitor::MonitorState>,
) -> TuiApp {
    TuiApp::new(
        store,
        ConfigPaths::from_config_dir(config_dir).unwrap(),
        AppConfig::default(),
        monitors,
    )
    .unwrap()
}

fn output(id: &str, x: i32, y: i32, mode: &str, scale: f64, transform: i32) -> ProfileOutput {
    ProfileOutput {
        monitor_id: id.to_owned(),
        enabled: true,
        mode: mode.to_owned(),
        x,
        y,
        scale,
        transform,
    }
}
