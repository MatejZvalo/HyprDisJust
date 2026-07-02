use std::env;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use directories::BaseDirs;

const CONFIG_DIR_ENV: &str = "HYPRDISJUST_CONFIG_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub config_dir: PathBuf,
    pub profile_store: PathBuf,
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
            profile_store: config_dir.join("profiles.toml"),
            config_dir,
        })
    }

    pub fn profile_store_path(&self) -> &Path {
        &self.profile_store
    }
}
