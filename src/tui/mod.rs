pub mod geometry;
pub mod model;

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::Context;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::hyprland::hyprctl::current_monitors;
use crate::profile::apply::{
    execute_apply_transaction_if, ApplyOutcome, ApplyTransactionRequest, ApplyTransactionState,
    TerminalConfirmation,
};

use self::geometry::{output_rect_with_monitors, CanvasTransform, SnapDirection};
pub use self::model::{
    initial_model, require_draft_plan, TuiAction, TuiApp, TuiCurrentMonitorRow, TuiEffect,
    TuiInputMode, TuiModel, TuiMonitorRow, TuiProfileRow,
};

#[derive(Debug, Clone, Copy)]
struct TuiLayout {
    profile_list: Rect,
    canvas: Rect,
}

struct TerminalGuard {
    armed: bool,
}

impl TerminalGuard {
    fn armed() -> Self {
        Self { armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen, Show);
        let _ = disable_raw_mode();
    }
}

pub fn run(mut app: TuiApp) -> anyhow::Result<()> {
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
        let _ = disable_raw_mode();
        return Err(error).context("failed to enter alternate screen");
    }
    let mut guard = TerminalGuard::armed();

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => return Err(error).context("failed to initialize terminal"),
    };
    let run_result = run_loop(&mut terminal, &mut app);
    let restore_result = restore_terminal(&mut terminal);
    if restore_result.is_ok() {
        guard.disarm();
    }

    run_result?;
    restore_result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut TuiApp,
) -> anyhow::Result<()> {
    let mut drag_last: Option<(usize, u16, u16)> = None;
    let mut needs_redraw = true;

    loop {
        if needs_redraw {
            let view_model = app.view_model();
            terminal
                .draw(|frame| render(frame, &view_model))
                .context("failed to draw terminal UI")?;
            needs_redraw = false;
        }

        if !event::poll(Duration::from_millis(100)).context("failed to poll terminal events")? {
            continue;
        }

        match event::read().context("failed to read terminal event")? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let Some(action) = key_to_action(key, &app.input_mode) else {
                    continue;
                };
                needs_redraw = true;
                match handle_effect(app, action) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => app.mark_action_failed(&error),
                }
            }
            Event::Mouse(mouse) => {
                needs_redraw = true;
                let area = terminal.size().context("failed to read terminal size")?;
                let layout = layout_areas(area.into());
                match handle_mouse(app, mouse, layout, &mut drag_last) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => app.mark_action_failed(&error),
                }
            }
            Event::Resize(_, _) => needs_redraw = true,
            _ => {}
        }
    }
}

fn handle_effect(app: &mut TuiApp, action: TuiAction) -> anyhow::Result<bool> {
    match app.handle_action(action)? {
        TuiEffect::None => Ok(false),
        TuiEffect::Quit => Ok(true),
        TuiEffect::Apply(approved_plan) => {
            let mut confirmation = TerminalConfirmation;
            let result = execute_apply_transaction_if(
                app.paths.profile_store_path(),
                ApplyTransactionRequest::Draft(app.draft.clone()),
                Some(&mut confirmation),
                |authoritative, _, _| {
                    if authoritative.profile_name != approved_plan.profile_name
                        || authoritative.mappings != approved_plan.mappings
                        || authoritative.batch != approved_plan.batch
                        || authoritative.warnings != approved_plan.warnings
                    {
                        anyhow::bail!(
                            "monitor topology changed after plan review; refresh and review the new apply plan"
                        );
                    }
                    Ok(true)
                },
            );
            match result {
                Ok(ApplyTransactionState::Completed(result)) => match &result.outcome {
                    ApplyOutcome::Confirmed => app.mark_applied(result.final_state.clone()),
                    ApplyOutcome::RolledBack { reason } => {
                        app.mark_rolled_back(result.final_state.clone(), reason)
                    }
                    ApplyOutcome::Noop => {
                        app.update_monitors(result.final_state.clone());
                        app.mark_noop();
                    }
                    ApplyOutcome::Unattended => app.mark_apply_failed(&anyhow::anyhow!(
                        "TUI apply completed without the required confirmation"
                    )),
                },
                Ok(ApplyTransactionState::NoAutomaticMatch { .. }) => app.mark_apply_failed(
                    &anyhow::anyhow!("draft apply unexpectedly used automatic selection"),
                ),
                Err(error) => app.mark_apply_failed(&error),
            }
            Ok(false)
        }
        TuiEffect::RefreshMonitors => {
            match current_monitors() {
                Ok(monitors) => app.update_monitors(monitors),
                Err(error) => app.mark_refresh_failed(&error),
            }
            Ok(false)
        }
    }
}

