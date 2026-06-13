use crate::api::ChatToolCall;
use crate::approval::{self, ApprovalDecision, ApprovalEngine, ToolSafety};
use crate::config::{self, Config, PermissionMode};
use crate::tools::{self, ToolObservation, ToolStatus};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

pub const TOOL_CALL_OPEN: &str = "<codeus_tool_call>";
pub const TOOL_CALL_CLOSE: &str = "</codeus_tool_call>";

#[derive(Debug, Clone, Deserialize)]
pub struct ModelToolCall {
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

pub fn parse_tool_call(text: &str) -> Result<Option<ModelToolCall>> {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix(TOOL_CALL_OPEN) else {
        return Ok(None);
    };
    let Some(json_text) = rest.strip_suffix(TOOL_CALL_CLOSE) else {
        return Ok(None);
    };

    let call = serde_json::from_str(json_text.trim()).context("failed to parse model tool call")?;
    Ok(Some(call))
}

pub async fn execute_tool_call(
    cwd: &Path,
    config: &Config,
    call: &ModelToolCall,
    approval: &mut ApprovalEngine,
) -> Result<ToolObservation> {
    // Approval gate for write/command tools
    let safety = approval::tool_safety(&call.name);
    if safety != ToolSafety::ReadOnly {
        let description = match call.name.as_str() {
            "run_command" => string_arg(&call.args, "command").unwrap_or("").to_string(),
            "replace_in_file" | "apply_patch" => {
                string_arg(&call.args, "path").unwrap_or("").to_string()
            }
            _ => format!("{}", call.args),
        };
        if approval.check(&call.name, safety, &description)? == ApprovalDecision::Denied {
            return Ok(denied(
                &call.name,
                &call.args,
                &format!(
                    "Action denied by approval policy (mode: {}). Use --approval-mode auto-edit or full-auto to change.",
                    approval.mode()
                ),
            ));
        }
    }

    match call.name.as_str() {
        "list_files" => tools::list_files(cwd, string_arg(&call.args, "path")),
        "read_file" => {
            let Some(path) = string_arg(&call.args, "path") else {
                return Ok(denied(&call.name, &call.args, "missing string arg `path`"));
            };
            tools::read_file(cwd, path)
        }
        "search_text" => {
            let Some(pattern) = string_arg(&call.args, "pattern") else {
                return Ok(denied(&call.name, &call.args, "missing string arg `pattern`"));
            };
            tools::search_text(cwd, pattern, string_arg(&call.args, "path"))
        }
        "replace_in_file" => {
            if config::effective_permission_mode(config) == PermissionMode::ReadOnly {
                return Ok(denied(
                    &call.name,
                    &call.args,
                    "File edits are denied in read-only permission mode.",
                ));
            }
            let Some(path) = string_arg(&call.args, "path") else {
                return Ok(denied(&call.name, &call.args, "missing string arg `path`"));
            };
            let Some(old_text) = string_arg(&call.args, "old_text") else {
                return Ok(denied(
                    &call.name,
                    &call.args,
                    "missing string arg `old_text`",
                ));
            };
            let Some(new_text) = call.args.get("new_text").and_then(Value::as_str) else {
                return Ok(denied(
                    &call.name,
                    &call.args,
                    "missing string arg `new_text`",
                ));
            };
            tools::replace_in_file(cwd, path, old_text, new_text)
        }
        "apply_patch" => {
            if config::effective_permission_mode(config) == PermissionMode::ReadOnly {
                return Ok(denied(
                    &call.name,
                    &call.args,
                    "File edits are denied in read-only permission mode.",
                ));
            }
            let Some(operations) = call.args.get("operations") else {
                return Ok(denied(
                    &call.name,
                    &call.args,
                    "missing array arg `operations`",
                ));
            };
            let operations = serde_json::from_value(operations.clone())
                .context("failed to parse apply_patch operations")?;
            tools::apply_patch(cwd, operations)
        }
        "git_status" => tools::git_status(cwd).await,
        "git_diff" => tools::git_diff(cwd).await,
        "run_command" => {
            let Some(command) = string_arg(&call.args, "command") else {
                return Ok(denied(&call.name, &call.args, "missing string arg `command`"));
            };
            tools::run_command(cwd, config, command).await
        }
        _ => Ok(denied(
            &call.name,
            &call.args,
            "unknown tool; allowed tools are list_files, read_file, search_text, replace_in_file, apply_patch, git_status, git_diff, run_command",
        )),
    }
}

pub fn from_chat_tool_call(tool_call: &ChatToolCall) -> Result<ModelToolCall> {
    let args = if tool_call.function.arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&tool_call.function.arguments)
            .context("failed to parse native tool-call arguments")?
    };

    Ok(ModelToolCall {
        name: tool_call.function.name.clone(),
        args,
    })
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn denied(name: &str, args: &Value, output: &str) -> ToolObservation {
    ToolObservation {
        name: name.to_string(),
        args: json!(args),
        status: ToolStatus::Denied,
        output: output.to_string(),
        elapsed_ms: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tagged_tool_call() {
        let call = parse_tool_call(
            r#"<codeus_tool_call>{"name":"read_file","args":{"path":"README.md"}}</codeus_tool_call>"#,
        )
        .unwrap()
        .unwrap();

        assert_eq!(call.name, "read_file");
        assert_eq!(call.args["path"], "README.md");
    }

    #[test]
    fn ignores_normal_text() {
        assert!(parse_tool_call("Here is the answer.").unwrap().is_none());
    }
}
