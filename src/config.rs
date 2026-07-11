use std::env;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use directories::BaseDirs;
use serde::Deserialize;

const CONFIG_DIR_ENV: &str = "HYPRDISJUST_CONFIG_DIR";

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
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create generated config directory {}",
            parent.display()
        )
    })?;

    let temp_path = unique_temp_path(path);
    let write_result = (|| -> anyhow::Result<()> {
        let mut temp_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .with_context(|| {
                format!(
                    "failed to create temporary generated file {}",
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
                "failed to replace generated file {} with {}",
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

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read config at {}", path.display()));
            }
        };

        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config at {}", path.display()))?;
        if !(1..=10_000).contains(&config.tui_move_step) {
            bail!("tui_move_step must be between 1 and 10000");
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

fn unique_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("generated");
    let process_id = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.{process_id}.{nanos}.tmp"))
}