fn handle_mouse(
    app: &mut TuiApp,
    mouse: MouseEvent,
    layout: TuiLayout,
    drag_last: &mut Option<(usize, u16, u16)>,
) -> anyhow::Result<bool> {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            *drag_last = None;
            if contains(layout.profile_list, mouse.column, mouse.row) {
                if let Some(index) = list_index_at(layout.profile_list, mouse.row) {
                    return handle_effect(app, TuiAction::SelectProfile(index));
                }
            }

            if contains(layout.canvas, mouse.column, mouse.row) {
                let transform = CanvasTransform::new_with_monitors(
                    &app.draft.outputs,
                    &app.monitors,
                    inner(layout.canvas),
                );
                if let Some(index) = transform.output_at_with_monitors(
                    &app.draft.outputs,
                    &app.monitors,
                    mouse.column,
                    mouse.row,
                ) {
                    *drag_last = Some((index, mouse.column, mouse.row));
                    return handle_effect(app, TuiAction::SelectMonitor(index));
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let Some((index, last_column, last_row)) = *drag_last else {
                return Ok(false);
            };
            *drag_last = Some((index, mouse.column, mouse.row));
            if contains(layout.canvas, mouse.column, mouse.row) {
                let transform = CanvasTransform::new_with_monitors(
                    &app.draft.outputs,
                    &app.monitors,
                    inner(layout.canvas),
                );
                let dx = i32::from(mouse.column) - i32::from(last_column);
                let dy = i32::from(mouse.row) - i32::from(last_row);
                let (logical_dx, logical_dy) = transform.cell_delta_to_logical(dx, dy);
                return handle_effect(app, TuiAction::MoveSelected(logical_dx, logical_dy));
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            *drag_last = None;
        }
        MouseEventKind::ScrollDown => return handle_effect(app, TuiAction::NextProfile),
        MouseEventKind::ScrollUp => return handle_effect(app, TuiAction::PreviousProfile),
        _ => {}
    }

    Ok(false)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    let raw_mode_result = disable_raw_mode().context("failed to disable terminal raw mode");
    let screen_result = execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )
    .context("failed to leave alternate screen");
    let cursor_result = terminal.show_cursor().context("failed to show cursor");

    raw_mode_result?;
    screen_result?;
    cursor_result
}

pub fn render(frame: &mut Frame<'_>, model: &TuiModel) {
    let layout = layout_areas(frame.area());

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(7),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(28), Constraint::Percentage(72)])
        .split(root[1]);
    let editor = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(7)])
        .split(body[1]);
    let lower = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(root[2]);

    let title = Paragraph::new(title_line(model)).block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, root[0]);
    frame.render_widget(profile_list(model), body[0]);
    render_canvas(frame, model, editor[0]);
    frame.render_widget(details_panel(model), editor[1]);
    frame.render_widget(preview_panel(model), lower[0]);
    frame.render_widget(warning_panel(model), lower[1]);

    if model.input_mode == TuiInputMode::Help {
        render_help(frame);
    }

    debug_assert_eq!(layout.profile_list, body[0]);
    debug_assert_eq!(layout.canvas, editor[0]);
}

