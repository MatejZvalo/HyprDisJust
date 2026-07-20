use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::atomic::{
    atomic_write, ensure_private_directory, open_private_lock, read_limited, PRIVATE_FILE_MODE,
};
use crate::hyprland::monitor::MonitorState;
use crate::profile::validation::{
    validate_profile, validate_profile_name, validate_store, MAX_PROFILE_STORE_BYTES,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileStore {
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ProfileMonitor {
    pub id: String,
    pub name_hint: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    #[serde(default)]
    pub physical_width: i32,
    #[serde(default)]
    pub physical_height: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        if let Err(error) = fs::symlink_metadata(path) {
            if error.kind() == ErrorKind::NotFound {
                return Ok(Self::default());
            }
            return Err(error)
                .with_context(|| format!("failed to inspect profile store at {}", path.display()));
        }
        ensure_private_directory(path.parent().unwrap_or_else(|| Path::new(".")))?;
        let contents = read_limited(path, MAX_PROFILE_STORE_BYTES, "profile store")?;
        let contents = std::str::from_utf8(&contents)
            .with_context(|| format!("profile store at {} is not valid UTF-8", path.display()))?;
        let store: Self = toml::from_str(contents)
            .with_context(|| format!("failed to parse profile store at {}", path.display()))?;
        validate_store(&store)
            .with_context(|| format!("invalid profile store at {}", path.display()))?;
        Ok(store)
    }

    pub fn save_atomic(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        validate_store(self).context("refusing to save invalid profile store")?;
        let contents = toml::to_string_pretty(self).context("failed to serialize profile store")?;
        if contents.len() as u64 > MAX_PROFILE_STORE_BYTES {
            bail!("refusing to save profile store larger than {MAX_PROFILE_STORE_BYTES} bytes");
        }
        atomic_write(path, contents.as_bytes(), PRIVATE_FILE_MODE)
            .with_context(|| format!("failed to save profile store at {}", path.display()))
    }

    pub fn mutate_atomic<R>(
        path: impl AsRef<Path>,
        mutation: impl FnOnce(&mut Self) -> anyhow::Result<R>,
    ) -> anyhow::Result<(Self, R)> {
        Self::mutate_atomic_with_initial(path, None, mutation)
    }

    pub fn mutate_atomic_with_initial<R>(
        path: impl AsRef<Path>,
        initial: Option<&Self>,
        mutation: impl FnOnce(&mut Self) -> anyhow::Result<R>,
    ) -> anyhow::Result<(Self, R)> {
        let path = path.as_ref();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        ensure_private_directory(parent)?;
        let lock_path = path.with_file_name("profiles.lock");
        let lock_file = open_private_lock(&lock_path, "profile lock")?;
        lock_file
            .lock()
            .with_context(|| format!("failed to lock profile store {}", path.display()))?;

        let mut store = if path.exists() {
            Self::load(path)?
        } else {
            initial.cloned().unwrap_or_default()
        };
        let result = mutation(&mut store)?;
        validate_store(&store).context("profile mutation produced an invalid store")?;
        store.save_atomic(path)?;
        Ok((store, result))
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
        validate_profile(&profile)?;

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

        self.next_available_name(&base)
    }

    pub fn next_available_name(&self, base: &str) -> String {
        if !self.has_profile(base) {
            return base.to_owned();
        }

        let mut suffix = 2_u128;
        loop {
            let candidate = format!("{base}-{suffix}");
            if !self.has_profile(&candidate) {
                return candidate;
            }
            suffix = suffix.saturating_add(1);
        }
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
        if source_name == destination_name {
            bail!(
                "destination profile `{destination_name}` already exists as the source; choose a different name"
            );
        }
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
            physical_width: monitor.physical_width,
            physical_height: monitor.physical_height,
        }
    }
}

impl From<&MonitorState> for ProfileOutput {
    fn from(monitor: &MonitorState) -> Self {
        let mode = if monitor.width > 0 && monitor.height > 0 {
            format_mode(monitor.width, monitor.height, monitor.refresh_rate)
        } else {
            monitor
                .available_modes
                .first()
                .map(|mode| mode.strip_suffix("Hz").unwrap_or(mode).to_owned())
                .unwrap_or_else(|| "preferred".to_owned())
        };
        Self {
            monitor_id: monitor.id.clone(),
            enabled: monitor.enabled,
            mode,
            x: monitor.x,
            y: monitor.y,
            scale: monitor.scale,
            transform: monitor.transform,
        }
    }
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
