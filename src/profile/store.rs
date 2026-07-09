use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::hyprland::monitor::MonitorState;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileStore {
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub monitors: Vec<ProfileMonitor>,
    #[serde(default)]
    pub outputs: Vec<ProfileOutput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileMonitor {
    pub id: String,
    pub name_hint: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileOutput {
    pub monitor_id: String,
    pub enabled: bool,
    pub mode: String,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: i32,
}

impl ProfileStore {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read profile store at {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("failed to parse profile store at {}", path.display()))
    }

    pub fn save_atomic(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;

        let contents = toml::to_string_pretty(self).context("failed to serialize profile store")?;
        let temp_path = unique_temp_path(path);

        let write_result = (|| -> anyhow::Result<()> {
            let mut temp_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .with_context(|| {
                    format!(
                        "failed to create temporary profile store {}",
                        temp_path.display()
                    )
                })?;
            temp_file
                .write_all(contents.as_bytes())
                .with_context(|| format!("failed to write {}", temp_path.display()))?;
            temp_file
                .sync_all()
                .with_context(|| format!("failed to sync {}", temp_path.display()))?;
            fs::rename(&temp_path, path).with_context(|| {
                format!(
                    "failed to replace profile store {} with {}",
                    path.display(),
                    temp_path.display()
                )
            })?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }

        write_result
    }

    pub fn save_current_profile(
        &mut self,
        requested_name: Option<&str>,
        monitors: &[MonitorState],
        replace: bool,
    ) -> anyhow::Result<String> {
        let name = match requested_name {
            Some(name) => validate_profile_name(name)?.to_owned(),
            None => self.next_generated_name(monitors),
        };

        let existing_index = self
            .profiles
            .iter()
            .position(|profile| profile.name == name);
        if existing_index.is_some() && !replace {
            bail!("profile `{name}` already exists; pass --replace to overwrite it");
        }

        let now = timestamp_now();
        let created_at = existing_index
            .and_then(|index| self.profiles.get(index))
            .map(|profile| profile.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let profile = Profile::from_monitors(name.clone(), monitors, created_at, now);

        if let Some(index) = existing_index {
            self.profiles[index] = profile;
        } else {
            self.profiles.push(profile);
        }

        self.profiles
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(name)
    }

    pub fn save_profile(&mut self, mut profile: Profile, replace: bool) -> anyhow::Result<String> {
        let name = validate_profile_name(&profile.name)?.to_owned();
        profile.name = name.clone();

        let existing_index = self
            .profiles
            .iter()
            .position(|stored_profile| stored_profile.name == name);
        if existing_index.is_some() && !replace {
            bail!("profile `{name}` already exists; pass --replace to overwrite it");
        }

        let now = timestamp_now();
        profile.updated_at = now.clone();
        if let Some(index) = existing_index {
            profile.created_at = self.profiles[index].created_at.clone();
            self.profiles[index] = profile;
        } else {
            if profile.created_at.trim().is_empty() {
                profile.created_at = now;
            }
            self.profiles.push(profile);
        }

        self.profiles
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(name)
    }

    pub fn next_generated_name(&self, monitors: &[MonitorState]) -> String {
        let enabled_count = monitors.iter().filter(|monitor| monitor.enabled).count();
        let monitor_count = if enabled_count == 0 {
            monitors.len()
        } else {
            enabled_count
        };
        let plural = if monitor_count == 1 {
            "monitor"
        } else {
            "monitors"
        };
        let prefix = if monitor_count <= 1 { "laptop" } else { "desk" };
        let base = format!("{prefix}-{monitor_count}-{plural}");

        if !self.has_profile(&base) {
            return base;
        }

        for suffix in 2.. {
            let candidate = format!("{base}-{suffix}");
            if !self.has_profile(&candidate) {
                return candidate;
            }
        }

        unreachable!("infinite suffix search should always find an available profile name")
    }

    pub fn has_profile(&self, name: &str) -> bool {
        self.profiles.iter().any(|profile| profile.name == name)
    }

    pub fn rename_profile(&mut self, old_name: &str, new_name: &str) -> anyhow::Result<()> {
        let old_name = validate_profile_name(old_name)?;
        let new_name = validate_profile_name(new_name)?;
        if old_name == new_name {
            bail!("profile `{old_name}` already has that name");
        }
        if self.has_profile(new_name) {
            bail!("profile `{new_name}` already exists");
        }

        let Some(index) = self
            .profiles
            .iter()
            .position(|profile| profile.name == old_name)
        else {
            bail!("profile `{old_name}` does not exist");
        };

        self.profiles[index].name = new_name.to_owned();
        self.profiles[index].updated_at = timestamp_now();
        self.profiles
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(())
    }

    pub fn delete_profile(&mut self, name: &str) -> anyhow::Result<Profile> {
        let name = validate_profile_name(name)?;
        let Some(index) = self
            .profiles
            .iter()
            .position(|profile| profile.name == name)
        else {
            bail!("profile `{name}` does not exist");
        };

        Ok(self.profiles.remove(index))
    }

    pub fn copy_profile(
        &mut self,
        source_name: &str,
        destination_name: &str,
        replace: bool,
    ) -> anyhow::Result<()> {
        let source_name = validate_profile_name(source_name)?;
        let destination_name = validate_profile_name(destination_name)?;
        let Some(source) = self
            .profiles
            .iter()
            .find(|profile| profile.name == source_name)
            .cloned()
        else {
            bail!("profile `{source_name}` does not exist");
        };

        let now = timestamp_now();
        let mut profile = source;
        profile.name = destination_name.to_owned();
        profile.created_at = now.clone();
        profile.updated_at = now;
        self.save_profile(profile, replace)?;
        Ok(())
    }
}

impl Profile {
    pub fn from_monitors(
        name: String,
        monitors: &[MonitorState],
        created_at: String,
        updated_at: String,
    ) -> Self {
        Self {
            name,
            created_at,
            updated_at,
            monitors: monitors.iter().map(ProfileMonitor::from).collect(),
            outputs: monitors.iter().map(ProfileOutput::from).collect(),
        }
    }
}

impl From<&MonitorState> for ProfileMonitor {
    fn from(monitor: &MonitorState) -> Self {
        Self {
            id: monitor.id.clone(),
            name_hint: monitor.output_name.clone(),
            description: monitor.description.clone(),
            make: monitor.make.clone(),
            model: monitor.model.clone(),
            serial: monitor.serial.clone(),
        }
    }
}

impl From<&MonitorState> for ProfileOutput {
    fn from(monitor: &MonitorState) -> Self {
        Self {
            monitor_id: monitor.id.clone(),
            enabled: monitor.enabled,
            mode: format_mode(monitor.width, monitor.height, monitor.refresh_rate),
            x: monitor.x,
            y: monitor.y,
            scale: monitor.scale,
            transform: monitor.transform,
        }
    }
}

fn validate_profile_name(name: &str) -> anyhow::Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("profile name must not be empty");
    }

    Ok(name)
}

fn format_mode(width: i32, height: i32, refresh_rate: f64) -> String {
    format!("{}x{}@{}", width, height, format_number(refresh_rate))
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

fn timestamp_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("unix:{seconds}")
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("profiles.toml");
    let process_id = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.{process_id}.{nanos}.tmp"))
}