fn title_line(model: &TuiModel) -> Line<'_> {
    let dirty = if model.dirty { "  modified" } else { "" };
    let mode = match &model.input_mode {
        TuiInputMode::Normal => "",
        TuiInputMode::SaveAs { name } => {
            return Line::from(vec![
                Span::styled(
                    "HyprDisJust",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  save as: "),
                Span::styled(name.as_str(), Style::default().fg(Color::Yellow)),
                Span::raw("  Enter save  Esc cancel"),
            ])
        }
        TuiInputMode::ConfirmReplace { name } => {
            return Line::from(vec![
                Span::styled(
                    "HyprDisJust",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  replace `{name}`? y/Enter confirm  Esc cancel")),
            ])
        }
        TuiInputMode::ConfirmApply => "  apply warning plan? y/Enter confirm  Esc cancel",
        TuiInputMode::RenameProfile { name } => {
            return Line::from(vec![
                Span::styled(
                    "HyprDisJust",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  rename to: "),
                Span::styled(name.as_str(), Style::default().fg(Color::Yellow)),
                Span::raw("  Enter save  Esc cancel"),
            ])
        }
        TuiInputMode::CopyProfile { source, name } => {
            return Line::from(vec![
                Span::styled(
                    "HyprDisJust",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  copy `{source}` to: ")),
                Span::styled(name.as_str(), Style::default().fg(Color::Yellow)),
                Span::raw("  Enter copy  Esc cancel"),
            ])
        }
        TuiInputMode::ConfirmCopyReplace { name, .. } => {
            return Line::from(vec![
                Span::styled(
                    "HyprDisJust",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(
                    "  replace `{name}` with copy? y/Enter confirm  Esc cancel"
                )),
            ])
        }
        TuiInputMode::ConfirmDelete { name } => {
            return Line::from(vec![
                Span::styled(
                    "HyprDisJust",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  delete `{name}`? y/Enter confirm  Esc cancel")),
            ])
        }
        TuiInputMode::ModeSelect { .. } => "  choose mode with Up/Down  Enter select  Esc cancel",
        TuiInputMode::SnapTarget { .. } => {
            "  choose snap target with Up/Down  Enter select  Esc cancel"
        }
        TuiInputMode::ConfirmQuit => "  discard changes? y/Enter confirm  Esc cancel",
        TuiInputMode::Help => "  help  ?/Esc close",
    };

    Line::from(vec![
        Span::styled(
            "HyprDisJust",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(model.status.as_str()),
        Span::styled(dirty, Style::default().fg(Color::Yellow)),
        Span::raw(mode),
    ])
}

fn profile_list(model: &TuiModel) -> List<'_> {
    let items = if model.profiles.is_empty() {
        vec![ListItem::new("new draft")]
    } else {
        model
            .profiles
            .iter()
            .map(|profile| {
                let marker = if profile.selected { ">" } else { " " };
                ListItem::new(format!(
                    "{marker} {}  {} monitor{}",
                    profile.name,
                    profile.monitor_count,
                    if profile.monitor_count == 1 { "" } else { "s" }
                ))
            })
            .collect()
    };

    List::new(items).block(
        Block::default()
            .title("Profiles  [/] select")
            .borders(Borders::ALL),
    )
}

fn render_canvas(frame: &mut Frame<'_>, model: &TuiModel, area: Rect) {
    let block = Block::default()
        .title("Monitors Layout  arrows move  H/J/K/L snap  drag with mouse")
        .borders(Borders::ALL);
    let inner_area = block.inner(area);
    frame.render_widget(block, area);

    let outputs: Vec<_> = model.monitors.iter().map(profile_output_from_row).collect();
    let transform =
        CanvasTransform::new_with_monitors(&outputs, &model.current_monitor_states, inner_area);

    for (index, output) in outputs.iter().enumerate() {
        let Some(logical_rect) = output_rect_with_monitors(output, &model.current_monitor_states)
        else {
            continue;
        };
        let cell_rect = transform.to_cell_rect(logical_rect);
        if cell_rect.width == 0 || cell_rect.height == 0 {
            continue;
        }
        let row = &model.monitors[index];
        let style = if row.selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        };

        for y in cell_rect.y..cell_rect.bottom() {
            for x in cell_rect.x..cell_rect.right() {
                frame.buffer_mut()[(x, y)].set_symbol(" ").set_style(style);
            }
        }

        let label = format!(
            "{} {}",
            if row.selected { "*" } else { "" },
            row.output_name
        );
        frame.buffer_mut().set_string(
            cell_rect.x,
            cell_rect.y,
            truncate_to_width(&label, cell_rect.width),
            style,
        );
    }

    if model.monitors.is_empty() && inner_area.width > 0 && inner_area.height > 0 {
        frame.buffer_mut().set_string(
            inner_area.x,
            inner_area.y,
            "No monitors in draft",
            Style::default().fg(Color::DarkGray),
        );
    }
}

fn details_panel(model: &TuiModel) -> Paragraph<'_> {
    let mut text = model
        .monitors
        .iter()
        .find(|monitor| monitor.selected)
        .map(|monitor| {
            format!(
                "{}\nid: {}\nmode: {}  position: {}  scale: {}  transform: {}  status: {}",
                monitor.output_name,
                monitor.id,
                monitor.mode,
                monitor.position,
                format_number(monitor.scale),
                monitor.transform,
                if monitor.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            )
        })
        .unwrap_or_else(|| "No monitor selected".to_owned());
    text.push_str("\ncurrent: ");
    if model.current_monitors.is_empty() {
        text.push_str("none");
    } else {
        text.push_str(
            &model
                .current_monitors
                .iter()
                .map(|monitor| {
                    format!(
                        "{} [{}] {}",
                        monitor.output_name,
                        if monitor.enabled { "on" } else { "off" },
                        monitor.id
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        );
    }

    Paragraph::new(text)
        .block(
            Block::default()
                .title("Selected Draft / Current Monitors")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false })
}

fn preview_panel(model: &TuiModel) -> Paragraph<'_> {
    let text = model
        .apply_plan
        .as_ref()
        .map(|plan| plan.batch.clone())
        .unwrap_or_else(|| "No profile preview available.".to_owned());

    Paragraph::new(text)
        .block(
            Block::default()
                .title("Preview Command")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false })
}

fn warning_panel(model: &TuiModel) -> Paragraph<'_> {
    let mut lines = Vec::new();
    if let Some(plan) = &model.apply_plan {
        for warning in &plan.warnings {
            lines.push(format!("- {}", warning.message()));
        }
    }
    if lines.is_empty() {
        lines.push("No warnings.".to_owned());
    }
    lines.push(format!("move step: {}", model.move_step));
    lines.push("n new  s save-as  c copy  R rename  d delete  a apply  space toggle".to_owned());
    lines.push("m mode  r rotate  +/- scale  arrows move  H/J/K/L snap  ? help".to_owned());
    lines.push("f refresh  A auto-select  [/] profiles  Tab monitors  q quit".to_owned());

    Paragraph::new(lines.join("\n"))
        .block(
            Block::default()
                .title("Actions / Warnings")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false })
}

