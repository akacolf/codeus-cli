use crate::config;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn enabled() -> bool {
    std::env::var("CODEUS_TRACE")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

pub fn record(event: &str, fields: Value) -> Result<()> {
    if !enabled() {
        return Ok(());
    }

    let path = trace_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    serde_json::to_writer(
        &mut file,
        &json!({
            "timestamp": now_secs(),
            "event": event,
            "fields": fields,
        }),
    )
    .context("failed to serialize trace event")?;
    writeln!(file).context("failed to write trace event")?;
    Ok(())
}

pub fn trace_path() -> Result<PathBuf> {
    let path = config::config_path()?;
    let parent = path.parent().context("config path has no parent")?;
    Ok(parent.join("events.jsonl"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
