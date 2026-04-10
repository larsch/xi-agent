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
}

#[derive(Debug, serde::Deserialize)]
struct LegacySessionMeta {
    id: String,
    cwd: String,
}

#[derive(Debug, Default, serde::Deserialize)]
struct LegacySessionIndex {
    #[serde(default)]
    sessions: Vec<LegacySessionMeta>,
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

    fn open_at(sessions_dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&sessions_dir).with_context(|| {
            format!("Failed to create sessions dir: {}", sessions_dir.display())
        })?;

        let store = Self { sessions_dir };
        store.migrate_legacy_index_if_present()?;
        Ok(store)
    }

    pub fn create_session(&mut self, cwd: &str) -> anyhow::Result<String> {
        let session_id = new_session_id();
        let path = self.session_file_path(cwd, &session_id);
        self.write_session_messages(&path, &[])?;
        Ok(session_id)
    }

    pub fn save_messages(
        &mut self,
        session_id: &str,
        cwd: &str,
        messages: &[Message],
    ) -> anyhow::Result<()> {
        let target = self.session_file_path(cwd, session_id);
        self.write_session_messages(&target, messages)?;

        if let Some(existing_path) = self.find_session_file_by_id(session_id)?
            && !paths_are_same(&existing_path, &target)
        {
            fs::remove_file(&existing_path).with_context(|| {
                format!(
                    "Failed to remove stale duplicate session file: {}",
                    existing_path.display()
                )
            })?;
        }

        Ok(())
    }

    pub fn load_messages(&self, session_id: &str) -> anyhow::Result<Vec<Message>> {
        let Some(path) = self.find_session_file_by_id(session_id)? else {
            return Ok(vec![]);
        };

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

            let meta = SessionMeta {
                id: id.clone(),
                cwd: cwd.unwrap_or_default(),
                created_at_ms,
                updated_at_ms,
                message_count,
            };

            match by_id.get(&id) {
                Some(existing) if existing.updated_at_ms >= meta.updated_at_ms => {}
                _ => {
                    by_id.insert(id, meta);
                }
            }
        }

        let mut sessions = by_id.into_values().collect::<Vec<_>>();
        sessions.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
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

            if file_type.is_file() {
                if is_session_jsonl_file(&path) {
                    out.push((path, None));
                }
                continue;
            }

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

    fn migrate_legacy_index_if_present(&self) -> anyhow::Result<()> {
        let index_path = self.sessions_dir.join("index.json");
        if !index_path.exists() {
            return Ok(());
        }

        let raw = match fs::read_to_string(&index_path) {
            Ok(raw) => raw,
            Err(e) => {
                log::debug!(
                    "Could not read legacy session index {}: {}",
                    index_path.display(),
                    e
                );
                return Ok(());
            }
        };

        let index = match serde_json::from_str::<LegacySessionIndex>(&raw) {
            Ok(index) => index,
            Err(e) => {
                log::debug!(
                    "Could not parse legacy session index {}: {}",
                    index_path.display(),
                    e
                );
                return Ok(());
            }
        };

        for meta in index.sessions {
            let old_path = self.sessions_dir.join(format!("{}.jsonl", meta.id));
            if !old_path.exists() {
                continue;
            }

            let new_path = self.session_file_path(&meta.cwd, &meta.id);
            if let Some(parent) = new_path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create migrated session dir: {}",
                        parent.display()
                    )
                })?;
            }

            if new_path.exists() {
                if let Err(e) = fs::remove_file(&old_path) {
                    log::debug!(
                        "Failed to remove legacy session file {} after migration (target already exists {}): {}",
                        old_path.display(),
                        new_path.display(),
                        e
                    );
                }
                continue;
            }

            fs::rename(&old_path, &new_path).with_context(|| {
                format!(
                    "Failed to migrate session file {} -> {}",
                    old_path.display(),
                    new_path.display()
                )
            })?;
        }

        if let Err(e) = fs::remove_file(&index_path) {
            log::debug!(
                "Failed to remove legacy session index {}: {}",
                index_path.display(),
                e
            );
        }

        Ok(())
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

