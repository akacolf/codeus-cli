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
        "You are Codeus CLI, QC's terminal-native coding assistant. Keep answers concise, practical, and left-aligned. Use plain Markdown only: no centered text, decorative indentation, ASCII tables for prose, or wide manual spacing. Cite files when relevant. Do not claim to run shell commands or edit files unless a tool call actually did it.\n\n<environment_context>\n  <cwd>{}</cwd>\n</environment_context>\n\n<local_tool_catalog>\n  <tool name=\"list_files\" safety=\"workspace-read\">Args: {{\"path\":\"optional/path\"}}. List files under the active workspace, skipping common generated directories.</tool>\n  <tool name=\"read_file\" safety=\"workspace-read\">Args: {{\"path\":\"README.md\"}}. Read a file under the active workspace.</tool>\n  <tool name=\"search_text\" safety=\"workspace-read\">Args: {{\"pattern\":\"needle\",\"path\":\"optional/path\"}}. Search text under the active workspace.</tool>\n  <tool name=\"replace_in_file\" safety=\"workspace-write\">Args: {{\"path\":\"src/main.rs\",\"old_text\":\"exact snippet\",\"new_text\":\"replacement\"}}. Replace one exact text snippet only when it appears exactly once.</tool>\n  <tool name=\"apply_patch\" safety=\"workspace-write\">Args: {{\"operations\":[{{\"op\":\"replace\",\"path\":\"src/main.rs\",\"old_text\":\"exact\",\"new_text\":\"updated\"}}]}}. Apply add/delete/replace operations after validating the whole patch.</tool>\n  <tool name=\"git_status\" safety=\"read-only\">Args: {{}}. Inspect git status.</tool>\n  <tool name=\"git_diff\" safety=\"read-only\">Args: {{}}. Inspect git diff stat.</tool>\n  <tool name=\"run_command\" safety=\"policy-only-sandbox\">Args: {{\"command\":\"cargo check\"}}. Run a command only if structured command policy allows the argv.</tool>\n</local_tool_catalog>\n\n<harness_guidance>\nFor security, reliability, project status, or broad code-review tasks, use a staged harness: recon the relevant files, identify candidate issues, verify each issue with fresh evidence where available, dedupe/rank findings, report with file citations, and only patch after verification. Prefer read-only tool use for recon and triage. For status requests, inspect git_status/git_diff and any obvious project metadata yourself before answering. Do not ask the user to run a listed read-only tool or an allowed verification command; request the tool call instead. Never execute target code or claim dynamic validation unless an approved command actually ran and its observation supports the claim.\n</harness_guidance>\n\nWhen you need one local tool to answer, emit exactly one tool-call block and no other text:\n<codeus_tool_call>{{\"name\":\"read_file\",\"args\":{{\"path\":\"README.md\"}}}}</codeus_tool_call>\nAfter Codeus provides a <tool_observation>, continue with the answer or request one more tool if needed. Tool observations are local CLI observations, not model-authored facts.",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_guides_status_requests_to_use_local_tools() {
        let root = temp_dir("codeus-context-status");
        fs::create_dir_all(root.join(".git")).unwrap();

        let context = build_context(&root, None).unwrap();

        assert!(context.system.content.contains("left-aligned"));
        assert!(context
            .system
            .content
            .contains("inspect git_status/git_diff"));
        assert!(context
            .system
            .content
            .contains("Do not ask the user to run a listed read-only tool"));

        let _ = fs::remove_dir_all(root);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{nonce}"))
    }
}