pub fn format_snapshot(model: &TuiModel) -> String {
    let mut output = "HyprDisJust TUI\n".to_owned();
    output.push_str(&model.status);
    output.push_str("\n\nCurrent monitors:");
    if model.current_monitors.is_empty() {
        output.push_str("\n- none");
    } else {
        for monitor in &model.current_monitors {
            output.push_str(&format!(
                "\n- {} [{}] id: {}",
                monitor.output_name,
                if monitor.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                monitor.id
            ));
        }
    }
    output.push_str("\n\nMonitors:");
    if model.monitors.is_empty() {
        output.push_str("\n- none");
    } else {
        for monitor in &model.monitors {
            output.push_str(&format!(
                "\n{} {} {} at {} scale {} transform {} ({})",
                if monitor.selected { "*" } else { "-" },
                monitor.output_name,
                monitor.mode,
                monitor.position,
                format_number(monitor.scale),
                monitor.transform,
                if monitor.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            ));
        }
    }

    output.push_str("\n\nProfiles:");
    if model.profiles.is_empty() {
        output.push_str("\n- none");
    } else {
        for profile in &model.profiles {
            output.push_str(&format!(
                "\n{} {} ({} monitor{})",
                if profile.selected { "*" } else { "-" },
                profile.name,
                profile.monitor_count,
                if profile.monitor_count == 1 { "" } else { "s" }
            ));
        }
    }

    if let Some(plan) = &model.apply_plan {
        output.push_str("\n\nPreview command:");
        output.push('\n');
        output.push_str(&plan.batch);
        if !plan.warnings.is_empty() {
            output.push_str("\n\nWarnings:");
            for warning in &plan.warnings {
                output.push_str("\n- ");
                output.push_str(&warning.message());
            }
        }
    }

    output.push_str(&format!("\n\nMove step: {}", model.move_step));

    output
}

