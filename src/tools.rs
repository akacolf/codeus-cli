use crate::config::{self, Config, PermissionMode, SandboxPolicy};
use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_EDIT_BYTES: usize = 512 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_SEARCH_MATCHES: usize = 80;
const MAX_FILE_LIST_ENTRIES: usize = 240;
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

const READ_ONLY_COMMAND_PREFIXES: &[&[&str]] = &[
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
pub struct CommandDecision {
    pub allowed: bool,
    pub reason: String,
    pub matched_rule: Option<String>,
    pub required_permission: PermissionMode,
    pub sandbox_policy: SandboxPolicy,
}

#[derive(Debug, Clone)]
struct CommandRule {
    id: String,
    argv_prefix: Vec<String>,
    required_permission: PermissionMode,
    explanation: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOperation {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Replace {
        path: String,
        old_text: String,
        new_text: String,
    },
}

struct PlannedPatch {
    path: PathBuf,
    action: PlannedPatchAction,
}

enum PlannedPatchAction {
    Write(Vec<u8>),
    Delete,
}

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
    tracing::debug!(tool = "read_file", path, "starting local tool");
    let resolved = resolve_under_cwd(cwd, path)?;
    let metadata = fs::metadata(&resolved)
        .with_context(|| format!("failed to stat {}", resolved.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a file", resolved.display());
    }

    let raw =
        fs::read(&resolved).with_context(|| format!("failed to read {}", resolved.display()))?;
    if looks_binary(&raw) {
        bail!(
            "{} looks like a binary file; Codeus only reads text files",
            resolved.display()
        );
    }

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
    tracing::debug!(tool = "search_text", pattern, root, "starting local tool");
    if pattern.trim().is_empty() {
        bail!("search pattern cannot be empty");
    }

    let root = match root {
        Some(root) if !root.trim().is_empty() => resolve_under_cwd(cwd, root)?,
        _ => cwd.canonicalize().context("failed to resolve cwd")?,
    };
    let mut matches = Vec::new();
    let metadata = fs::symlink_metadata(&root)
        .with_context(|| format!("failed to stat {}", root.display()))?;
    if metadata.is_file() {
        search_file(&root, pattern, &mut matches)?;
    } else {
        for path in walk_workspace_files(&root, MAX_SEARCH_MATCHES)? {
            if matches.len() >= MAX_SEARCH_MATCHES {
                break;
            }
            search_file(&path, pattern, &mut matches)?;
        }
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

pub fn list_files(cwd: &Path, root: Option<&str>) -> Result<ToolObservation> {
    let started = Instant::now();
    tracing::debug!(tool = "list_files", root, "starting local tool");
    let cwd = cwd.canonicalize().context("failed to resolve cwd")?;
    let root = match root {
        Some(root) if !root.trim().is_empty() => resolve_under_cwd(&cwd, root)?,
        _ => cwd.clone(),
    };

    let metadata = fs::symlink_metadata(&root)
        .with_context(|| format!("failed to stat {}", root.display()))?;
    let mut entries = Vec::new();
    if metadata.is_file() {
        entries.push(display_relative_path(&cwd, &root));
    } else if metadata.is_dir() {
        entries.extend(
            walk_workspace_files(&root, MAX_FILE_LIST_ENTRIES)?
                .into_iter()
                .map(|path| display_relative_path(&cwd, &path)),
        );
    } else {
        bail!("{} is not a regular file or directory", root.display());
    }

    let mut output = if entries.is_empty() {
        "No files found.".to_string()
    } else {
        entries.join("\n")
    };
    if entries.len() >= MAX_FILE_LIST_ENTRIES {
        output.push_str(&format!(
            "\n[stopped after {} files; pass a narrower path for more]",
            MAX_FILE_LIST_ENTRIES
        ));
    }

    Ok(ToolObservation {
        name: "list_files".to_string(),
        args: json!({ "path": root, "limit": MAX_FILE_LIST_ENTRIES }),
        status: ToolStatus::Success,
        output,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

pub fn replace_in_file(
    cwd: &Path,
    path: &str,
    old_text: &str,
    new_text: &str,
) -> Result<ToolObservation> {
    let started = Instant::now();
    tracing::debug!(tool = "replace_in_file", path, "starting local tool");
    if old_text.is_empty() {
        bail!("old_text cannot be empty");
    }
    let resolved = resolve_under_cwd(cwd, path)?;
    let metadata = fs::metadata(&resolved)
        .with_context(|| format!("failed to stat {}", resolved.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a file", resolved.display());
    }
    if metadata.len() as usize > MAX_EDIT_BYTES {
        bail!(
            "{} is too large to edit safely ({} bytes > {} bytes)",
            resolved.display(),
            metadata.len(),
            MAX_EDIT_BYTES
        );
    }

    let content = fs::read_to_string(&resolved)
        .with_context(|| format!("failed to read {}", resolved.display()))?;
    if looks_binary(content.as_bytes()) {
        bail!("{} looks like a binary file", resolved.display());
    }
    let match_count = content.match_indices(old_text).take(2).count();
    if match_count != 1 {
        return Ok(ToolObservation {
            name: "replace_in_file".to_string(),
            args: json!({ "path": path, "resolved": resolved, "match_count": match_count }),
            status: ToolStatus::Failed,
            output: match match_count {
                0 => "old_text was not found; read the file and try again with an exact snippet."
                    .to_string(),
                _ => {
                    "old_text matched more than once; provide a larger unique snippet.".to_string()
                }
            },
            elapsed_ms: started.elapsed().as_millis(),
        });
    }

    let updated = content.replacen(old_text, new_text, 1);
    atomic_write(&resolved, updated.as_bytes())
        .with_context(|| format!("failed to write {}", resolved.display()))?;

    Ok(ToolObservation {
        name: "replace_in_file".to_string(),
        args: json!({
            "path": path,
            "resolved": resolved,
            "old_bytes": old_text.len(),
            "new_bytes": new_text.len()
        }),
        status: ToolStatus::Success,
        output: format!(
            "Replaced {} bytes with {} bytes in {}.",
            old_text.len(),
            new_text.len(),
            path
        ),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

pub fn apply_patch(cwd: &Path, operations: Vec<PatchOperation>) -> Result<ToolObservation> {
    let started = Instant::now();
    tracing::debug!(
        tool = "apply_patch",
        operation_count = operations.len(),
        "starting local tool"
    );
    if operations.is_empty() {
        bail!("patch operations cannot be empty");
    }

    let cwd = cwd.canonicalize().context("failed to resolve cwd")?;
    let planned = plan_patch(&cwd, operations)?;

    for plan in &planned {
        match &plan.action {
            PlannedPatchAction::Write(bytes) => {
                atomic_write(&plan.path, bytes)
                    .with_context(|| format!("failed to write {}", plan.path.display()))?;
            }
            PlannedPatchAction::Delete => {
                fs::remove_file(&plan.path)
                    .with_context(|| format!("failed to delete {}", plan.path.display()))?;
            }
        }
    }

    Ok(ToolObservation {
        name: "apply_patch".to_string(),
        args: json!({ "operation_count": planned.len() }),
        status: ToolStatus::Success,
        output: format!("Applied {} patch operation(s).", planned.len()),
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
    tracing::debug!(
        tool = "run_command",
        command = command_line,
        "starting local tool"
    );
    let argv = parse_command_line(command_line)?;
    if argv.is_empty() {
        bail!("command cannot be empty");
    }

    let decision = evaluate_command_policy(&argv, config);
    if !decision.allowed {
        let reason = decision.reason.clone();
        return Ok(ToolObservation {
            name: "run_command".to_string(),
            args: json!({ "command": command_line, "argv": argv, "decision": decision }),
            status: ToolStatus::Denied,
            output: reason,
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
        args: json!({ "command": command_line, "argv": argv, "decision": decision }),
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
    allowed_prefix_labels(config).join(", ")
}

pub fn allowed_prefix_labels(config: &Config) -> Vec<String> {
    let mut prefixes = command_rules(config)
        .into_iter()
        .map(|rule| rule.argv_prefix.join(" "))
        .collect::<Vec<_>>();
    prefixes.sort();
    prefixes.dedup();
    prefixes
}

pub fn evaluate_command_policy(argv: &[String], config: &Config) -> CommandDecision {
    let permission_mode = config::effective_permission_mode(config);
    let sandbox_policy = config::effective_sandbox_policy(config);

    for rule in command_rules(config) {
        if !matches_prefix(argv, &rule.argv_prefix) {
            continue;
        }

        if permission_mode == PermissionMode::ReadOnly
            && rule.required_permission != PermissionMode::ReadOnly
        {
            return CommandDecision {
                allowed: false,
                reason: format!(
                    "Command denied by {}. {} requires workspace-write, but permission mode is read-only.",
                    rule.id, rule.explanation
                ),
                matched_rule: Some(rule.id),
                required_permission: rule.required_permission,
                sandbox_policy,
            };
        }

        return CommandDecision {
            allowed: true,
            reason: format!(
                "Allowed by {} with {} sandbox enforcement.",
                rule.id, sandbox_policy
            ),
            matched_rule: Some(rule.id),
            required_permission: rule.required_permission,
            sandbox_policy,
        };
    }

    CommandDecision {
        allowed: false,
        reason: format!(
            "Command denied. No command policy rule matched. Approved prefixes: {}",
            format_allowed_prefixes(config)
        ),
        matched_rule: None,
        required_permission: PermissionMode::WorkspaceWrite,
        sandbox_policy,
    }
}

fn search_file(path: &Path, pattern: &str, matches: &mut Vec<String>) -> Result<()> {
    if matches.len() >= MAX_SEARCH_MATCHES {
        return Ok(());
    }
    let raw = match fs::read(path) {
        Ok(raw) if raw.len() <= MAX_READ_BYTES => raw,
        _ => return Ok(()),
    };
    if looks_binary(&raw) {
        return Ok(());
    }
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

fn walk_workspace_files(root: &Path, max_entries: usize) -> Result<Vec<PathBuf>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(false)
        .hidden(false)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true);
    builder.filter_entry(|entry| !is_skipped_dir(entry.path()));

    let mut entries = Vec::new();
    for result in builder.build() {
        if entries.len() >= max_entries {
            break;
        }

        let entry =
            result.with_context(|| format!("failed to walk files under {}", root.display()))?;
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            entries.push(entry.into_path());
        }
    }
    entries.sort();
    Ok(entries)
}

fn is_skipped_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| SKIP_DIRS.contains(&name))
}

fn display_relative_path(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn plan_patch(cwd: &Path, operations: Vec<PatchOperation>) -> Result<Vec<PlannedPatch>> {
    let mut planned = Vec::new();
    let mut total_write_bytes = 0usize;

    for operation in operations {
        match operation {
            PatchOperation::Add { path, content } => {
                let resolved = resolve_write_path_under_cwd(cwd, &path)?;
                if resolved.exists() {
                    bail!("{} already exists; use replace for existing files", path);
                }
                total_write_bytes = total_write_bytes.saturating_add(content.len());
                if total_write_bytes > MAX_EDIT_BYTES {
                    bail!("patch writes exceed {} bytes", MAX_EDIT_BYTES);
                }
                planned.push(PlannedPatch {
                    path: resolved,
                    action: PlannedPatchAction::Write(content.into_bytes()),
                });
            }
            PatchOperation::Delete { path } => {
                let resolved = resolve_under_cwd(cwd, &path)?;
                let metadata = fs::metadata(&resolved)
                    .with_context(|| format!("failed to stat {}", resolved.display()))?;
                if !metadata.is_file() {
                    bail!("{} is not a file", resolved.display());
                }
                planned.push(PlannedPatch {
                    path: resolved,
                    action: PlannedPatchAction::Delete,
                });
            }
            PatchOperation::Replace {
                path,
                old_text,
                new_text,
            } => {
                if old_text.is_empty() {
                    bail!("old_text cannot be empty for {}", path);
                }
                let resolved = resolve_under_cwd(cwd, &path)?;
                let metadata = fs::metadata(&resolved)
                    .with_context(|| format!("failed to stat {}", resolved.display()))?;
                if !metadata.is_file() {
                    bail!("{} is not a file", resolved.display());
                }
                if metadata.len() as usize > MAX_EDIT_BYTES {
                    bail!(
                        "{} is too large to edit safely ({} bytes > {} bytes)",
                        resolved.display(),
                        metadata.len(),
                        MAX_EDIT_BYTES
                    );
                }

                let content = fs::read_to_string(&resolved)
                    .with_context(|| format!("failed to read {}", resolved.display()))?;
                if looks_binary(content.as_bytes()) {
                    bail!("{} looks like a binary file", resolved.display());
                }
                let match_count = content.match_indices(&old_text).take(2).count();
                if match_count != 1 {
                    bail!(
                        "{} replacement must match exactly once; matched {} time(s)",
                        path,
                        match_count
                    );
                }
                let updated = content.replacen(&old_text, &new_text, 1);
                total_write_bytes = total_write_bytes.saturating_add(updated.len());
                if total_write_bytes > MAX_EDIT_BYTES {
                    bail!("patch writes exceed {} bytes", MAX_EDIT_BYTES);
                }
                planned.push(PlannedPatch {
                    path: resolved,
                    action: PlannedPatchAction::Write(updated.into_bytes()),
                });
            }
        }
    }

    Ok(planned)
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

fn resolve_write_path_under_cwd(cwd: &Path, path: &str) -> Result<PathBuf> {
    let cwd = cwd.canonicalize().context("failed to resolve cwd")?;
    let path = expand_path(cwd.as_path(), path)?;
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?
        .canonicalize()
        .with_context(|| format!("failed to resolve parent for {}", path.display()))?;
    if !parent.starts_with(&cwd) {
        bail!(
            "{} is outside the workspace root {}",
            parent.display(),
            cwd.display()
        );
    }
    let file_name = path
        .file_name()
        .with_context(|| format!("{} is not a file path", path.display()))?;
    Ok(parent.join(file_name))
}

fn expand_path(cwd: &Path, path: &str) -> Result<PathBuf> {
    Ok(if let Some(rest) = path.strip_prefix("~/") {
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
    })
}

fn matches_prefix(argv: &[String], prefix: &[String]) -> bool {
    argv.len() >= prefix.len()
        && argv
            .iter()
            .zip(prefix)
            .all(|(arg, expected)| arg == expected)
}

fn command_rules(config: &Config) -> Vec<CommandRule> {
    let mut rules = Vec::new();
    for prefix in default_prefixes() {
        let required_permission = if read_only_prefixes()
            .iter()
            .any(|read_only| read_only == &prefix)
        {
            PermissionMode::ReadOnly
        } else {
            PermissionMode::WorkspaceWrite
        };
        rules.push(CommandRule {
            id: format!("builtin:{}", prefix.join(" ")),
            argv_prefix: prefix.clone(),
            required_permission,
            explanation: format!("built-in command prefix `{}`", prefix.join(" ")),
        });
    }

    if let Some(extra) = &config.approved_command_prefixes {
        for (index, prefix) in extra.iter().enumerate() {
            rules.push(CommandRule {
                id: format!("config:approved_command_prefixes[{index}]"),
                argv_prefix: prefix.clone(),
                required_permission: PermissionMode::WorkspaceWrite,
                explanation: format!("user-approved command prefix `{}`", prefix.join(" ")),
            });
        }
    }

    rules
}

fn default_prefixes() -> Vec<Vec<String>> {
    DEFAULT_COMMAND_PREFIXES
        .iter()
        .map(|prefix| prefix.iter().map(|part| part.to_string()).collect())
        .collect()
}

fn read_only_prefixes() -> Vec<Vec<String>> {
    READ_ONLY_COMMAND_PREFIXES
        .iter()
        .map(|prefix| prefix.iter().map(|part| part.to_string()).collect())
        .collect()
}

fn parse_command_line(command_line: &str) -> Result<Vec<String>> {
    shlex::split(command_line).context("unterminated or invalid shell-style quoting in command")
}

fn looks_binary(raw: &[u8]) -> bool {
    raw.iter().take(8192).any(|byte| *byte == 0)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("codeus-edit");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{file_name}.codeus-{nonce}.tmp"));
    fs::write(&tmp, bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| {
        format!(
            "failed to replace {} with temporary file {}",
            path.display(),
            tmp.display()
        )
    })?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shell_like_quotes() {
        let argv = parse_command_line(r#"cargo test "tool parser" -- --exact"#).unwrap();

        assert_eq!(argv, vec!["cargo", "test", "tool parser", "--", "--exact"]);
    }

    #[test]
    fn rejects_unterminated_quotes() {
        let error = parse_command_line(r#"cargo test "unterminated"#).unwrap_err();

        assert!(error.to_string().contains("quoting"));
    }

    #[test]
    fn list_files_returns_relative_paths_and_skips_generated_dirs() {
        let root = std::env::temp_dir().join(format!(
            "codeus-tools-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("README.md"), "hello").unwrap();
        std::fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("target").join("generated.txt"), "skip").unwrap();

        let observation = list_files(&root, None).unwrap();

        assert!(observation.output.contains("README.md"));
        assert!(observation.output.contains("src/main.rs"));
        assert!(!observation.output.contains("target/generated.txt"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replace_in_file_requires_exactly_one_match() {
        let root = std::env::temp_dir().join(format!(
            "codeus-replace-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("example.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();

        let observation = replace_in_file(&root, "example.txt", "beta", "gamma").unwrap();
        let updated = std::fs::read_to_string(&path).unwrap();

        assert!(matches!(observation.status, ToolStatus::Success));
        assert_eq!(updated, "alpha\ngamma\n");

        std::fs::write(&path, "same\nsame\n").unwrap();
        let observation = replace_in_file(&root, "example.txt", "same", "once").unwrap();

        assert!(matches!(observation.status, ToolStatus::Failed));
        assert!(observation.output.contains("more than once"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn command_policy_reports_read_only_denial() {
        let config = Config {
            permission_mode: Some(PermissionMode::ReadOnly),
            ..Config::default()
        };
        let argv = vec!["cargo".to_string(), "check".to_string()];

        let decision = evaluate_command_policy(&argv, &config);

        assert!(!decision.allowed);
        assert_eq!(decision.required_permission, PermissionMode::WorkspaceWrite);
        assert!(decision.reason.contains("read-only"));
        assert_eq!(
            decision.matched_rule.as_deref(),
            Some("builtin:cargo check")
        );
    }

    #[test]
    fn apply_patch_validates_and_applies_operations() {
        let root = std::env::temp_dir().join(format!(
            "codeus-patch-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("existing.txt"), "hello\n").unwrap();
        std::fs::write(root.join("delete.txt"), "remove me").unwrap();

        let observation = apply_patch(
            &root,
            vec![
                PatchOperation::Add {
                    path: "new.txt".to_string(),
                    content: "fresh\n".to_string(),
                },
                PatchOperation::Replace {
                    path: "existing.txt".to_string(),
                    old_text: "hello".to_string(),
                    new_text: "hi".to_string(),
                },
                PatchOperation::Delete {
                    path: "delete.txt".to_string(),
                },
            ],
        )
        .unwrap();

        assert!(matches!(observation.status, ToolStatus::Success));
        assert_eq!(
            std::fs::read_to_string(root.join("existing.txt")).unwrap(),
            "hi\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("new.txt")).unwrap(),
            "fresh\n"
        );
        assert!(!root.join("delete.txt").exists());

        std::fs::remove_dir_all(root).unwrap();
    }
}
