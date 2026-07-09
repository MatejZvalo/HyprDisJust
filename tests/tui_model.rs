use hyprdisjust::hyprland::hyprctl::parse_monitors_output;
use hyprdisjust::profile::store::{ProfileOutput, ProfileStore};
use hyprdisjust::tui::geometry::{
    move_output, output_rect, snap_output, CanvasTransform, SnapDirection,
    TERMINAL_CELL_ASPECT_RATIO,
};
use hyprdisjust::tui::{
    format_snapshot, initial_model, render, TuiAction, TuiApp, TuiEffect, TuiInputMode,
};
use hyprdisjust::{config::AppConfig, config::ConfigPaths};
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
