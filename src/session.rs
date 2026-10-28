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
    /// First line of the first user prompt in this session, if any.
    pub first_prompt: Option<String>,
}

pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    pub fn open() -> anyhow::Result<Self> {
        let dirs =
            project_dirs().context("Could not resolve platform data directory for xi-agent")?;
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

            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            let cwd_value = cwd.as_deref().unwrap_or_default();
            let has_stash = self.stash_file_path(cwd_value, &id).exists();
            if metadata.len() == 0 && !has_stash {
                continue;
            }

            let file_updated_at_ms = file_modified_at_ms(&path).max(
                has_stash
                    .then(|| file_modified_at_ms(&self.stash_file_path(cwd_value, &id)))
                    .flatten(),
            );
            let session_created_at_ms = session_id_created_at_ms(&id);
            let updated_at_ms = file_updated_at_ms
                .zip(session_created_at_ms)
                .map(|(file_ts, id_ts)| file_ts.max(id_ts))
                .or(file_updated_at_ms)
                .or(session_created_at_ms)
                .unwrap_or_else(|| Utc::now().timestamp_millis());
            let created_at_ms = session_created_at_ms.unwrap_or(updated_at_ms);

            let (first_prompt, has_content) = read_first_user_prompt(&path);
            if !has_content && !has_stash {
                continue;
            }

            let meta = SessionMeta {
                id: id.clone(),
                cwd: cwd.unwrap_or_default(),
                created_at_ms,
                updated_at_ms,
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

    /// Load the per-session draft stash. A missing sidecar means an empty stash.
    pub fn load_stash(&self, cwd: &str, session_id: &str) -> anyhow::Result<Vec<String>> {
        let path = self.stash_file_path(cwd, session_id);
        match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents)
                .with_context(|| format!("Failed to parse draft stash: {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error)
                .with_context(|| format!("Failed to read draft stash: {}", path.display())),
        }
    }

    /// Load a stash by session ID, resolving its stored working directory.
    pub fn load_stash_for_session(&self, session_id: &str) -> anyhow::Result<Vec<String>> {
        let cwd = self.cwd_for_session(session_id)?;
        self.load_stash(&cwd, session_id)
    }

    fn cwd_for_session(&self, session_id: &str) -> anyhow::Result<String> {
        let path = self
            .find_session_file_by_id(session_id)?
            .ok_or_else(|| anyhow::anyhow!("no session file found for session {session_id}"))?;
        let key = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid session path for session {session_id}"))?;
        decode_cwd_key(key)
            .ok_or_else(|| anyhow::anyhow!("invalid session working directory for {session_id}"))
    }

    /// Save a stash by session ID using its recorded working directory.
    pub fn save_stash_for_session(
        &self,
        session_id: &str,
        drafts: &[String],
    ) -> anyhow::Result<()> {
        let cwd = self.cwd_for_session(session_id)?;
        self.save_stash(&cwd, session_id, drafts)
    }

    /// Durably replace the per-session draft stash, removing its sidecar when empty.
    pub fn save_stash(&self, cwd: &str, session_id: &str, drafts: &[String]) -> anyhow::Result<()> {
        let path = self.stash_file_path(cwd, session_id);
        if drafts.is_empty() {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error)
                    .with_context(|| format!("Failed to remove draft stash: {}", path.display())),
            }
        } else {
            let contents = serde_json::to_string(drafts)?;
            crate::atomic_file::save_atomic(&path, &contents)
                .with_context(|| format!("Failed to save draft stash: {}", path.display()))
        }
    }

    pub fn latest_for_cwd(&self, cwd: &str) -> Option<SessionMeta> {
        let needle = normalize_cwd_for_match(cwd);

        // Cheap scan: only directory entries + file metadata. Session files are
        // JSONL of non-empty message lines, so `len > 0` means the session has
        // at least one message (an empty file was created but never written).
        // This avoids the full-content reads that `list_sessions` performs, so
        // the startup resume-availability check stays fast regardless of how
        // much session history has accumulated.
        let mut best: Option<(i64, i64, String, String)> = None;
        let entries = self.collect_session_files().ok()?;
        for (path, file_cwd) in entries {
            let Some(decoded_cwd) = file_cwd else {
                continue;
            };
            if normalize_cwd_for_match(&decoded_cwd) != needle {
                continue;
            }
            let Some(id) = session_id_from_path(&path) else {
                continue;
            };
            let has_stash = self.stash_file_path(&decoded_cwd, &id).exists();
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if metadata.len() == 0 && !has_stash {
                continue;
            }
            let file_updated = file_modified_at_ms(&path).max(
                has_stash
                    .then(|| file_modified_at_ms(&self.stash_file_path(&decoded_cwd, &id)))
                    .flatten(),
            );
            let created = session_id_created_at_ms(&id);
            let updated = file_updated
                .zip(created)
                .map(|(f, i)| f.max(i))
                .or(file_updated)
                .or(created)
                .unwrap_or_else(|| Utc::now().timestamp_millis());
            let created = created.unwrap_or(updated);

            let candidate = (updated, created, id, decoded_cwd);
            let is_newer = match &best {
                Some(current) => {
                    (candidate.0, candidate.1, &candidate.2) > (current.0, current.1, &current.2)
                }
                None => true,
            };
            if is_newer {
                best = Some(candidate);
            }
        }

        best.map(|(updated, created, id, cwd)| SessionMeta {
            id,
            cwd,
            created_at_ms: created,
            updated_at_ms: updated,
            first_prompt: None,
        })
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

    fn stash_file_path(&self, cwd: &str, session_id: &str) -> PathBuf {
        self.sessions_dir
            .join(cwd_key(cwd))
            .join(format!("{session_id}.stash.json"))
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
    if getrandom::fill(&mut bytes).is_err() {
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

/// Read the first user prompt and note whether the log contains any nonblank line.
fn read_first_user_prompt(path: &Path) -> (Option<String>, bool) {
    let Ok(file) = fs::File::open(path) else {
        return (None, false);
    };
    let reader = std::io::BufReader::new(file);
    let mut has_content = false;
    for line in reader.lines() {
        let Ok(line) = line else {
            return (None, has_content);
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        has_content = true;

        if let Ok(crate::session_event::SessionEvent::UserMessage { content, .. }) =
            serde_json::from_str::<crate::session_event::SessionEvent>(line)
            && let Some(first_line) = content.lines().find(|l| !l.trim().is_empty())
        {
            return (Some(first_line.trim().to_string()), true);
        }
    }
    (None, has_content)
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

        let cwd = "/home/larsch/prj/xi-agent";
        let path = store.session_file_path(cwd, "20260328T120000-deadbeef");
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());

        assert_eq!(parent, Some("%2Fhome%2Flarsch%2Fprj%2Fxi-agent"));
    }

    #[test]
    fn latest_for_cwd_only_looks_at_matching_directory() {
        let tmp = tempdir().expect("tempdir");
        let store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");

        let cwd_a = "/a";
        let cwd_b = "/b";

        // Seed three sessions with a minimal event log line so they are resumable.
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
    fn stash_sidecar_round_trips_and_is_removed_when_empty() {
        let tmp = tempdir().expect("tempdir");
        let mut store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");
        let id = store.create_session("/repo").expect("create session");
        let drafts = vec!["draft one".to_string(), "draft two".to_string()];
        store.save_stash("/repo", &id, &drafts).expect("save stash");
        assert_eq!(store.load_stash("/repo", &id).unwrap(), drafts);
        store.save_stash("/repo", &id, &[]).expect("remove stash");
        assert!(store.load_stash("/repo", &id).unwrap().is_empty());
        assert!(!store.stash_file_path("/repo", &id).exists());
    }

    #[test]
    fn stash_only_session_is_listed_and_latest_for_cwd() {
        let tmp = tempdir().expect("tempdir");
        let mut store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");
        let id = store.create_session("/repo").expect("create session");
        store
            .save_stash("/repo", &id, &["recover me".to_string()])
            .expect("save stash");
        let session = store
            .list_sessions()
            .into_iter()
            .find(|meta| meta.id == id)
            .expect("stash-only session listed");
        assert!(session.first_prompt.is_none());
        assert_eq!(store.latest_for_cwd("/repo").unwrap().id, id);
    }

    #[test]
    fn blank_only_log_is_not_listed_without_stash() {
        let tmp = tempdir().expect("tempdir");
        let mut store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");
        let id = store.create_session("/repo").expect("create session");
        fs::write(store.session_file_path("/repo", &id), " \n\t\n").unwrap();
        assert!(store.list_sessions().iter().all(|session| session.id != id));

        store
            .save_stash("/repo", &id, &["draft".to_string()])
            .unwrap();
        assert!(store.list_sessions().iter().any(|session| session.id == id));
    }

    #[test]
    fn malformed_stash_is_reported() {
        let tmp = tempdir().expect("tempdir");
        let mut store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");
        let id = store.create_session("/repo").expect("create session");
        fs::write(store.stash_file_path("/repo", &id), "not json").unwrap();
        assert!(store.load_stash("/repo", &id).is_err());
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
