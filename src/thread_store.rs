use crate::api::Message;
use crate::config;
use crate::tools::ToolObservation;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct ThreadStore {
    id: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct StoredThread {
    pub id: String,
    pub cwd: PathBuf,
    pub transcript: Vec<Message>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub id: String,
    pub path: PathBuf,
    pub updated_at: u64,
    pub size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ThreadRecord {
    Meta {
        thread_id: String,
        cwd: PathBuf,
        created_at: u64,
    },
    Message {
        role: String,
        content: String,
        tool_call_id: Option<String>,
        tool_calls: Option<Vec<crate::api::ChatToolCall>>,
        timestamp: u64,
    },
    Compaction {
        summary: String,
        dropped_messages: usize,
        timestamp: u64,
    },
    Cwd {
        cwd: PathBuf,
        timestamp: u64,
    },
    Reset {
        timestamp: u64,
    },
    Tool {
        observation: ToolObservation,
        timestamp: u64,
    },
}

impl ThreadStore {
    pub fn create(cwd: &Path) -> Result<Self> {
        let id = new_thread_id();
        let path = threads_dir()?.join(format!("{id}.jsonl"));
        let store = Self { id, path };
        store.append(&ThreadRecord::Meta {
            thread_id: store.id.clone(),
            cwd: cwd.to_path_buf(),
            created_at: now_secs(),
        })?;
        Ok(store)
    }

    pub fn open(id: &str) -> Result<(Self, StoredThread)> {
        let path = threads_dir()?.join(format!("{id}.jsonl"));
        let stored = read_thread_file(&path)?;
        Ok((
            Self {
                id: stored.id.clone(),
                path,
            },
            stored,
        ))
    }

    pub fn open_latest_excluding(
        excluded_id: Option<&str>,
    ) -> Result<Option<(Self, StoredThread)>> {
        let Some(info) = list_threads()?
            .into_iter()
            .find(|info| Some(info.id.as_str()) != excluded_id)
        else {
            return Ok(None);
        };

        Self::open(&info.id).map(Some)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn append_message(&self, message: &Message) -> Result<()> {
        self.append(&ThreadRecord::Message {
            role: message.role.clone(),
            content: message.content.clone(),
            tool_call_id: message.tool_call_id.clone(),
            tool_calls: message.tool_calls.clone(),
            timestamp: now_secs(),
        })
    }

    pub fn append_compaction(&self, summary: &str, dropped_messages: usize) -> Result<()> {
        self.append(&ThreadRecord::Compaction {
            summary: summary.to_string(),
            dropped_messages,
            timestamp: now_secs(),
        })
    }

    pub fn append_cwd(&self, cwd: &Path) -> Result<()> {
        self.append(&ThreadRecord::Cwd {
            cwd: cwd.to_path_buf(),
            timestamp: now_secs(),
        })
    }

    pub fn append_reset(&self) -> Result<()> {
        self.append(&ThreadRecord::Reset {
            timestamp: now_secs(),
        })
    }

    pub fn append_tool(&self, observation: &ToolObservation) -> Result<()> {
        self.append(&ThreadRecord::Tool {
            observation: observation.clone(),
            timestamp: now_secs(),
        })
    }

    fn append(&self, record: &ThreadRecord) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        serde_json::to_writer(&mut file, record).context("failed to serialize thread record")?;
        writeln!(file).context("failed to write thread record")?;
        Ok(())
    }
}

pub fn list_threads() -> Result<Vec<ThreadInfo>> {
    let dir = threads_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut threads = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }

        let metadata = entry.metadata()?;
        let updated_at = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };

        threads.push(ThreadInfo {
            id: id.to_string(),
            path,
            updated_at,
            size: metadata.len(),
        });
    }

    threads.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(threads)
}

fn read_thread_file(path: &Path) -> Result<StoredThread> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut cwd = std::env::current_dir().context("failed to read current directory")?;
    let mut transcript = Vec::new();
    let mut summary = None;

    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: ThreadRecord = serde_json::from_str(line)
            .with_context(|| format!("failed to parse {} line {}", path.display(), index + 1))?;
        match record {
            ThreadRecord::Meta {
                thread_id,
                cwd: meta_cwd,
                ..
            } => {
                id = thread_id;
                cwd = meta_cwd;
            }
            ThreadRecord::Message {
                role,
                content,
                tool_call_id,
                tool_calls,
                ..
            } => transcript.push(Message {
                role,
                content,
                tool_call_id,
                tool_calls,
            }),
            ThreadRecord::Compaction {
                summary: next_summary,
                ..
            } => {
                summary = Some(next_summary);
                transcript.clear();
            }
            ThreadRecord::Cwd { cwd: next_cwd, .. } => cwd = next_cwd,
            ThreadRecord::Reset { .. } => {
                transcript.clear();
                summary = None;
            }
            ThreadRecord::Tool { observation, .. } => transcript.push(Message {
                role: "user".to_string(),
                content: observation.as_context_message(),
                tool_call_id: None,
                tool_calls: None,
            }),
        }
    }

    Ok(StoredThread {
        id,
        cwd,
        transcript,
        summary,
    })
}

pub fn threads_dir() -> Result<PathBuf> {
    let path = config::config_path()?;
    let parent = path.parent().context("config path has no parent")?;
    Ok(parent.join("threads"))
}

fn new_thread_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("{}-{}", millis, std::process::id())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
