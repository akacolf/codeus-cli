use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub const DEFAULT_API_BASE: &str = "https://api.quantumclub.sh/v1";
pub const DEFAULT_MODEL: &str = "sonnet-4.6";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub api_base: Option<String>,
    pub model: Option<String>,
    pub session_token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_expires_at: Option<u64>,
    pub theme: Option<String>,
    pub font: Option<String>,
    pub approved_command_prefixes: Option<Vec<Vec<String>>>,
    pub onboarded: Option<bool>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        let read_path = if path.exists() {
            path
        } else if let Some(legacy) = legacy_config_path().filter(|legacy| legacy.exists()) {
            legacy
        } else {
            return Ok(Self::default());
        };

        let raw = fs::read_to_string(&read_path)
            .with_context(|| format!("failed to read config at {}", read_path.display()))?;
        toml::from_str(&raw).with_context(|| format!("failed to parse {}", read_path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let raw = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&path, perms)?;
        }

        Ok(())
    }
}

pub fn config_path() -> Result<PathBuf> {
    let base = dirs::home_dir().context("could not locate user home directory")?;
    Ok(base.join(".config").join("codeus").join("config.toml"))
}

fn legacy_config_path() -> Option<PathBuf> {
    let legacy = dirs::config_dir()?.join("codeus").join("config.toml");
    let primary = config_path().ok()?;

    if legacy == primary {
        None
    } else {
        Some(legacy)
    }
}
