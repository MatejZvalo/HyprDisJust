use anyhow::{bail, Context};

use crate::config::{AppConfig, ConfigPaths};
use crate::hyprland::monitor::MonitorState;
use crate::profile::apply::{ensure_plan_safe_to_apply, plan_apply, ApplyPlan};
use crate::profile::r#match::{
    best_profile_match, decide_auto_apply, format_auto_apply_decision, resolve_monitor_matches,
    AutoApplyDecision,
};
use crate::profile::store::{Profile, ProfileMonitor, ProfileOutput, ProfileStore};
use crate::profile::validation::{MAX_PROFILE_NAME_BYTES, MAX_SCALE, MIN_SCALE};
use crate::text::sanitize_terminal_text;

use super::geometry::{move_output, snap_output_with_monitors, SnapDirection};

#[derive(Debug, Clone, PartialEq)]
pub struct TuiApp {
    pub paths: ConfigPaths,
    pub config: AppConfig,
    pub store: ProfileStore,
    pub monitors: Vec<MonitorState>,
    pub selected_profile_index: Option<usize>,
    pub selected_monitor_index: Option<usize>,
    pub draft: Profile,
    pub dirty: bool,
    pub input_mode: TuiInputMode,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiInputMode {
    Normal,
    SaveAs {
        name: String,
    },
    ConfirmReplace {
        name: String,
    },
    ConfirmQuit,
    ConfirmApply,
    RenameProfile {
        name: String,
    },
    CopyProfile {
        source: String,
        name: String,
    },
    ConfirmCopyReplace {
        source: String,
        name: String,
    },
    ConfirmDelete {
        name: String,
    },
    ModeSelect {
        modes: Vec<String>,
        cursor: usize,
    },
    SnapTarget {
        direction: SnapDirection,
        targets: Vec<usize>,
        cursor: usize,
    },
    Help,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TuiAction {
    NextProfile,
    PreviousProfile,
    SelectProfile(usize),
    NextMonitor,
    PreviousMonitor,
    SelectMonitor(usize),
    MoveSelected(i32, i32),
    NudgeLeft,
    NudgeRight,
    NudgeUp,
    NudgeDown,
    SnapSelected(SnapDirection),
    ToggleSelected,
    CycleSelectedMode,
    SelectNextMode,
    SelectPreviousMode,
    CycleSelectedTransform,
    AdjustSelectedScale(f64),
    NewDraft,
    BeginSaveAs,
    BeginRename,
    BeginCopy,
    BeginDelete,
    SaveNameChar(char),
    SaveNameBackspace,
    Submit,
    Cancel,
    Confirm,
    RequestQuit,
    ApplyDraft,
    RefreshMonitors,
    AutoSelect,
    ShowHelp,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TuiEffect {
    None,
    Apply(Box<ApplyPlan>),
    RefreshMonitors,
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TuiModel {
    pub monitors: Vec<TuiMonitorRow>,
    pub current_monitors: Vec<TuiCurrentMonitorRow>,
    pub current_monitor_states: Vec<MonitorState>,
    pub profiles: Vec<TuiProfileRow>,
    pub selected_profile: Option<String>,
    pub selected_monitor: Option<String>,
    pub apply_plan: Option<ApplyPlan>,
    pub status: String,
    pub input_mode: TuiInputMode,
    pub dirty: bool,
    pub move_step: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TuiMonitorRow {
    pub output_name: String,
    pub id: String,
    pub mode: String,
    pub position: String,
    pub scale: f64,
    pub transform: i32,
    pub enabled: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiCurrentMonitorRow {
    pub output_name: String,
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiProfileRow {
    pub name: String,
    pub monitor_count: usize,
    pub selected: bool,
}

impl TuiApp {
    pub fn new(
        store: ProfileStore,
        paths: ConfigPaths,
        config: AppConfig,
        monitors: Vec<MonitorState>,
    ) -> anyhow::Result<Self> {
        let best_match = best_profile_match(&store, &monitors);
        let decision = decide_auto_apply(&store, &best_match, config.fallback_profile.as_deref());
        let selected_profile_index = match &decision {
            AutoApplyDecision::Apply { profile_name, .. } => store
                .profiles
                .iter()
                .position(|profile| profile.name == *profile_name),
            _ => {
                if store.profiles.is_empty() {
                    None
                } else {
                    Some(0)
                }
            }
        };
        let draft = selected_profile_index
            .and_then(|index| store.profiles.get(index).cloned())
            .unwrap_or_else(|| draft_from_current(&store, &monitors));
        let selected_monitor_index = first_selectable_monitor(&draft);
        let mut app = Self {
            paths,
            config,
            store,
            monitors,
            selected_profile_index,
            selected_monitor_index,
            draft,
            dirty: false,
            input_mode: TuiInputMode::Normal,
            status: String::new(),
        };
        app.refresh_status();
        Ok(app)
    }

    pub fn view_model(&self) -> TuiModel {
        let selected_profile = self
            .selected_profile_index
            .and_then(|index| self.store.profiles.get(index))
            .map(|profile| sanitize_terminal_text(&profile.name));
        let apply_plan = self.draft_apply_plan().ok();

        TuiModel {
            monitors: self
                .draft
                .outputs
                .iter()
                .enumerate()
                .map(|(index, output)| {
                    monitor_row(
                        &self.draft,
                        output,
                        Some(index) == self.selected_monitor_index,
                    )
                })
                .collect(),
            current_monitors: self
                .monitors
                .iter()
                .map(|monitor| TuiCurrentMonitorRow {
                    output_name: sanitize_terminal_text(&monitor.output_name),
                    id: sanitize_terminal_text(&monitor.id),
                    enabled: monitor.enabled,
                })
                .collect(),
            current_monitor_states: self.monitors.clone(),
            profiles: self
                .store
                .profiles
                .iter()
                .enumerate()
                .map(|(index, profile)| TuiProfileRow {
                    name: sanitize_terminal_text(&profile.name),
                    monitor_count: profile.monitors.len(),
                    selected: Some(index) == self.selected_profile_index,
                })
                .collect(),
            selected_monitor: self
                .selected_monitor_index
                .and_then(|index| self.draft.outputs.get(index))
                .map(|output| sanitize_terminal_text(&output_label(&self.draft, output))),
            selected_profile,
            apply_plan,
            status: sanitize_terminal_text(&self.status),
            input_mode: self.input_mode.clone(),
            dirty: self.dirty,
            move_step: self.move_step(),
        }
    }

    pub fn handle_action(&mut self, action: TuiAction) -> anyhow::Result<TuiEffect> {
        match std::mem::replace(&mut self.input_mode, TuiInputMode::Normal) {
            TuiInputMode::Normal => self.handle_normal_action(action),
            TuiInputMode::SaveAs { mut name } => match action {
                TuiAction::SaveNameChar(character) => {
                    append_profile_name_char(&mut name, character);
                    self.input_mode = TuiInputMode::SaveAs { name };
                    Ok(TuiEffect::None)
                }
                TuiAction::SaveNameBackspace => {
                    name.pop();
                    self.input_mode = TuiInputMode::SaveAs { name };
                    Ok(TuiEffect::None)
                }
                TuiAction::Submit => {
                    let name = name.trim().to_owned();
                    if name.is_empty() {
                        self.status = "Profile name must not be empty".to_owned();
                        self.input_mode = TuiInputMode::SaveAs { name };
                    } else if self.store.has_profile(&name) {
                        self.status = format!("Replace profile `{name}`? press y to confirm");
                        self.input_mode = TuiInputMode::ConfirmReplace { name };
                    } else {
                        self.save_draft_named(&name, false)?;
                    }
                    Ok(TuiEffect::None)
                }
                TuiAction::Cancel | TuiAction::RequestQuit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.status = "Save cancelled".to_owned();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::SaveAs { name };
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::ConfirmReplace { name } => match action {
                TuiAction::Confirm | TuiAction::Submit => {
                    self.save_draft_named(&name, true)?;
                    Ok(TuiEffect::None)
                }
                TuiAction::Cancel | TuiAction::RequestQuit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.status = "Replace cancelled".to_owned();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::ConfirmReplace { name };
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::ConfirmApply => match action {
                TuiAction::Confirm | TuiAction::Submit => {
                    self.input_mode = TuiInputMode::Normal;
                    Ok(TuiEffect::Apply(Box::new(self.draft_apply_plan()?)))
                }
                TuiAction::Cancel | TuiAction::RequestQuit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.status = "Apply cancelled".to_owned();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::ConfirmApply;
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::RenameProfile { mut name } => match action {
                TuiAction::SaveNameChar(character) => {
                    append_profile_name_char(&mut name, character);
                    self.input_mode = TuiInputMode::RenameProfile { name };
                    Ok(TuiEffect::None)
                }
                TuiAction::SaveNameBackspace => {
                    name.pop();
                    self.input_mode = TuiInputMode::RenameProfile { name };
                    Ok(TuiEffect::None)
                }
                TuiAction::Submit => {
                    let name = name.trim().to_owned();
                    if name.is_empty() {
                        self.status = "Profile name must not be empty".to_owned();
                        self.input_mode = TuiInputMode::RenameProfile { name };
                    } else {
                        self.rename_selected_profile(&name)?;
                    }
                    Ok(TuiEffect::None)
                }
                TuiAction::Cancel | TuiAction::RequestQuit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.status = "Rename cancelled".to_owned();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::RenameProfile { name };
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::CopyProfile { source, mut name } => match action {
                TuiAction::SaveNameChar(character) => {
                    append_profile_name_char(&mut name, character);
                    self.input_mode = TuiInputMode::CopyProfile { source, name };
                    Ok(TuiEffect::None)
                }
                TuiAction::SaveNameBackspace => {
                    name.pop();
                    self.input_mode = TuiInputMode::CopyProfile { source, name };
                    Ok(TuiEffect::None)
                }
                TuiAction::Submit => {
                    let name = name.trim().to_owned();
                    if name.is_empty() {
                        self.status = "Profile name must not be empty".to_owned();
                        self.input_mode = TuiInputMode::CopyProfile { source, name };
                    } else if self.store.has_profile(&name) {
                        self.status =
                            format!("Replace profile `{name}` with the copy? press y to confirm");
                        self.input_mode = TuiInputMode::ConfirmCopyReplace { source, name };
                    } else {
                        self.copy_profile_named(&source, &name, false)?;
                    }
                    Ok(TuiEffect::None)
                }
                TuiAction::Cancel | TuiAction::RequestQuit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.status = "Copy cancelled".to_owned();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::CopyProfile { source, name };
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::ConfirmCopyReplace { source, name } => match action {
                TuiAction::Confirm | TuiAction::Submit => {
                    self.copy_profile_named(&source, &name, true)?;
                    Ok(TuiEffect::None)
                }
                TuiAction::Cancel | TuiAction::RequestQuit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.status = "Copy replace cancelled".to_owned();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::ConfirmCopyReplace { source, name };
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::ConfirmDelete { name } => match action {
                TuiAction::Confirm | TuiAction::Submit => {
                    self.delete_profile_named(&name)?;
                    Ok(TuiEffect::None)
                }
                TuiAction::Cancel | TuiAction::RequestQuit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.status = "Delete cancelled".to_owned();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::ConfirmDelete { name };
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::ModeSelect { modes, mut cursor } => match action {
                TuiAction::SelectNextMode => {
                    if !modes.is_empty() {
                        cursor = (cursor + 1) % modes.len();
                    }
                    self.status = mode_select_status(&modes, cursor);
                    self.input_mode = TuiInputMode::ModeSelect { modes, cursor };
                    Ok(TuiEffect::None)
                }
                TuiAction::SelectPreviousMode => {
                    if !modes.is_empty() {
                        cursor = (cursor + modes.len() - 1) % modes.len();
                    }
                    self.status = mode_select_status(&modes, cursor);
                    self.input_mode = TuiInputMode::ModeSelect { modes, cursor };
                    Ok(TuiEffect::None)
                }
                TuiAction::Submit | TuiAction::Confirm => {
                    self.apply_selected_mode(&modes, cursor);
                    self.input_mode = TuiInputMode::Normal;
                    Ok(TuiEffect::None)
                }
                TuiAction::Cancel | TuiAction::RequestQuit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.status = "Mode selection cancelled".to_owned();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::ModeSelect { modes, cursor };
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::SnapTarget {
                direction,
                targets,
                mut cursor,
            } => match action {
                TuiAction::SelectNextMode => {
                    if !targets.is_empty() {
                        cursor = (cursor + 1) % targets.len();
                    }
                    self.status = snap_target_status(self, &targets, cursor);
                    self.input_mode = TuiInputMode::SnapTarget {
                        direction,
                        targets,
                        cursor,
                    };
                    Ok(TuiEffect::None)
                }
                TuiAction::SelectPreviousMode => {
                    if !targets.is_empty() {
                        cursor = (cursor + targets.len() - 1) % targets.len();
                    }
                    self.status = snap_target_status(self, &targets, cursor);
                    self.input_mode = TuiInputMode::SnapTarget {
                        direction,
                        targets,
                        cursor,
                    };
                    Ok(TuiEffect::None)
                }
                TuiAction::Submit | TuiAction::Confirm => {
                    if let (Some(selected_index), Some(target_index)) =
                        (self.selected_monitor_index, targets.get(cursor).copied())
                    {
                        self.apply_snap(selected_index, target_index, direction);
                    }
                    self.input_mode = TuiInputMode::Normal;
                    Ok(TuiEffect::None)
                }
                TuiAction::Cancel | TuiAction::RequestQuit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.status = "Snap cancelled".to_owned();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::SnapTarget {
                        direction,
                        targets,
                        cursor,
                    };
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::ConfirmQuit => match action {
                TuiAction::Confirm | TuiAction::Submit => Ok(TuiEffect::Quit),
                TuiAction::Cancel => {
                    self.input_mode = TuiInputMode::Normal;
                    self.refresh_status();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::ConfirmQuit;
                    Ok(TuiEffect::None)
                }
            },
            TuiInputMode::Help => match action {
                TuiAction::Cancel
                | TuiAction::RequestQuit
                | TuiAction::ShowHelp
                | TuiAction::Submit => {
                    self.input_mode = TuiInputMode::Normal;
                    self.refresh_status();
                    Ok(TuiEffect::None)
                }
                _ => {
                    self.input_mode = TuiInputMode::Help;
                    Ok(TuiEffect::None)
                }
            },
        }
    }

    pub fn draft_apply_plan(&self) -> anyhow::Result<ApplyPlan> {
        plan_apply(&self.draft, &self.monitors)
    }

    pub fn draft_apply_plan_for_monitors(
        &mut self,
        monitors: Vec<MonitorState>,
    ) -> anyhow::Result<ApplyPlan> {
        self.monitors = monitors;
        self.draft_apply_plan()
    }

    pub fn mark_applied(&mut self, monitors: Vec<MonitorState>) {
        self.monitors = monitors;
        self.status = format!("Applied draft `{}`", self.draft.name);
    }

    pub fn mark_rolled_back(&mut self, monitors: Vec<MonitorState>, reason: &str) {
        self.monitors = monitors;
        self.status = format!("Apply rolled back: {reason}; previous monitor layout restored");
    }

    pub fn mark_apply_failed(&mut self, error: &anyhow::Error) {
        self.status = format!("Apply failed: {error:#}");
    }

    pub fn mark_action_failed(&mut self, error: &anyhow::Error) {
        self.status = format!("Action failed: {error:#}");
    }

    pub fn mark_noop(&mut self) {
        self.status = format!("No changes: draft `{}` is already active", self.draft.name);
    }

    pub fn update_monitors(&mut self, monitors: Vec<MonitorState>) {
        self.monitors = monitors;
        if self.selected_profile_index.is_none() && !self.dirty {
            self.draft = draft_from_current(&self.store, &self.monitors);
            self.selected_monitor_index = first_selectable_monitor(&self.draft);
        }
        self.status = format!(
            "Refreshed {} current monitor{}",
            self.monitors.len(),
            if self.monitors.len() == 1 { "" } else { "s" }
        );
    }

    pub fn mark_refresh_failed(&mut self, error: &anyhow::Error) {
        self.status = format!("Refresh failed: {error:#}");
    }

    fn handle_normal_action(&mut self, action: TuiAction) -> anyhow::Result<TuiEffect> {
        match action {
            TuiAction::NextProfile => self.select_relative_profile(1),
            TuiAction::PreviousProfile => self.select_relative_profile(-1),
            TuiAction::SelectProfile(index) => self.select_profile(index),
            TuiAction::NextMonitor => self.select_relative_monitor(1),
            TuiAction::PreviousMonitor => self.select_relative_monitor(-1),
            TuiAction::SelectMonitor(index) => self.select_monitor(index),
            TuiAction::MoveSelected(dx, dy) => self.move_selected(dx, dy),
            TuiAction::NudgeLeft => self.move_selected(-self.move_step(), 0),
            TuiAction::NudgeRight => self.move_selected(self.move_step(), 0),
            TuiAction::NudgeUp => self.move_selected(0, -self.move_step()),
            TuiAction::NudgeDown => self.move_selected(0, self.move_step()),
            TuiAction::SnapSelected(direction) => self.snap_selected(direction),
            TuiAction::ToggleSelected => self.toggle_selected(),
            TuiAction::CycleSelectedMode => self.cycle_selected_mode(),
            TuiAction::SelectNextMode | TuiAction::SelectPreviousMode => {}
            TuiAction::CycleSelectedTransform => self.cycle_selected_transform(),
            TuiAction::AdjustSelectedScale(delta) => self.adjust_selected_scale(delta),
            TuiAction::NewDraft => self.new_draft_from_current(),
            TuiAction::BeginSaveAs => {
                self.input_mode = TuiInputMode::SaveAs {
                    name: self.store.next_generated_name(&self.monitors),
                };
                self.status = "Save as: edit name and press Enter".to_owned();
            }
            TuiAction::BeginRename => self.begin_rename(),
            TuiAction::BeginCopy => self.begin_copy(),
            TuiAction::BeginDelete => self.begin_delete(),
            TuiAction::RefreshMonitors => return Ok(TuiEffect::RefreshMonitors),
            TuiAction::AutoSelect => self.select_automatic_profile(),
            TuiAction::ShowHelp => {
                self.input_mode = TuiInputMode::Help;
                self.status = "Help".to_owned();
            }
            TuiAction::RequestQuit => {
                if self.dirty {
                    self.input_mode = TuiInputMode::ConfirmQuit;
                    self.status = "Discard unsaved changes? press y to confirm".to_owned();
                } else {
                    return Ok(TuiEffect::Quit);
                }
            }
            TuiAction::ApplyDraft => match self.request_apply() {
                Ok(effect) => return Ok(effect),
                Err(error) => self.status = format!("Draft cannot be applied: {error:#}"),
            },
            _ => {}
        }

        Ok(TuiEffect::None)
    }

    fn new_draft_from_current(&mut self) {
        if self.dirty {
            self.status = "Save or discard the modified draft before creating another".to_owned();
            return;
        }
        self.selected_profile_index = None;
        self.draft = draft_from_current(&self.store, &self.monitors);
        self.selected_monitor_index = first_selectable_monitor(&self.draft);
        self.dirty = true;
        self.input_mode = TuiInputMode::Normal;
        self.status = format!("New draft `{}` from current monitors", self.draft.name);
    }

    fn select_profile(&mut self, index: usize) {
        if self.dirty {
            self.status = "Save the modified draft or quit before switching profiles".to_owned();
            return;
        }
        if let Some(profile) = self.store.profiles.get(index).cloned() {
            self.selected_profile_index = Some(index);
            self.draft = profile;
            self.selected_monitor_index = first_selectable_monitor(&self.draft);
            self.dirty = false;
            self.input_mode = TuiInputMode::Normal;
            self.refresh_status();
        }
    }

    fn select_relative_profile(&mut self, delta: isize) {
        if self.store.profiles.is_empty() {
            return;
        }

        let current = self.selected_profile_index.unwrap_or(0);
        let len = self.store.profiles.len() as isize;
        let next = (current as isize + delta).rem_euclid(len) as usize;
        self.select_profile(next);
    }

    fn select_monitor(&mut self, index: usize) {
        if index < self.draft.outputs.len() {
            self.selected_monitor_index = Some(index);
            self.refresh_status();
        }
    }

    fn select_relative_monitor(&mut self, delta: isize) {
        if self.draft.outputs.is_empty() {
            return;
        }

        let current = self.selected_monitor_index.unwrap_or(0);
        let len = self.draft.outputs.len() as isize;
        self.select_monitor((current as isize + delta).rem_euclid(len) as usize);
    }

    fn move_selected(&mut self, dx: i32, dy: i32) {
        let Some(index) = self.selected_monitor_index else {
            return;
        };
        let label = self.output_label_by_index(index);
        if let Some(output) = self.draft.outputs.get_mut(index) {
            if move_output(output, dx, dy) {
                self.mark_dirty(format!("Moved {label}"));
            }
        }
    }

    fn snap_selected(&mut self, direction: SnapDirection) {
        let Some(selected_index) = self.selected_monitor_index else {
            return;
        };
        let targets = self.snap_target_indexes(selected_index);
        if targets.is_empty() {
            self.status = "No enabled snap target".to_owned();
            return;
        }

        if targets.len() > 1 {
            self.status = snap_target_status(self, &targets, 0);
            self.input_mode = TuiInputMode::SnapTarget {
                direction,
                targets,
                cursor: 0,
            };
            return;
        }

        self.apply_snap(selected_index, targets[0], direction);
    }

    fn snap_target_indexes(&self, selected_index: usize) -> Vec<usize> {
        self.draft
            .outputs
            .iter()
            .enumerate()
            .filter_map(|(index, output)| {
                if index != selected_index && output.enabled {
                    Some(index)
                } else {
                    None
                }
            })
            .collect()
    }

    fn apply_snap(&mut self, selected_index: usize, target_index: usize, direction: SnapDirection) {
        if snap_output_with_monitors(
            &mut self.draft.outputs,
            &self.monitors,
            selected_index,
            target_index,
            direction,
        ) {
            self.mark_dirty("Snapped selected monitor".to_owned());
        }
    }

    fn toggle_selected(&mut self) {
        let Some(index) = self.selected_monitor_index else {
            return;
        };
        let label = self.output_label_by_index(index);
        if let Some(output) = self.draft.outputs.get_mut(index) {
            output.enabled = !output.enabled;
            self.mark_dirty(format!("Toggled {label}"));
        }
    }

    fn cycle_selected_mode(&mut self) {
        let Some(index) = self.selected_monitor_index else {
            return;
        };
        let Some(output) = self.draft.outputs.get(index) else {
            return;
        };
        let modes = self
            .draft
            .monitors
            .iter()
            .find(|monitor| monitor.id == output.monitor_id)
            .and_then(|monitor| {
                resolve_monitor_matches(std::slice::from_ref(monitor), &self.monitors)
                    .into_iter()
                    .next()
                    .flatten()
            })
            .filter(|resolved| !resolved.ambiguous)
            .and_then(|resolved| self.monitors.get(resolved.current_index))
            .map(|monitor| monitor.available_modes.clone())
            .unwrap_or_default();
        if modes.is_empty() {
            self.status = "Selected monitor has no advertised modes".to_owned();
            return;
        }

        let cursor = modes
            .iter()
            .map(|mode| normalize_mode(mode))
            .position(|mode| mode == output.mode)
            .unwrap_or_default();
        self.status = mode_select_status(&modes, cursor);
        self.input_mode = TuiInputMode::ModeSelect { modes, cursor };
    }

    fn cycle_selected_transform(&mut self) {
        let Some(index) = self.selected_monitor_index else {
            return;
        };
        if let Some(output) = self.draft.outputs.get_mut(index) {
            output.transform = (output.transform + 1).rem_euclid(8);
            self.mark_dirty("Changed selected monitor transform".to_owned());
        }
    }

    fn adjust_selected_scale(&mut self, delta: f64) {
        let Some(index) = self.selected_monitor_index else {
            return;
        };
        if let Some(output) = self.draft.outputs.get_mut(index) {
            output.scale = (output.scale + delta).clamp(MIN_SCALE, MAX_SCALE);
            self.mark_dirty("Changed selected monitor scale".to_owned());
        }
    }

    fn save_draft_named(&mut self, name: &str, replace: bool) -> anyhow::Result<()> {
        let mut profile = self.draft.clone();
        profile.name = name.to_owned();
        let (store, saved_name) = ProfileStore::mutate_atomic_with_initial(
            self.paths.profile_store_path(),
            Some(&self.store),
            move |candidate| candidate.save_profile(profile, replace),
        )
        .with_context(|| {
            format!(
                "failed to save profile store at {}",
                self.paths.profile_store_path().display()
            )
        })?;
        self.store = store;
        self.selected_profile_index = self
            .store
            .profiles
            .iter()
            .position(|profile| profile.name == saved_name);
        self.draft = self
            .selected_profile_index
            .and_then(|index| self.store.profiles.get(index).cloned())
            .ok_or_else(|| anyhow::anyhow!("saved profile `{saved_name}` was not reloaded"))?;
        self.selected_monitor_index = first_selectable_monitor(&self.draft);
        self.dirty = false;
        self.input_mode = TuiInputMode::Normal;
        self.status = format!("Saved profile `{saved_name}`");
        Ok(())
    }

    fn begin_rename(&mut self) {
        if self.dirty {
            self.status = "Save or discard the modified draft before renaming".to_owned();
            return;
        }
        let Some(name) = self
            .selected_profile_index
            .and_then(|index| self.store.profiles.get(index))
            .map(|profile| profile.name.clone())
        else {
            self.status = "Select a saved profile before renaming".to_owned();
            return;
        };

        self.input_mode = TuiInputMode::RenameProfile { name };
        self.status = "Rename profile: edit name and press Enter".to_owned();
    }

    fn begin_delete(&mut self) {
        if self.dirty {
            self.status = "Save or discard the modified draft before deleting".to_owned();
            return;
        }
        let Some(name) = self
            .selected_profile_index
            .and_then(|index| self.store.profiles.get(index))
            .map(|profile| profile.name.clone())
        else {
            self.status = "Select a saved profile before deleting".to_owned();
            return;
        };

        self.input_mode = TuiInputMode::ConfirmDelete { name: name.clone() };
        self.status = format!("Delete profile `{name}`? press y to confirm");
    }

    fn begin_copy(&mut self) {
        if self.dirty {
            self.status = "Save or discard the modified draft before copying".to_owned();
            return;
        }
        let Some(source) = self
            .selected_profile_index
            .and_then(|index| self.store.profiles.get(index))
            .map(|profile| profile.name.clone())
        else {
            self.status = "Select a saved profile before copying".to_owned();
            return;
        };
        let name = next_copy_name(&self.store, &source);
        self.input_mode = TuiInputMode::CopyProfile { source, name };
        self.status = "Copy profile: edit destination and press Enter".to_owned();
    }

    fn rename_selected_profile(&mut self, new_name: &str) -> anyhow::Result<()> {
        let old_name = self
            .selected_profile_index
            .and_then(|index| self.store.profiles.get(index))
            .map(|profile| profile.name.clone())
            .ok_or_else(|| anyhow::anyhow!("select a saved profile before renaming"))?;
        let (store, ()) = ProfileStore::mutate_atomic_with_initial(
            self.paths.profile_store_path(),
            Some(&self.store),
            |candidate| candidate.rename_profile(&old_name, new_name),
        )
        .with_context(|| {
            format!(
                "failed to save profile store at {}",
                self.paths.profile_store_path().display()
            )
        })?;
        self.store = store;
        self.selected_profile_index = self
            .store
            .profiles
            .iter()
            .position(|profile| profile.name == new_name);
        self.draft = self
            .selected_profile_index
            .and_then(|index| self.store.profiles.get(index).cloned())
            .ok_or_else(|| anyhow::anyhow!("renamed profile `{new_name}` was not reloaded"))?;
        self.selected_monitor_index = first_selectable_monitor(&self.draft);
        self.dirty = false;
        self.input_mode = TuiInputMode::Normal;
        self.status = format!("Renamed profile `{old_name}` to `{new_name}`");
        Ok(())
    }

    fn delete_profile_named(&mut self, name: &str) -> anyhow::Result<()> {
        let (store, ()) = ProfileStore::mutate_atomic_with_initial(
            self.paths.profile_store_path(),
            Some(&self.store),
            |candidate| candidate.delete_profile(name).map(|_| ()),
        )
        .with_context(|| {
            format!(
                "failed to save profile store at {}",
                self.paths.profile_store_path().display()
            )
        })?;
        self.store = store;
        self.selected_profile_index = if self.store.profiles.is_empty() {
            None
        } else {
            Some(0)
        };
        self.draft = self
            .selected_profile_index
            .and_then(|index| self.store.profiles.get(index).cloned())
            .unwrap_or_else(|| draft_from_current(&self.store, &self.monitors));
        self.selected_monitor_index = first_selectable_monitor(&self.draft);
        self.dirty = false;
        self.input_mode = TuiInputMode::Normal;
        self.status = format!("Deleted profile `{name}`");
        Ok(())
    }

    fn copy_profile_named(
        &mut self,
        source: &str,
        destination: &str,
        replace: bool,
    ) -> anyhow::Result<()> {
        let (store, ()) = ProfileStore::mutate_atomic_with_initial(
            self.paths.profile_store_path(),
            Some(&self.store),
            |candidate| candidate.copy_profile(source, destination, replace),
        )
        .with_context(|| {
            format!(
                "failed to save profile store at {}",
                self.paths.profile_store_path().display()
            )
        })?;
        self.store = store;
        self.selected_profile_index = self
            .store
            .profiles
            .iter()
            .position(|profile| profile.name == destination);
        self.draft = self
            .selected_profile_index
            .and_then(|index| self.store.profiles.get(index).cloned())
            .ok_or_else(|| anyhow::anyhow!("copied profile `{destination}` was not reloaded"))?;
        self.selected_monitor_index = first_selectable_monitor(&self.draft);
        self.dirty = false;
        self.input_mode = TuiInputMode::Normal;
        self.status = format!("Copied profile `{source}` to `{destination}`");
        Ok(())
    }

    fn select_automatic_profile(&mut self) {
        if self.dirty {
            self.status =
                "Automatic selection blocked: save or discard the modified draft first".to_owned();
            return;
        }
        let best_match = best_profile_match(&self.store, &self.monitors);
        let decision = decide_auto_apply(
            &self.store,
            &best_match,
            self.config.fallback_profile.as_deref(),
        );
        if let Some(profile_name) = decision.profile_name() {
            if let Some(index) = self
                .store
                .profiles
                .iter()
                .position(|profile| profile.name == profile_name)
            {
                self.select_profile(index);
            }
        }
        self.status =
            format_auto_apply_decision(&decision, "Automatic selection").replace('\n', " | ");
    }

    fn apply_selected_mode(&mut self, modes: &[String], cursor: usize) {
        let Some(index) = self.selected_monitor_index else {
            return;
        };
        let Some(mode) = modes.get(cursor) else {
            return;
        };
        if let Some(output) = self.draft.outputs.get_mut(index) {
            output.mode = normalize_mode(mode);
            self.mark_dirty("Changed selected monitor mode".to_owned());
        }
    }

    fn request_apply(&mut self) -> anyhow::Result<TuiEffect> {
        let plan = self.draft_apply_plan()?;
        ensure_plan_safe_to_apply(&plan)?;
        if plan.warnings.is_empty() {
            return Ok(TuiEffect::Apply(Box::new(plan)));
        }

        self.input_mode = TuiInputMode::ConfirmApply;
        self.status = format!(
            "Apply has {} warning{}; press y to confirm",
            plan.warnings.len(),
            if plan.warnings.len() == 1 { "" } else { "s" }
        );
        Ok(TuiEffect::None)
    }

    fn mark_dirty(&mut self, status: String) {
        self.dirty = true;
        self.status = status;
    }

    fn refresh_status(&mut self) {
        self.status = match self.draft_apply_plan() {
            Ok(plan) if plan.warnings.is_empty() => {
                format!("Editing profile `{}`", self.draft.name)
            }
            Ok(plan) => format!(
                "Editing profile `{}` with {} warning{}",
                self.draft.name,
                plan.warnings.len(),
                if plan.warnings.len() == 1 { "" } else { "s" }
            ),
            Err(error) => format!("Draft cannot be previewed: {error}"),
        };
    }

    fn output_label_by_index(&self, index: usize) -> String {
        self.draft
            .outputs
            .get(index)
            .map(|output| output_label(&self.draft, output))
            .unwrap_or_else(|| "selected monitor".to_owned())
    }

    fn move_step(&self) -> i32 {
        self.config.tui_move_step.clamp(1, 10_000)
    }
}

pub fn initial_model(
    store: &ProfileStore,
    monitors: &[MonitorState],
    fallback_profile: Option<&str>,
) -> anyhow::Result<TuiModel> {
    let config = AppConfig {
        fallback_profile: fallback_profile.map(str::to_owned),
        ..AppConfig::default()
    };
    let app = TuiApp::new(
        store.clone(),
        ConfigPaths::from_config_dir("/tmp/hyprdisjust-tui-snapshot")?,
        config,
        monitors.to_vec(),
    )?;
    let mut model = app.view_model();
    model.status = snapshot_status(&model);
    Ok(model)
}

fn draft_from_current(store: &ProfileStore, monitors: &[MonitorState]) -> Profile {
    Profile::from_monitors(
        store.next_generated_name(monitors),
        monitors,
        String::new(),
        String::new(),
    )
}

fn next_copy_name(store: &ProfileStore, source: &str) -> String {
    store.next_available_name(&format!("{source}-copy"))
}

fn append_profile_name_char(name: &mut String, character: char) {
    if character.is_control()
        || name.len().saturating_add(character.len_utf8()) > MAX_PROFILE_NAME_BYTES
    {
        return;
    }
    name.push(character);
}

fn first_selectable_monitor(profile: &Profile) -> Option<usize> {
    if profile.outputs.is_empty() {
        None
    } else {
        Some(0)
    }
}

fn monitor_row(profile: &Profile, output: &ProfileOutput, selected: bool) -> TuiMonitorRow {
    TuiMonitorRow {
        output_name: sanitize_terminal_text(&output_label(profile, output)),
        id: sanitize_terminal_text(&output.monitor_id),
        mode: sanitize_terminal_text(&output.mode),
        position: format!("{}x{}", output.x, output.y),
        scale: output.scale,
        transform: output.transform,
        enabled: output.enabled,
        selected,
    }
}

fn output_label(profile: &Profile, output: &ProfileOutput) -> String {
    profile
        .monitors
        .iter()
        .find(|monitor| monitor.id == output.monitor_id)
        .map(profile_monitor_label)
        .unwrap_or_else(|| output.monitor_id.clone())
}

fn profile_monitor_label(monitor: &ProfileMonitor) -> String {
    if monitor.name_hint.trim().is_empty() {
        monitor.id.clone()
    } else {
        monitor.name_hint.clone()
    }
}

fn normalize_mode(mode: &str) -> String {
    mode.strip_suffix("Hz").unwrap_or(mode).to_owned()
}

fn mode_select_status(modes: &[String], cursor: usize) -> String {
    modes
        .get(cursor)
        .map(|mode| format!("Mode: {}  Enter select  Esc cancel", normalize_mode(mode)))
        .unwrap_or_else(|| "No modes available".to_owned())
}

fn snap_target_status(app: &TuiApp, targets: &[usize], cursor: usize) -> String {
    targets
        .get(cursor)
        .map(|target| {
            format!(
                "Snap target: {}  Enter select  Esc cancel",
                app.output_label_by_index(*target)
            )
        })
        .unwrap_or_else(|| "No enabled snap target".to_owned())
}

pub fn require_draft_plan(app: &TuiApp) -> anyhow::Result<ApplyPlan> {
    let plan = app.draft_apply_plan()?;
    if plan.rules.is_empty() {
        bail!("draft has no monitor rules");
    }
    Ok(plan)
}

fn snapshot_status(model: &TuiModel) -> String {
    match (&model.selected_profile, &model.apply_plan) {
        (Some(profile), Some(plan)) if plan.warnings.is_empty() => {
            format!("Previewing profile `{profile}`")
        }
        (Some(profile), Some(plan)) => format!(
            "Previewing profile `{profile}` with {} warning{}",
            plan.warnings.len(),
            if plan.warnings.len() == 1 { "" } else { "s" }
        ),
        (Some(profile), None) => format!("Selected profile `{profile}` cannot be previewed"),
        (None, _) => "No profiles saved yet".to_owned(),
    }
}
