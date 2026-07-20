use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use directories::BaseDirs;
use serde::Deserialize;

use crate::atomic::{atomic_write, ensure_private_directory, read_limited, PRIVATE_FILE_MODE};
use crate::profile::validation::validate_profile_name;

const CONFIG_DIR_ENV: &str = "HYPRDISJUST_CONFIG_DIR";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub profile_store: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub fallback_profile: Option<String>,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default)]
    pub apply_on_start: bool,
    #[serde(default = "default_tui_move_step")]
    pub tui_move_step: i32,
}

impl ConfigPaths {
    pub fn resolve() -> anyhow::Result<Self> {
        if let Some(config_dir) = env::var_os(CONFIG_DIR_ENV) {
            return Self::from_config_dir(config_dir);
        }

        let base_dirs = BaseDirs::new()
            .ok_or_else(|| anyhow!("could not determine the user config directory"))?;
        Self::from_config_dir(base_dirs.config_dir().join("hyprdisjust"))
    }

    pub fn from_config_dir(config_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let config_dir = config_dir.into();
        if config_dir.as_os_str().is_empty() {
            bail!("{CONFIG_DIR_ENV} must not be empty");
        }

        Ok(Self {
            config_file: config_dir.join("config.toml"),
            profile_store: config_dir.join("profiles.toml"),
            config_dir,
        })
    }

    pub fn profile_store_path(&self) -> &Path {
        &self.profile_store
    }

    pub fn config_file_path(&self) -> &Path {
        &self.config_file
    }

    pub fn generated_dir_path(&self) -> PathBuf {
        self.config_dir.join("generated")
    }

    pub fn generated_monitors_lua_path(&self) -> PathBuf {
        self.generated_dir_path().join("monitors.lua")
    }
}

pub fn write_generated_file(path: impl AsRef<Path>, contents: &str) -> anyhow::Result<()> {
    let path = path.as_ref();
    atomic_write(path, contents.as_bytes(), PRIVATE_FILE_MODE)
        .with_context(|| format!("failed to write generated file {}", path.display()))
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if let Err(error) = fs::symlink_metadata(path) {
            if error.kind() == ErrorKind::NotFound {
                return Ok(Self::default());
            }
            return Err(error)
                .with_context(|| format!("failed to inspect config at {}", path.display()));
        }
        ensure_private_directory(path.parent().unwrap_or_else(|| Path::new(".")))?;
        let contents = read_limited(path, MAX_CONFIG_BYTES, "config")?;
        let contents = std::str::from_utf8(&contents)
            .with_context(|| format!("config at {} is not valid UTF-8", path.display()))?;

        let config: Self = toml::from_str(contents)
            .with_context(|| format!("failed to parse config at {}", path.display()))?;
        if !(1..=10_000).contains(&config.tui_move_step) {
            bail!("tui_move_step must be between 1 and 10000");
        }
        if config.debounce_ms > 60_000 {
            bail!("debounce_ms must be between 0 and 60000 (zero disables debounce)");
        }
        if let Some(fallback_profile) = config.fallback_profile.as_deref() {
            validate_profile_name(fallback_profile).context("invalid fallback_profile")?;
        }
        Ok(config)
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            fallback_profile: None,
            debounce_ms: default_debounce_ms(),
            apply_on_start: false,
            tui_move_step: default_tui_move_step(),
        }
    }
}

fn default_debounce_ms() -> u64 {
    900
}

fn default_tui_move_step() -> i32 {
    20
}