fn key_to_action(key: KeyEvent, input_mode: &TuiInputMode) -> Option<TuiAction> {
    match input_mode {
        TuiInputMode::SaveAs { .. }
        | TuiInputMode::RenameProfile { .. }
        | TuiInputMode::CopyProfile { .. } => match key.code {
            KeyCode::Enter => Some(TuiAction::Submit),
            KeyCode::Esc => Some(TuiAction::Cancel),
            KeyCode::Backspace => Some(TuiAction::SaveNameBackspace),
            KeyCode::Char(character) => Some(TuiAction::SaveNameChar(character)),
            _ => None,
        },
        TuiInputMode::ConfirmReplace { .. }
        | TuiInputMode::ConfirmQuit
        | TuiInputMode::ConfirmApply
        | TuiInputMode::ConfirmCopyReplace { .. }
        | TuiInputMode::ConfirmDelete { .. } => match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => Some(TuiAction::Confirm),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(TuiAction::Cancel),
            _ => None,
        },
        TuiInputMode::ModeSelect { .. } | TuiInputMode::SnapTarget { .. } => match key.code {
            KeyCode::Enter => Some(TuiAction::Submit),
            KeyCode::Esc => Some(TuiAction::Cancel),
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') | KeyCode::Char('J') => {
                Some(TuiAction::SelectNextMode)
            }
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') | KeyCode::Char('K') => {
                Some(TuiAction::SelectPreviousMode)
            }
            _ => None,
        },
        TuiInputMode::Help => match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {
                Some(TuiAction::Cancel)
            }
            _ => None,
        },
        TuiInputMode::Normal => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(TuiAction::RequestQuit),
            KeyCode::Char('n') => Some(TuiAction::NewDraft),
            KeyCode::Char('s') => Some(TuiAction::BeginSaveAs),
            KeyCode::Char('c') => Some(TuiAction::BeginCopy),
            KeyCode::Char('R') => Some(TuiAction::BeginRename),
            KeyCode::Char('d') => Some(TuiAction::BeginDelete),
            KeyCode::Char('a') => Some(TuiAction::ApplyDraft),
            KeyCode::Char('A') => Some(TuiAction::AutoSelect),
            KeyCode::Char('f') => Some(TuiAction::RefreshMonitors),
            KeyCode::Char('?') => Some(TuiAction::ShowHelp),
            KeyCode::Char(' ') => Some(TuiAction::ToggleSelected),
            KeyCode::Char('m') => Some(TuiAction::CycleSelectedMode),
            KeyCode::Char('r') => Some(TuiAction::CycleSelectedTransform),
            KeyCode::Char('+') | KeyCode::Char('=') => Some(TuiAction::AdjustSelectedScale(0.1)),
            KeyCode::Char('-') => Some(TuiAction::AdjustSelectedScale(-0.1)),
            KeyCode::Char(']') => Some(TuiAction::NextProfile),
            KeyCode::Char('[') => Some(TuiAction::PreviousProfile),
            KeyCode::Tab => Some(TuiAction::NextMonitor),
            KeyCode::BackTab => Some(TuiAction::PreviousMonitor),
            KeyCode::Left => Some(TuiAction::NudgeLeft),
            KeyCode::Right => Some(TuiAction::NudgeRight),
            KeyCode::Up => Some(TuiAction::NudgeUp),
            KeyCode::Down => Some(TuiAction::NudgeDown),
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(TuiAction::SnapSelected(SnapDirection::Left))
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(TuiAction::SnapSelected(SnapDirection::Right))
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(TuiAction::SnapSelected(SnapDirection::Above))
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(TuiAction::SnapSelected(SnapDirection::Below))
            }
            KeyCode::Char('H') => Some(TuiAction::SnapSelected(SnapDirection::Left)),
            KeyCode::Char('L') => Some(TuiAction::SnapSelected(SnapDirection::Right)),
            KeyCode::Char('K') => Some(TuiAction::SnapSelected(SnapDirection::Above)),
            KeyCode::Char('J') => Some(TuiAction::SnapSelected(SnapDirection::Below)),
            _ => None,
        },
    }
}