/// Return true when `a` and `b` refer to the same file-system entry.
///
/// On case-insensitive file systems (e.g. Windows NTFS) two `PathBuf`s that
/// differ only in case (`D%3A%5Ctoday` vs `d%3A%5Ctoday`) point to the same
/// file.  A plain `==` comparison on the raw path strings would incorrectly
/// treat them as different, causing `save_messages` to delete the file it just
/// wrote.  Canonicalising both paths resolves the true on-disk identity before
/// comparing.  Falls back to a plain equality check if canonicalisation fails
/// (e.g. if either path does not yet exist).
fn paths_are_same(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
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
        let mut store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");

        let cwd_a = "/a";
        let cwd_b = "/b";

        store
            .save_messages("20260328T120000-aaaaaaaa", cwd_a, &[Message::user("hello")])
            .expect("save a1");
        store
            .save_messages("20260328T120100-bbbbbbbb", cwd_b, &[Message::user("hi")])
            .expect("save b1");
        store
            .save_messages("20260328T120200-cccccccc", cwd_a, &[Message::user("newer")])
            .expect("save a2");

        let latest = store.latest_for_cwd(cwd_a).expect("latest for cwd a");
        assert_eq!(latest.id, "20260328T120200-cccccccc");
        assert_eq!(latest.cwd, cwd_a);
    }

    /// Regression test: saving a session whose cwd encodes to a directory name
    /// that differs only in case from the on-disk directory must not delete the
    /// file immediately after writing it.
    ///
    /// On Windows, NTFS is case-insensitive but the `PathBuf` string comparison
    /// used by the old code was case-sensitive.  A session first saved with
    /// `D:\today` created dir `D%3A%5Ctoday`; a subsequent save with `d:\today`
    /// computed target path `d%3A%5Ctoday\<id>.jsonl`, wrote the file (NTFS
    /// silently mapped it to the existing dir), then `find_session_file_by_id`
    /// returned the on-disk path with the original casing (`D%3A%5Ctoday\…`),
    /// which differed from the computed target string → the file was deleted.
    #[cfg(windows)]
    #[test]
    fn save_messages_does_not_delete_file_on_cwd_case_mismatch() {
        let tmp = tempdir().expect("tempdir");
        let mut store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");

        let id = "20260328T120000-casetest";

        // First save with uppercase drive letter — creates D%3A%5Ctoday dir.
        store
            .save_messages(id, "D:\\today", &[Message::user("first")])
            .expect("first save");

        // Second save with lowercase drive letter — must NOT delete the file.
        store
            .save_messages(id, "d:\\today", &[Message::user("first")])
            .expect("second save");

        // File must still exist and be loadable.
        let loaded = store.load_messages(id).expect("load after second save");
        assert_eq!(loaded.len(), 1, "session file was deleted after save");
    }

    #[cfg(windows)]
    #[test]
    fn latest_for_cwd_matches_case_and_separator_variants_on_windows() {
        let tmp = tempdir().expect("tempdir");
        let mut store = SessionStore::open_at(tmp.path().to_path_buf()).expect("open store");

        store
            .save_messages(
                "20260328T120000-aaaaaaaa",
                "D:\\today",
                &[Message::user("hello")],
            )
            .expect("save");

        let latest = store
            .latest_for_cwd("d:/today/")
            .expect("latest for normalized cwd");
        assert_eq!(latest.id, "20260328T120000-aaaaaaaa");
        assert_eq!(latest.cwd, "D:\\today");
    }

    #[test]
    fn migrates_legacy_index_flat_files_into_cwd_dirs() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();

        let legacy_id = "20260328T120000-12345678";
        fs::write(
            root.join(format!("{legacy_id}.jsonl")),
            format!(
                "{}\n",
                serde_json::to_string(&Message::user("hello")).expect("json")
            ),
        )
        .expect("write legacy file");

        fs::write(
            root.join("index.json"),
            r#"{"sessions":[{"id":"20260328T120000-12345678","cwd":"/legacy/cwd"}]}"#,
        )
        .expect("write legacy index");

        let store = SessionStore::open_at(root.to_path_buf()).expect("open store");

        let migrated = store.session_file_path("/legacy/cwd", legacy_id);
        assert!(migrated.exists());
        assert!(!root.join("index.json").exists());
    }
}
