use std::{fs, io::BufRead, path::PathBuf};

use anyhow::Context;
use chrono::Utc;

use crate::dirs::project_dirs;
use crate::llm::Message;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub cwd: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub message_count: usize,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct SessionIndex {
    sessions: Vec<SessionMeta>,
}

pub struct SessionStore {
    sessions_dir: PathBuf,
    index_path: PathBuf,
    index: SessionIndex,
}

impl SessionStore {
    pub fn open() -> anyhow::Result<Self> {
        let dirs = project_dirs().context("Could not resolve platform data directory for tau")?;
        let sessions_dir = dirs.data_dir().join("sessions");
        fs::create_dir_all(&sessions_dir).with_context(|| {
            format!("Failed to create sessions dir: {}", sessions_dir.display())
        })?;

        let index_path = sessions_dir.join("index.json");
        let index = if index_path.exists() {
            let raw = fs::read_to_string(&index_path)
                .with_context(|| format!("Failed to read index: {}", index_path.display()))?;
            serde_json::from_str::<SessionIndex>(&raw).with_context(|| {
                format!(
                    "Failed to parse session index JSON: {}",
                    index_path.display()
                )
            })?
        } else {
            SessionIndex::default()
        };

        Ok(Self {
            sessions_dir,
            index_path,
            index,
        })
    }

    pub fn create_session(&mut self, cwd: &str) -> anyhow::Result<String> {
        let session_id = new_session_id();
        let now = Utc::now().timestamp_millis();
        self.index.sessions.push(SessionMeta {
            id: session_id.clone(),
            cwd: cwd.to_string(),
            created_at_ms: now,
            updated_at_ms: now,
            message_count: 0,
        });
        self.save_index()?;
        self.write_session_messages(&session_id, &[])?;
        Ok(session_id)
    }

    pub fn save_messages(
        &mut self,
        session_id: &str,
        cwd: &str,
        messages: &[Message],
    ) -> anyhow::Result<()> {
        let now = Utc::now().timestamp_millis();
        match self.index.sessions.iter_mut().find(|s| s.id == session_id) {
            Some(meta) => {
                meta.cwd = cwd.to_string();
                meta.updated_at_ms = now;
                meta.message_count = messages.len();
            }
            None => {
                self.index.sessions.push(SessionMeta {
                    id: session_id.to_string(),
                    cwd: cwd.to_string(),
                    created_at_ms: now,
                    updated_at_ms: now,
                    message_count: messages.len(),
                });
            }
        }

        self.write_session_messages(session_id, messages)?;
        self.save_index()?;
        Ok(())
    }

    pub fn load_messages(&self, session_id: &str) -> anyhow::Result<Vec<Message>> {
        let path = self.session_file_path(session_id);
        if !path.exists() {
            return Ok(vec![]);
        }

        let file = fs::File::open(&path)
            .with_context(|| format!("Failed to open session file: {}", path.display()))?;
        let reader = std::io::BufReader::new(file);

        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Message>(line) {
                Ok(msg) => out.push(msg),
                Err(e) => {
                    log::debug!(
                        "skipping invalid session message line in {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }
        Ok(out)
    }

    pub fn list_sessions(&self) -> Vec<SessionMeta> {
        let mut sessions = self.index.sessions.clone();
        sessions.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
        sessions
    }

    pub fn latest_for_cwd(&self, cwd: &str) -> Option<SessionMeta> {
        self.index
            .sessions
            .iter()
            .filter(|s| s.cwd == cwd && s.message_count > 0)
            .max_by_key(|s| s.updated_at_ms)
            .cloned()
    }

    fn write_session_messages(&self, session_id: &str, messages: &[Message]) -> anyhow::Result<()> {
        let path = self.session_file_path(session_id);
        let mut body = String::new();
        for msg in messages {
            body.push_str(&serde_json::to_string(msg)?);
            body.push('\n');
        }
        fs::write(&path, body)
            .with_context(|| format!("Failed to write session file: {}", path.display()))?;
        Ok(())
    }

    fn save_index(&self) -> anyhow::Result<()> {
        let body = serde_json::to_string_pretty(&self.index)?;
        fs::write(&self.index_path, body).with_context(|| {
            format!(
                "Failed to write session index file: {}",
                self.index_path.display()
            )
        })?;
        Ok(())
    }

    fn session_file_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{session_id}.jsonl"))
    }
}

fn new_session_id() -> String {
    let ts = Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let mut bytes = [0u8; 4];
    if getrandom::getrandom(&mut bytes).is_err() {
        return format!("{ts}-00000000");
    }
    let suffix = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!("{ts}-{suffix}")
}
