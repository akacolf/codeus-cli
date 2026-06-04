use crate::config::Config;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_SEARCH_MATCHES: usize = 80;
const RUN_TIMEOUT_SECS: u64 = 30;

const DEFAULT_COMMAND_PREFIXES: &[&[&str]] = &[
    &["cargo", "check"],
    &["cargo", "fmt"],
    &["git", "status"],
    &["git", "diff"],
    &["ls"],
    &["pwd"],
    &["rg"],
];

const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".next",
    "dist",
    "build",
    ".cache",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolObservation {
    pub name: String,
    pub args: Value,
    pub status: ToolStatus,
    pub output: String,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Success,
    Failed,
    Denied,
}

impl ToolObservation {
    pub fn as_context_message(&self) -> String {
        format!(
            "<tool_observation name=\"{}\" status=\"{:?}\" elapsed_ms=\"{}\">\n{}\n</tool_observation>",
            self.name, self.status, self.elapsed_ms, self.output
        )
    }
}

pub fn read_file(cwd: &Path, path: &str) -> Result<ToolObservation> {
    let started = Instant::now();
    let resolved = resolve_under_cwd(cwd, path)?;
    let metadata = fs::metadata(&resolved)
        .with_context(|| format!("failed to stat {}", resolved.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a file", resolved.display());
    }

    let raw =
        fs::read(&resolved).with_context(|| format!("failed to read {}", resolved.display()))?;
    let output = if raw.len() > MAX_READ_BYTES {
        let mut text = String::from_utf8_lossy(&raw[..MAX_READ_BYTES]).to_string();
        text.push_str(&format!(
            "\n\n[truncated by Codeus after {} bytes; file has {} bytes]",
            MAX_READ_BYTES,
            raw.len()
        ));
        text
    } else {
        String::from_utf8_lossy(&raw).to_string()
    };

    Ok(ToolObservation {
        name: "read_file".to_string(),
        args: json!({ "path": path, "resolved": resolved }),
        status: ToolStatus::Success,
        output,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

pub fn search_text(cwd: &Path, pattern: &str, root: Option<&str>) -> Result<ToolObservation> {
    let started = Instant::now();
    if pattern.trim().is_empty() {
        bail!("search pattern cannot be empty");
    }

    let root = match root {
        Some(root) if !root.trim().is_empty() => resolve_under_cwd(cwd, root)?,
        _ => cwd.canonicalize().context("failed to resolve cwd")?,
    };
    let mut matches = Vec::new();
    let metadata =
        fs::metadata(&root).with_context(|| format!("failed to stat {}", root.display()))?;
    if metadata.is_file() {
        search_file(&root, pattern, &mut matches)?;
    } else {
        search_dir(&root, pattern, &mut matches)?;
    }

    let mut output = if matches.is_empty() {
        "No matches.".to_string()
    } else {
        matches.join("\n")
    };
    if matches.len() >= MAX_SEARCH_MATCHES {
        output.push_str(&format!(
            "\n[stopped after {} matches; refine the search for more]",
            MAX_SEARCH_MATCHES
        ));
    }

    Ok(ToolObservation {
        name: "search_text".to_string(),
        args: json!({ "pattern": pattern, "root": root }),
        status: ToolStatus::Success,
        output,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

pub async fn git_status(cwd: &Path) -> Result<ToolObservation> {
    run_command(cwd, &Config::default(), "git status --short").await
}

pub async fn git_diff(cwd: &Path) -> Result<ToolObservation> {
    run_command(cwd, &Config::default(), "git diff --stat").await
}

pub async fn run_command(
    cwd: &Path,
    config: &Config,
    command_line: &str,
) -> Result<ToolObservation> {
    let started = Instant::now();
    let argv = parse_command_line(command_line)?;
    if argv.is_empty() {
        bail!("command cannot be empty");
    }

    if !is_allowed_prefix(&argv, config) {
        return Ok(ToolObservation {
            name: "run_command".to_string(),
            args: json!({ "command": command_line, "argv": argv }),
            status: ToolStatus::Denied,
            output: format!(
                "Command denied. Approved prefixes: {}",
                format_allowed_prefixes(config)
            ),
            elapsed_ms: started.elapsed().as_millis(),
        });
    }

    let mut child = Command::new(&argv[0]);
    child.args(&argv[1..]).current_dir(cwd);
    child.kill_on_drop(true);
    child.stdout(std::process::Stdio::piped());
    child.stderr(std::process::Stdio::piped());

    let result = timeout(Duration::from_secs(RUN_TIMEOUT_SECS), child.output()).await;
    let output = match result {
        Ok(output) => output.context("failed to run command")?,
        Err(_) => {
            return Ok(ToolObservation {
                name: "run_command".to_string(),
                args: json!({ "command": command_line, "argv": argv }),
                status: ToolStatus::Failed,
                output: format!("Command timed out after {RUN_TIMEOUT_SECS}s."),
                elapsed_ms: started.elapsed().as_millis(),
            })
        }
    };

    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if text.trim().is_empty() {
        text = format!("Command exited with status {}.", output.status);
    }

    Ok(ToolObservation {
        name: "run_command".to_string(),
        args: json!({ "command": command_line, "argv": argv }),
        status: if output.status.success() {
            ToolStatus::Success
        } else {
            ToolStatus::Failed
        },
        output: truncate_output(&text, MAX_OUTPUT_BYTES),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

pub fn format_allowed_prefixes(config: &Config) -> String {
    let mut prefixes = default_prefixes()
        .into_iter()
        .map(|prefix| prefix.join(" "))
        .collect::<Vec<_>>();
    if let Some(extra) = &config.approved_command_prefixes {
        prefixes.extend(extra.iter().map(|prefix| prefix.join(" ")));
    }
    prefixes.sort();
    prefixes.dedup();
    prefixes.join(", ")
}

fn search_dir(root: &Path, pattern: &str, matches: &mut Vec<String>) -> Result<()> {
    if matches.len() >= MAX_SEARCH_MATCHES {
        return Ok(());
    }

    let mut entries = fs::read_dir(root)
        .with_context(|| format!("failed to read directory {}", root.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        if matches.len() >= MAX_SEARCH_MATCHES {
            break;
        }
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            if path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| SKIP_DIRS.contains(&name))
            {
                continue;
            }
            search_dir(&path, pattern, matches)?;
        } else if metadata.is_file() {
            search_file(&path, pattern, matches)?;
        }
    }

    Ok(())
}

fn search_file(path: &Path, pattern: &str, matches: &mut Vec<String>) -> Result<()> {
    if matches.len() >= MAX_SEARCH_MATCHES {
        return Ok(());
    }
    let raw = match fs::read(path) {
        Ok(raw) if raw.len() <= MAX_READ_BYTES => raw,
        _ => return Ok(()),
    };
    let Ok(text) = String::from_utf8(raw) else {
        return Ok(());
    };

    for (index, line) in text.lines().enumerate() {
        if line.contains(pattern) {
            matches.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
            if matches.len() >= MAX_SEARCH_MATCHES {
                break;
            }
        }
    }

    Ok(())
}

fn resolve_under_cwd(cwd: &Path, path: &str) -> Result<PathBuf> {
    let cwd = cwd.canonicalize().context("failed to resolve cwd")?;
    let path = if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .context("could not locate home directory")?
            .join(rest)
    } else if path == "~" {
        dirs::home_dir().context("could not locate home directory")?
    } else {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };
    let resolved = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", path.display()))?;
    if !resolved.starts_with(&cwd) {
        bail!(
            "{} is outside the workspace root {}",
            resolved.display(),
            cwd.display()
        );
    }
    Ok(resolved)
}

fn is_allowed_prefix(argv: &[String], config: &Config) -> bool {
    default_prefixes()
        .iter()
        .chain(
            config
                .approved_command_prefixes
                .as_deref()
                .unwrap_or(&[])
                .iter(),
        )
        .any(|prefix| matches_prefix(argv, prefix))
}

fn matches_prefix(argv: &[String], prefix: &[String]) -> bool {
    argv.len() >= prefix.len()
        && argv
            .iter()
            .zip(prefix)
            .all(|(arg, expected)| arg == expected)
}

fn default_prefixes() -> Vec<Vec<String>> {
    DEFAULT_COMMAND_PREFIXES
        .iter()
        .map(|prefix| prefix.iter().map(|part| part.to_string()).collect())
        .collect()
}

fn parse_command_line(command_line: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = command_line.chars().peekable();
    let mut quote = None;

    while let Some(ch) = chars.next() {
        match (ch, quote) {
            ('\'' | '"', None) => quote = Some(ch),
            (value, Some(active)) if value == active => quote = None,
            ('\\', Some('"')) => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (value, None) if value.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            (value, _) => current.push(value),
        }
    }

    if quote.is_some() {
        bail!("unterminated quote in command");
    }
    if !current.is_empty() {
        args.push(current);
    }

    Ok(args)
}

fn truncate_output(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }

    format!(
        "{}\n\n[truncated by Codeus after {} bytes]",
        &value[..end],
        max_bytes
    )
}