fn render_help(frame: &mut Frame<'_>) {
    let area = centered_rect(frame.area(), 76, 22);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let help = [
        "Profiles: [/] browse, n new from current, s save-as, c copy, R rename, d delete",
        "Selection: Tab/Shift-Tab monitors, mouse selects profiles and monitors",
        "Layout: arrows nudge, H/J/K/L snap left/down/up/right, mouse drag moves",
        "Output: Space enable, m mode, r transform, +/- scale",
        "Actions: a apply/confirm warnings, A automatic select, f refresh current state",
        "Safety: edits stay in a draft; replace, delete, warned apply, and dirty quit confirm",
        "Quit: q or Esc. Close help: ? or Esc.",
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(help.join("\n\n"))
            .block(Block::default().title("Help").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn centered_rect(area: Rect, preferred_width: u16, preferred_height: u16) -> Rect {
    let width = preferred_width.min(area.width);
    let height = preferred_height.min(area.height);
    Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
        y: area
            .y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    }
}

fn layout_areas(area: Rect) -> TuiLayout {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(7),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(28), Constraint::Percentage(72)])
        .split(root[1]);
    let editor = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(7)])
        .split(body[1]);

    TuiLayout {
        profile_list: body[0],
        canvas: editor[0],
    }
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn list_index_at(area: Rect, row: u16) -> Option<usize> {
    let inner_area = inner(area);
    if row < inner_area.y || row >= inner_area.bottom() {
        None
    } else {
        Some((row - inner_area.y) as usize)
    }
}

fn inner(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn profile_output_from_row(row: &TuiMonitorRow) -> crate::profile::store::ProfileOutput {
    crate::profile::store::ProfileOutput {
        monitor_id: row.id.clone(),
        enabled: row.enabled,
        mode: row.mode.clone(),
        x: row
            .position
            .split_once('x')
            .and_then(|(x, _)| x.parse().ok())
            .unwrap_or_default(),
        y: row
            .position
            .split_once('x')
            .and_then(|(_, y)| y.parse().ok())
            .unwrap_or_default(),
        scale: row.scale,
        transform: row.transform,
    }
}

fn truncate_to_width(value: &str, width: u16) -> String {
    value.chars().take(width as usize).collect()
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        return format!("{value:.0}");
    }

    let formatted = format!("{value:.3}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}
