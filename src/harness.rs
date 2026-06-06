use crate::api::{ChatToolCall, Message};
use crate::context::{self, ContextStats};
use crate::thread_store::{StoredThread, ThreadStore};
use crate::tools::ToolObservation;
use anyhow::{Context, Result};
use std::path::PathBuf;

const AUTO_COMPACT_BYTES: usize = 24 * 1024;
const COMPACT_KEEP_MESSAGES: usize = 8;

pub struct Session {
    transcript: Vec<Message>,
    cwd: PathBuf,
    summary: Option<String>,
    store: ThreadStore,
    context_stats: ContextStats,
}

impl Session {
    pub fn new() -> Result<Self> {
        let cwd = std::env::current_dir().context("failed to read current directory")?;
        let store = ThreadStore::create(&cwd)?;
        Self::from_parts(cwd, Vec::new(), None, store)
    }

    pub fn resume_latest_excluding(id: &str) -> Result<Option<Self>> {
        let Some((store, stored)) = ThreadStore::open_latest_excluding(Some(id))? else {
            return Ok(None);
        };
        Self::from_stored(store, stored).map(Some)
    }

    pub fn resume(id: &str) -> Result<Self> {
        let (store, stored) = ThreadStore::open(id)?;
        Self::from_stored(store, stored)
    }

    fn from_stored(store: ThreadStore, stored: StoredThread) -> Result<Self> {
        Self::from_parts(stored.cwd, stored.transcript, stored.summary, store)
    }

    fn from_parts(
        cwd: PathBuf,
        transcript: Vec<Message>,
        summary: Option<String>,
        store: ThreadStore,
    ) -> Result<Self> {
        let context = context::build_context(&cwd, summary.as_deref())?;
        Ok(Self {
            transcript,
            cwd,
            summary,
            store,
            context_stats: context.stats,
        })
    }

    pub fn thread_id(&self) -> &str {
        self.store.id()
    }

    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    pub fn transcript_len(&self) -> usize {
        self.transcript.len()
    }

    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    pub fn messages_for_turn(&mut self) -> Result<Vec<Message>> {
        self.refresh_context()?;
        let mut messages = Vec::with_capacity(self.transcript.len() + 1);
        messages.push(context::build_context(&self.cwd, self.summary.as_deref())?.system);
        messages.extend(self.transcript.clone());
        Ok(messages)
    }

    pub fn record_user(&mut self, content: String) -> Result<()> {
        self.push_message(Message::new("user", content))
    }

    pub fn record_assistant(&mut self, content: String) -> Result<()> {
        self.push_message(Message::new("assistant", content))
    }

    pub fn record_assistant_tool_calls(&mut self, tool_calls: Vec<ChatToolCall>) -> Result<()> {
        self.push_message(Message::assistant_tool_calls(tool_calls))
    }

    pub fn record_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        content: String,
    ) -> Result<()> {
        self.push_message(Message::tool_result(tool_call_id, content))
    }

    pub fn record_tool(&mut self, observation: &ToolObservation) -> Result<()> {
        self.store.append_tool(observation)?;
        self.transcript
            .push(Message::new("user", observation.as_context_message()));
        Ok(())
    }

    pub fn reset(&mut self) -> Result<()> {
        self.transcript.clear();
        self.summary = None;
        self.store.append_reset()?;
        self.refresh_context()
    }

    pub fn set_cwd(&mut self, cwd: PathBuf) -> Result<()> {
        self.cwd = cwd;
        self.transcript.clear();
        self.summary = None;
        self.store.append_cwd(&self.cwd)?;
        self.store.append_reset()?;
        self.refresh_context()
    }

    pub fn maybe_auto_compact(&mut self) -> Result<Option<usize>> {
        if transcript_bytes(&self.transcript) <= AUTO_COMPACT_BYTES {
            return Ok(None);
        }
        self.compact().map(Some)
    }

    pub fn compact(&mut self) -> Result<usize> {
        if self.transcript.len() <= COMPACT_KEEP_MESSAGES {
            return Ok(0);
        }

        let drop_count = self.transcript.len() - COMPACT_KEEP_MESSAGES;
        let dropped = self.transcript.drain(..drop_count).collect::<Vec<_>>();
        let next_summary = build_compaction_summary(self.summary.as_deref(), &dropped);
        self.summary = Some(next_summary.clone());
        self.store.append_compaction(&next_summary, drop_count)?;
        self.refresh_context()?;
        Ok(drop_count)
    }

    fn push_message(&mut self, message: Message) -> Result<()> {
        self.store.append_message(&message)?;
        self.transcript.push(message);
        Ok(())
    }

    fn refresh_context(&mut self) -> Result<()> {
        self.context_stats = context::build_context(&self.cwd, self.summary.as_deref())?.stats;
        Ok(())
    }
}

fn transcript_bytes(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| message.role.len() + message.content.len())
        .sum()
}

fn build_compaction_summary(previous: Option<&str>, dropped: &[Message]) -> String {
    let mut summary = String::new();
    if let Some(previous) = previous.filter(|value| !value.trim().is_empty()) {
        summary.push_str(previous.trim());
        summary.push_str("\n\n");
    }

    summary.push_str("Earlier Codeus conversation, compacted locally:\n");
    for message in dropped {
        let excerpt = excerpt(&message.content, 240);
        summary.push_str(&format!("- {}: {}\n", message.role, excerpt));
    }

    summary
}

fn excerpt(value: &str, max_chars: usize) -> String {
    let mut output = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect::<String>();
    if value.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}
