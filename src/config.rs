use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::PathBuf;

pub const DEFAULT_API_BASE: &str = "https://api.quantumclub.sh/v1";
pub const DEFAULT_MODEL: &str = "sonnet-4.6";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum PermissionMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
}

impl PermissionMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "read-only" | "readonly" | "read_only" => Ok(Self::ReadOnly),
            "workspace-write" | "workspace_write" | "workspacewrite" => Ok(Self::WorkspaceWrite),
            _ => bail!("permission_mode must be read-only or workspace-write"),
        }
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadOnly => write!(f, "read-only"),
            Self::WorkspaceWrite => write!(f, "workspace-write"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum SandboxPolicy {
    #[default]
    PolicyOnly,
}

impl SandboxPolicy {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "policy-only" | "policy_only" | "policyonly" | "dry-run" | "dry_run" => {
                Ok(Self::PolicyOnly)
            }
            _ => bail!("sandbox_policy must be policy-only"),
        }
    }
}

impl fmt::Display for SandboxPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PolicyOnly => write!(f, "policy-only"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum ApprovalMode {
    /// Ask before every write and command (safest).
    #[default]
    Suggest,
    /// Auto-approve file edits, ask for shell commands.
    AutoEdit,
    /// Auto-approve everything. Only for isolated environments.
    FullAuto,
}

impl ApprovalMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "suggest" => Ok(Self::Suggest),
            "auto-edit" | "auto_edit" | "autoedit" => Ok(Self::AutoEdit),
            "full-auto" | "full_auto" | "fullauto" => Ok(Self::FullAuto),
            _ => bail!("approval_mode must be suggest, auto-edit, or full-auto"),
        }
    }
}

impl fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Suggest => write!(f, "suggest"),
            Self::AutoEdit => write!(f, "auto-edit"),
            Self::FullAuto => write!(f, "full-auto"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub api_base: Option<String>,
    pub model: Option<String>,
    pub session_token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_expires_at: Option<u64>,
    pub theme: Option<String>,
    pub approved_command_prefixes: Option<Vec<Vec<String>>>,
    pub permission_mode: Option<PermissionMode>,
    pub sandbox_policy: Option<SandboxPolicy>,
    pub approval_mode: Option<ApprovalMode>,
    pub onboarded: Option<bool>,
    pub update_check_disabled: Option<bool>,
    pub last_update_check_at: Option<u64>,
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

pub fn effective_permission_mode(config: &Config) -> PermissionMode {
    std::env::var("CODEUS_PERMISSION_MODE")
        .ok()
        .and_then(|value| PermissionMode::parse(&value).ok())
        .or(config.permission_mode)
        .unwrap_or_default()
}

pub fn effective_sandbox_policy(config: &Config) -> SandboxPolicy {
    std::env::var("CODEUS_SANDBOX_POLICY")
        .ok()
        .and_then(|value| SandboxPolicy::parse(&value).ok())
        .or(config.sandbox_policy)
        .unwrap_or_default()
}

pub fn effective_approval_mode(config: &Config) -> ApprovalMode {
    std::env::var("CODEUS_APPROVAL_MODE")
        .ok()
        .and_then(|value| ApprovalMode::parse(&value).ok())
        .or(config.approval_mode)
        .unwrap_or_default()
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
