use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct AppConfig {
    pub fallback_profile: Option<String>,
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

        toml::from_str(&contents)
            .with_context(|| format!("failed to parse config at {}", path.display()))
    }
}
