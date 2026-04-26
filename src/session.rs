use std::{
    collections::HashMap,
    fs,
    io::BufRead,
    path::{Path, PathBuf},
};

use anyhow::Context;
use chrono::{NaiveDateTime, Utc};

use crate::dirs::project_dirs;
use crate::llm::Message;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub cwd: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub message_count: usize,
    /// First line of the first user prompt in this session, if any.
    pub first_prompt: Option<String>,
}

pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    pub fn open() -> anyhow::Result<Self> {
        let dirs = project_dirs().context("Could not resolve platform data directory for tau")?;
        let sessions_dir = dirs.data_dir().join("sessions");
        Self::open_at(sessions_dir)
    }

    pub(crate) fn open_at(sessions_dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&sessions_dir).with_context(|| {
            format!("Failed to create sessions dir: {}", sessions_dir.display())
        })?;

        let store = Self { sessions_dir };
        Ok(store)
    }

    pub fn create_session(&mut self, cwd: &str) -> anyhow::Result<String> {
        let session_id = new_session_id();
        let path = self.session_file_path(cwd, &session_id);
        self.write_session_messages(&path, &[])?;
        Ok(session_id)
    }

    pub fn list_sessions(&self) -> Vec<SessionMeta> {
        let mut by_id: HashMap<String, SessionMeta> = HashMap::new();

        let entries = match self.collect_session_files() {
            Ok(entries) => entries,
            Err(e) => {
                log::debug!("Failed to scan sessions directory: {e}");
                return vec![];
            }
        };

        for (path, cwd) in entries {
            let Some(id) = session_id_from_path(&path) else {
                continue;
            };

            let message_count = match count_non_empty_lines(&path) {
                Ok(count) => count,
                Err(e) => {
                    log::debug!(
                        "Failed to count session messages for {}: {}",
                        path.display(),
                        e
                    );
                    continue;
                }
            };

            let file_updated_at_ms = file_modified_at_ms(&path);
            let session_created_at_ms = session_id_created_at_ms(&id);
            let updated_at_ms = file_updated_at_ms
                .zip(session_created_at_ms)
                .map(|(file_ts, id_ts)| file_ts.max(id_ts))
                .or(file_updated_at_ms)
                .or(session_created_at_ms)
                .unwrap_or_else(|| Utc::now().timestamp_millis());
            let created_at_ms = session_created_at_ms.unwrap_or(updated_at_ms);

            let first_prompt = read_first_user_prompt(&path);

            let meta = SessionMeta {
                id: id.clone(),
                cwd: cwd.unwrap_or_default(),
                created_at_ms,
                updated_at_ms,
                message_count,
                first_prompt,
            };

            match by_id.get(&id) {
                Some(existing) if existing.updated_at_ms >= meta.updated_at_ms => {}
                _ => {
                    by_id.insert(id, meta);
                }
            }
        }

        let mut sessions = by_id.into_values().collect::<Vec<_>>();
        sessions.sort_by_key(|b| std::cmp::Reverse(b.updated_at_ms));
        sessions
    }

    pub fn latest_for_cwd(&self, cwd: &str) -> Option<SessionMeta> {
        let needle = normalize_cwd_for_match(cwd);
        self.list_sessions()
            .into_iter()
            .filter(|s| normalize_cwd_for_match(&s.cwd) == needle && s.message_count > 0)
            .max_by_key(|s| (s.updated_at_ms, s.created_at_ms, s.id.clone()))
    }

    fn write_session_messages(&self, path: &Path, messages: &[Message]) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create session directory: {}", parent.display())
            })?;
        }

        let mut body = String::new();
        for msg in messages {
            body.push_str(&serde_json::to_string(msg)?);
            body.push('\n');
        }
        fs::write(path, body)
            .with_context(|| format!("Failed to write session file: {}", path.display()))?;
        Ok(())
    }

    fn session_file_path(&self, cwd: &str, session_id: &str) -> PathBuf {
        self.sessions_dir
            .join(cwd_key(cwd))
            .join(format!("{session_id}.jsonl"))
    }

    /// Resolve the JSONL path for a session's event log.
    ///
    /// If a file already exists for `session_id` (located by scanning the
    /// sessions directory), that path is returned so that the event log reuses
    /// the same file.  For sessions that have no file yet, a path under an
    /// `_unknown_cwd` bucket is returned as a fallback; it will be superseded
    /// once the session is associated with a real cwd on the next
    /// `create_session` call.
    pub(crate) fn resolve_event_log_path(&self, session_id: &str) -> anyhow::Result<PathBuf> {
        if let Some(existing) = self.find_session_file_by_id(session_id)? {
            return Ok(existing);
        }
        Ok(self
            .sessions_dir
            .join("_unknown_cwd")
            .join(format!("{session_id}.jsonl")))
    }

    fn find_session_file_by_id(&self, session_id: &str) -> anyhow::Result<Option<PathBuf>> {
        let mut newest: Option<(PathBuf, i64)> = None;

        for (path, _) in self.collect_session_files()? {
            if session_id_from_path(&path).as_deref() != Some(session_id) {
                continue;
            }
            let ts = file_modified_at_ms(&path).unwrap_or(0);
            match &newest {
                Some((_, best_ts)) if *best_ts >= ts => {}
                _ => newest = Some((path, ts)),
            }
        }

        Ok(newest.map(|(path, _)| path))
    }

    fn collect_session_files(&self) -> anyhow::Result<Vec<(PathBuf, Option<String>)>> {
        let mut out = Vec::new();

        for entry in fs::read_dir(&self.sessions_dir).with_context(|| {
            format!(
                "Failed to read sessions directory: {}",
                self.sessions_dir.display()
            )
        })? {
            let entry = entry?;
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            if !file_type.is_dir() {
                continue;
            }

            let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(decoded_cwd) = decode_cwd_key(dir_name) else {
                continue;
            };

            let subdir_entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(e) => {
                    log::debug!(
                        "Failed to read session cwd directory {}: {}",
                        path.display(),
                        e
                    );
                    continue;
                }
            };

            for file_entry in subdir_entries {
                let Ok(file_entry) = file_entry else {
                    continue;
                };
                let file_path = file_entry.path();
                if !is_session_jsonl_file(&file_path) {
                    continue;
                }
                out.push((file_path, Some(decoded_cwd.clone())));
            }
        }

        Ok(out)
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

