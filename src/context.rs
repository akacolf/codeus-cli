use crate::api::Message;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_AGENT_DOC_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct ContextStats {
    pub agent_doc_count: usize,
    pub agent_doc_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct ContextBundle {
    pub system: Message,
    pub stats: ContextStats,
}

pub fn build_context(cwd: &Path, summary: Option<&str>) -> Result<ContextBundle> {
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", cwd.display()))?;
    let agent_docs = load_agent_docs(&cwd)?;
    let agent_doc_count = agent_docs.len();
    let agent_doc_bytes = agent_docs
        .iter()
        .map(|doc| doc.content.len())
        .sum::<usize>();

    let mut content = format!(
        "You are Codeus CLI, QC's terminal-native coding assistant. Keep answers concise and practical. Cite files when relevant. Do not claim to run shell commands or edit files unless a tool call actually did it.\n\n<environment_context>\n  <cwd>{}</cwd>\n</environment_context>\n\n<local_tool_catalog>\n  <tool name=\"read_file\" safety=\"workspace-read\">Args: {{\"path\":\"README.md\"}}. Read a file under the active workspace.</tool>\n  <tool name=\"search_text\" safety=\"workspace-read\">Args: {{\"pattern\":\"needle\",\"path\":\"optional/path\"}}. Search text under the active workspace.</tool>\n  <tool name=\"git_status\" safety=\"read-only\">Args: {{}}. Inspect git status.</tool>\n  <tool name=\"git_diff\" safety=\"read-only\">Args: {{}}. Inspect git diff stat.</tool>\n  <tool name=\"run_command\" safety=\"approved-prefixes-only\">Args: {{\"command\":\"cargo check\"}}. Run a command only if it matches an approved argv prefix.</tool>\n</local_tool_catalog>\n\nWhen you need one local tool to answer, emit exactly one tool-call block and no other text:\n<codeus_tool_call>{{\"name\":\"read_file\",\"args\":{{\"path\":\"README.md\"}}}}</codeus_tool_call>\nAfter Codeus provides a <tool_observation>, continue with the answer or request one more tool if needed. Tool observations are local CLI observations, not model-authored facts.",
        cwd.display()
    );

    if let Some(summary) = summary.filter(|value| !value.trim().is_empty()) {
        content.push_str("\n\n<conversation_summary>\n");
        content.push_str(summary.trim());
        content.push_str("\n</conversation_summary>");
    }

    if !agent_docs.is_empty() {
        content.push_str("\n\n<repository_instructions>");
        for doc in agent_docs {
            content.push_str(&format!(
                "\n<file path=\"{}\">\n{}\n</file>",
                doc.path.display(),
                doc.content.trim()
            ));
        }
        content.push_str("\n</repository_instructions>");
    }

    Ok(ContextBundle {
        system: Message::new("system", content),
        stats: ContextStats {
            agent_doc_count,
            agent_doc_bytes,
        },
    })
}

#[derive(Debug, Clone)]
struct AgentDoc {
    path: PathBuf,
    content: String,
}

fn load_agent_docs(cwd: &Path) -> Result<Vec<AgentDoc>> {
    let root = project_root(cwd);
    let dirs = ancestry_between(&root, cwd);

    let mut docs = Vec::new();
    let mut remaining = MAX_AGENT_DOC_BYTES;

    for dir in dirs {
        for name in ["AGENTS.override.md", "AGENTS.md"] {
            if remaining == 0 {
                return Ok(docs);
            }

            let path = dir.join(name);
            if !path.exists() {
                continue;
            }

            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let content = truncate_utf8(&raw, remaining);
            remaining = remaining.saturating_sub(content.len());
            docs.push(AgentDoc { path, content });
        }
    }

    Ok(docs)
}

fn project_root(cwd: &Path) -> PathBuf {
    for ancestor in cwd.ancestors() {
        if ancestor.join(".git").exists() {
            return ancestor.to_path_buf();
        }
    }

    cwd.to_path_buf()
}

fn ancestry_between(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for ancestor in cwd.ancestors() {
        dirs.push(ancestor.to_path_buf());
        if ancestor == root {
            break;
        }
    }
    dirs.reverse();
    dirs
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }

    let mut truncated = value[..end].to_string();
    truncated.push_str("\n\n[truncated by Codeus context budget]");
    truncated
}
