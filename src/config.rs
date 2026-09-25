use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::github::RepositoryName;

const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub schema_version: u32,
    pub default_repository: RepositoryName,
}

impl AppConfig {
    pub fn new(default_repository: RepositoryName) -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            default_repository,
        }
    }

    pub fn load(path: &Path) -> Result<Option<Self>> {
        let contents = match fs::read(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read configuration {}", path.display()));
            }
        };
        let config: Self = serde_json::from_slice(&contents)
            .with_context(|| format!("invalid configuration {}", path.display()))?;
        if config.schema_version != CONFIG_SCHEMA_VERSION {
            bail!(
                "unsupported configuration schema version {}",
                config.schema_version
            );
        }
        Ok(Some(config))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .context("configuration path has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create configuration directory {}",
                parent.display()
            )
        })?;
        let temporary = path.with_extension("json.tmp");
        let mut contents = serde_json::to_vec_pretty(self)?;
        contents.push(b'\n');
        fs::write(&temporary, contents)
            .with_context(|| format!("failed to write configuration {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("failed to install configuration {}", path.display()))
    }
}

pub fn config_path() -> Result<PathBuf> {
    if let Some(base) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(base).join("work-tracker/config.json"));
    }
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home).join(".config/work-tracker/config.json"));
    }
    Ok(env::current_dir()
        .context("failed to determine current directory")?
        .join("work-tracker-config.json"))
}