fn cwd_key(cwd: &str) -> String {
    urlencoding::encode(cwd).into_owned()
}

fn normalize_cwd_for_match(cwd: &str) -> String {
    #[cfg(windows)]
    {
        let mut out = cwd.replace('/', "\\").to_ascii_lowercase();
        while out.ends_with('\\') && out.len() > 3 {
            out.pop();
        }
        out
    }

    #[cfg(not(windows))]
    {
        cwd.to_string()
    }
}

fn decode_cwd_key(key: &str) -> Option<String> {
    urlencoding::decode(key).ok().map(|s| s.into_owned())
}

fn session_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()?.to_str().map(ToOwned::to_owned)
}

fn is_session_jsonl_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
}

fn count_non_empty_lines(path: &Path) -> anyhow::Result<usize> {
    let file = fs::File::open(path).with_context(|| {
        format!(
            "Failed to open session file for counting: {}",
            path.display()
        )
    })?;
    let reader = std::io::BufReader::new(file);
    let mut count = 0usize;
    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            count += 1;
        }
    }
    Ok(count)
}

/// Read the first line of the first user prompt in a session file.
fn read_first_user_prompt(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines() {
        let line = line.ok()?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Ok(crate::session_event::SessionEvent::UserMessage { content, .. }) =
            serde_json::from_str::<crate::session_event::SessionEvent>(line)
        {
            let first_line = content
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string());
            if first_line.is_some() {
                return first_line;
            }
        }
    }
    None
}

fn file_modified_at_ms(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let elapsed = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(elapsed.as_millis()).ok()
}

fn session_id_created_at_ms(session_id: &str) -> Option<i64> {
    let ts = session_id.split_once('-')?.0;
    let naive = NaiveDateTime::parse_from_str(ts, "%Y%m%dT%H%M%S").ok()?;
    Some(naive.and_utc().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_files_are_grouped_by_encoded_cwd() {
        let tmp = tempdir().expect("tempdir");
        let store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");

        let cwd = "/home/larsch/prj/tau";
        let path = store.session_file_path(cwd, "20260328T120000-deadbeef");
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());

        assert_eq!(parent, Some("%2Fhome%2Flarsch%2Fprj%2Ftau"));
    }

    #[test]
    fn latest_for_cwd_only_looks_at_matching_directory() {
        let tmp = tempdir().expect("tempdir");
        let store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");

        let cwd_a = "/a";
        let cwd_b = "/b";

        // Seed three sessions by creating them and writing a minimal event log
        // line so they register as non-empty (message_count > 0).
        let minimal_event = "{\"type\":\"user_message\",\"content\":\"hi\",\"timestamp\":1}\n";
        for (id, cwd) in [
            ("20260328T120000-aaaaaaaa", cwd_a),
            ("20260328T120100-bbbbbbbb", cwd_b),
            ("20260328T120200-cccccccc", cwd_a),
        ] {
            let path = store.session_file_path(cwd, id);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, minimal_event).unwrap();
        }

        let latest = store.latest_for_cwd(cwd_a).expect("latest for cwd a");
        assert_eq!(latest.id, "20260328T120200-cccccccc");
        assert_eq!(latest.cwd, cwd_a);
    }

    #[test]
    fn first_prompt_is_read_from_event_log_user_message() {
        let tmp = tempdir().expect("tempdir");
        let mut store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");

        let session_id = store.create_session("/repo").expect("create session");
        let path = store.session_file_path("/repo", &session_id);
        fs::write(
            &path,
            concat!(
                "{\"type\":\"model_changed\",\"model\":\"gpt\",\"provider\":\"openai\",\"timestamp\":1}\n",
                "{\"type\":\"user_message\",\"content\":\"\\nFirst prompt line\\nSecond line\",\"timestamp\":2}\n",
                "{\"type\":\"assistant_message\",\"content\":\"hi\",\"thinking\":null,\"phase\":\"final\",\"usage\":null,\"timestamp\":3}\n"
            ),
        )
        .expect("write event log");

        let meta = store
            .list_sessions()
            .into_iter()
            .find(|s| s.id == session_id)
            .expect("session metadata");

        assert_eq!(meta.first_prompt.as_deref(), Some("First prompt line"));
    }
}
