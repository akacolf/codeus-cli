use crate::api::ChatToolCall;
use crate::config::Config;
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
) -> Result<ToolObservation> {
    match call.name.as_str() {
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
            "unknown tool; allowed tools are read_file, search_text, git_status, git_diff, run_command",
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
