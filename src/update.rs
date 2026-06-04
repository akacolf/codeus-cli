use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_UPDATE_ENDPOINT: &str =
    "https://github.com/quantumclub/codeus-cli/releases/latest/download/codeus-release.json";
const CHECK_INTERVAL_SECS: u64 = 60 * 60 * 24;

#[derive(Debug, Clone, Deserialize)]
struct ReleaseInfo {
    version: String,
    #[serde(default)]
    notes_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    install: InstallCommands,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct InstallCommands {
    curl: Option<String>,
    homebrew: Option<String>,
    npm: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum InstallChannel {
    Direct,
    Homebrew,
    Npm,
    Cargo,
    Unknown,
}

pub async fn maybe_print_update_notice(config: &Config) {
    if update_checks_disabled(config) || checked_recently(config) {
        return;
    }

    let Ok(Some(release)) = fetch_latest_release().await else {
        let _ = mark_checked();
        return;
    };
    let _ = mark_checked();

    if !is_newer_version(&release.version, env!("CARGO_PKG_VERSION")) {
        return;
    }

    println!(
        "Update available: Codeus CLI {} -> {}",
        env!("CARGO_PKG_VERSION"),
        release.version
    );
    println!("Run `codeus update` for the right install command.");
    if let Some(url) = release.notes_url.as_deref().or(release.url.as_deref()) {
        println!("Release notes: {url}");
    }
    println!();
}

pub async fn run_update() -> Result<()> {
    let release = fetch_latest_release()
        .await?
        .ok_or_else(|| anyhow!("update metadata is not available yet"))?;

    if !is_newer_version(&release.version, env!("CARGO_PKG_VERSION")) {
        println!("Codeus CLI is up to date ({})", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    println!(
        "Codeus CLI {} is available. Current version: {}",
        release.version,
        env!("CARGO_PKG_VERSION")
    );
    if let Some(url) = release.notes_url.as_deref().or(release.url.as_deref()) {
        println!("Release notes: {url}");
    }
    println!();
    println!("{}", update_command(&release, detect_install_channel()));
    Ok(())
}

fn update_command(release: &ReleaseInfo, channel: InstallChannel) -> String {
    match channel {
        InstallChannel::Homebrew => release
            .install
            .homebrew
            .clone()
            .unwrap_or_else(|| "Update with: brew update && brew upgrade quantumclub/tap/codeus".to_string()),
        InstallChannel::Npm => release
            .install
            .npm
            .clone()
            .unwrap_or_else(|| "Update with: npm install -g @quantumclub/codeus@latest".to_string()),
        InstallChannel::Cargo => {
            "Update with: cargo install --git https://github.com/quantumclub/codeus-cli codeus --locked --force".to_string()
        }
        InstallChannel::Direct | InstallChannel::Unknown => release.install.curl.clone().unwrap_or_else(|| {
            "Update with: curl -fsSL https://install.quantumclub.sh/codeus.sh | sh".to_string()
        }),
    }
}

async fn fetch_latest_release() -> Result<Option<ReleaseInfo>> {
    let endpoint = std::env::var("CODEUS_UPDATE_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_UPDATE_ENDPOINT.to_string());

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .context("failed to build update HTTP client")?;
    let response = http
        .get(endpoint)
        .send()
        .await
        .context("failed to check for Codeus CLI updates")?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(anyhow!("update check returned {}", response.status()));
    }

    response
        .json::<ReleaseInfo>()
        .await
        .map(Some)
        .context("failed to parse update metadata")
}

fn update_checks_disabled(config: &Config) -> bool {
    config.update_check_disabled.unwrap_or(false)
        || std::env::var_os("CODEUS_NO_UPDATE_CHECK").is_some()
}

fn checked_recently(config: &Config) -> bool {
    let Some(last) = config.last_update_check_at else {
        return false;
    };
    now_secs().saturating_sub(last) < CHECK_INTERVAL_SECS
}

fn mark_checked() -> Result<()> {
    let mut config = Config::load()?;
    config.last_update_check_at = Some(now_secs());
    config.save()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn detect_install_channel() -> InstallChannel {
    if let Ok(channel) = std::env::var("CODEUS_INSTALL_CHANNEL") {
        return match channel.as_str() {
            "homebrew" | "brew" => InstallChannel::Homebrew,
            "npm" => InstallChannel::Npm,
            "cargo" => InstallChannel::Cargo,
            "direct" | "curl" => InstallChannel::Direct,
            _ => InstallChannel::Unknown,
        };
    }

    let Ok(exe) = std::env::current_exe() else {
        return InstallChannel::Unknown;
    };
    let path = exe.display().to_string();
    if path.contains("/Cellar/") || path.contains("/Homebrew/") || path.contains("/homebrew/") {
        InstallChannel::Homebrew
    } else if path.contains("node_modules") {
        InstallChannel::Npm
    } else if path.contains("/.cargo/bin/") {
        InstallChannel::Cargo
    } else if path.contains("/.local/bin/") || path.contains("/usr/local/bin/") {
        InstallChannel::Direct
    } else {
        InstallChannel::Unknown
    }
}

fn is_newer_version(candidate: &str, current: &str) -> bool {
    let candidate = version_parts(candidate);
    let current = version_parts(current);
    candidate > current
}

fn version_parts(value: &str) -> Vec<u64> {
    value
        .trim()
        .trim_start_matches('v')
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .take(3)
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semverish_versions() {
        assert!(is_newer_version("0.2.0", "0.1.9"));
        assert!(is_newer_version("v1.0.0", "0.9.9"));
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.1.0-alpha.1", "0.1.0"));
    }
}
