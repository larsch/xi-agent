use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
    time::SystemTime,
};

use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};

/// Maximum number of diff lines to inline in the notification message.
/// If the diff exceeds this, only a warning (no diff) is included.
pub const DIFF_INLINE_MAX_LINES: usize = 50;

/// Snapshot of a file at the time the agent last touched it.
#[derive(Clone)]
struct FileSnapshot {
    mtime: SystemTime,
    hash: [u8; 32],
    /// UTF-8 content of the file; used to produce diffs on change.
    content: String,
}

/// A file that was modified externally since the agent last touched it.
pub struct ChangedFile {
    pub path: PathBuf,
    pub old_content: String,
    pub new_content: String,
}

/// Outcome of checking whether a tracked file is stale relative to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staleness {
    /// The file was modified on disk since it was last read or written by
    /// the agent.
    Stale {
        mod_time: SystemTime,
        read_time: SystemTime,
    },
    /// The file has not changed since it was last read or written.
    Current,
    /// The file has never been read or written by the agent in this
    /// session.
    NeverRead,
}

/// Tracks files touched by the agent's file tools and detects external
/// modifications using a two-stage check: mtime first, then SHA-256.
///
/// Paths can be excluded from tracking in two ways:
/// - **Prefix exclusions**: any path whose canonical prefix matches one of the
///   entries in `excluded_prefixes` is silently skipped.  Use this for
///   directories such as the session store or debug-log directory.
/// - **Filename exclusions**: any path whose file name (last component) matches
///   one of the entries in `excluded_filenames` is silently skipped.  Use this
///   for instruction files such as `AGENTS.md` and `SKILL.md` that should not
///   trigger change notifications.
#[derive(Default)]
pub struct FileTracker {
    files: HashMap<PathBuf, FileSnapshot>,
    /// Directory prefixes whose contents should never be tracked.
    excluded_prefixes: Vec<PathBuf>,
    /// Exact file names (last path component) that should never be tracked.
    excluded_filenames: HashSet<OsString>,
}

impl FileTracker {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a tracker that ignores paths under the given directory prefixes
    /// and paths whose file name matches any entry in `filenames`.
    ///
    /// `filenames` entries are matched against `path.file_name()` only (the
    /// last component), so `"AGENTS.md"` matches `/any/dir/AGENTS.md`.
    pub fn with_exclusions(excluded_prefixes: Vec<PathBuf>, excluded_filenames: &[&str]) -> Self {
        Self {
            files: HashMap::new(),
            excluded_prefixes,
            excluded_filenames: excluded_filenames.iter().map(OsString::from).collect(),
        }
    }

    /// Returns `true` when `path` should be silently ignored by [`record`].
    fn is_excluded(&self, path: &Path) -> bool {
        // Filename-based exclusion (e.g. AGENTS.md, SKILL.md).
        if let Some(name) = path.file_name()
            && self.excluded_filenames.contains(name)
        {
            return true;
        }

        // Prefix-based exclusion (e.g. sessions dir, cache dir).
        for prefix in &self.excluded_prefixes {
            if path.starts_with(prefix) {
                return true;
            }
        }

        false
    }

    /// Record the current state of `path`. Called after a successful
    /// `read_file`, `write_file`, or `edit_file` tool execution.
    ///
    /// Non-UTF-8 files are silently skipped (binary files are not tracked).
    /// Paths matching the configured exclusions are also silently skipped.
    pub fn record(&mut self, path: &Path) {
        if self.is_excluded(path) {
            log::debug!("file_tracker: skipping excluded path {}", path.display());
            return;
        }
        match snapshot(path) {
            Ok(snap) => {
                log::info!(
                    "file_tracker: record({}) mtime={:?}",
                    path.display(),
                    snap.mtime
                );
                self.files.insert(path.to_path_buf(), snap);
            }
            Err(e) => {
                log::warn!("file_tracker: could not record {}: {e}", path.display());
            }
        }
    }

    /// Check whether a tracked file has been modified on disk since it was
    /// last read or written.
    ///
    /// Returns [`Staleness::NeverRead`] when `path` has never been recorded,
    /// [`Staleness::Stale`] when the disk mtime is newer than the recorded
    /// mtime **and** the content hash differs, and [`Staleness::Current`]
    /// when the two match or when only the mtime bumped without content change.
    pub fn staleness(&self, path: &Path) -> Staleness {
        let Some(snap) = self.files.get(path) else {
            log::info!(
                "file_tracker: staleness({}) → NeverRead (not in tracker)",
                path.display()
            );
            return Staleness::NeverRead;
        };

        let disk_mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(snap.mtime);

        log::info!(
            "file_tracker: staleness({}) disk={:?} snap={:?} {}",
            path.display(),
            disk_mtime,
            snap.mtime,
            if disk_mtime > snap.mtime {
                "→ Stale"
            } else {
                "→ Current"
            }
        );

        if disk_mtime > snap.mtime {
            // mtime changed — verify content actually changed via hash.
            // Avoids false-positives when filesystems report slightly
            // different mtimes for the same content after a write + stat.
            match std::fs::read_to_string(path) {
                Ok(new_content) => {
                    let new_hash = hash_content(&new_content);
                    if new_hash == snap.hash {
                        log::info!(
                            "file_tracker: staleness({}) mtime changed but content identical → Current",
                            path.display()
                        );
                        Staleness::Current
                    } else {
                        log::info!(
                            "file_tracker: staleness({}) → Stale (content changed)",
                            path.display()
                        );
                        Staleness::Stale {
                            mod_time: disk_mtime,
                            read_time: snap.mtime,
                        }
                    }
                }
                Err(_) => {
                    // Can't read the file — assume stale.
                    log::info!(
                        "file_tracker: staleness({}) → Stale (mtime changed, file unreadable)",
                        path.display()
                    );
                    Staleness::Stale {
                        mod_time: disk_mtime,
                        read_time: snap.mtime,
                    }
                }
            }
        } else {
            Staleness::Current
        }
    }

    /// Check whether `path` is tracked by git.
    ///
    /// Uses `git ls-files --error-unmatch <path>` to determine tracking
    /// status. The git repo root is discovered once per call via
    /// `git rev-parse --show-toplevel` in the file's parent directory.
    /// Returns `false` when git is not installed, the file is not in a
    /// repo, or the path resolves to a gitignored or untracked file.
    pub fn is_git_tracked(&mut self, path: &Path) -> bool {
        // Ensure the path is absolute so git can resolve it.
        let path = match std::fs::canonicalize(path) {
            Ok(p) => p,
            Err(_) => return false,
        };

        // Discover the repo root from the file's directory, not cwd.
        let repo_root = match discover_git_root(path.parent().unwrap_or(&path)) {
            Some(r) => r,
            None => return false,
        };

        // git ls-files --error-unmatch exits 0 for tracked, 1 for
        // untracked/ignored.
        std::process::Command::new("git")
            .args(["ls-files", "--error-unmatch"])
            .arg(&path)
            .current_dir(&repo_root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Discard all tracked file snapshots.
    ///
    /// Call this when starting a new session so that file changes from the
    /// previous session are not detected as external modifications.
    pub fn reset(&mut self) {
        self.files.clear();
    }

    /// Absorb any file changes that occurred since the last snapshot without
    /// reporting them as external modifications.
    ///
    /// Call this whenever the agent pauses for user input (end of agent run,
    /// after a tool-call batch, or just before awaiting an `ask_user` reply).
    /// Only changes made *during the subsequent user-input window* will be
    /// reported by the next [`check_modified`] call.
    ///
    /// Uses an mtime fast-path identical to [`check_modified`]: files whose
    /// mtime has not changed are skipped entirely (no read, no hash).
    pub fn refresh_baselines(&mut self) {
        for (path, snap) in &mut self.files {
            // Stat first; if mtime hasn't changed the stored snapshot is still
            // valid and there is nothing to absorb.
            let new_mtime = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(snap.mtime);

            if new_mtime == snap.mtime {
                continue;
            }

            // mtime changed — re-read and re-hash to update the baseline.
            match snapshot(path) {
                Ok(new_snap) => {
                    *snap = new_snap;
                }
                Err(e) => {
                    log::warn!(
                        "file_tracker: could not refresh baseline for {}: {e}",
                        path.display()
                    );
                }
            }
        }
    }

    /// Check all tracked paths for external modifications.
    ///
    /// A file is considered externally modified when its mtime has changed
    /// **and** its content hash has changed (mtime-only bumps are ignored).
    ///
    /// Returns one [`ChangedFile`] per modified path.  Snapshots are **not**
    /// updated — call [`accept_changes`] to update the snapshots that were
    /// acted upon.
    pub fn check_modified(&self) -> Vec<ChangedFile> {
        let mut changed = Vec::new();

        for (path, snap) in &self.files {
            // Fast path: stat only.
            let meta = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(e) => {
                    log::debug!("file_tracker: could not stat {}: {e}", path.display());
                    continue;
                }
            };

            let new_mtime = match meta.modified() {
                Ok(t) => t,
                Err(_) => continue, // platform doesn't support mtime
            };

            if new_mtime == snap.mtime {
                continue; // unchanged
            }

            // mtime changed — read + hash to confirm content change.
            let new_content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    log::debug!("file_tracker: could not read {}: {e}", path.display());
                    continue;
                }
            };

            let new_hash = hash_content(&new_content);
            if new_hash == snap.hash {
                // Content identical — no-op save. Update mtime only so
                // staleness doesn't trigger on mtime bumps.
                continue;
            }

            let old_content = snap.content.clone();

            changed.push(ChangedFile {
                path: path.clone(),
                old_content,
                new_content,
            });
        }

        changed
    }

    /// Update the snapshot for each changed file to reflect the current
    /// disk state.  Call this after [`check_modified`] for files whose
    /// changes you want to absorb.
    pub fn accept_changes(&mut self, paths: &[std::path::PathBuf]) {
        for path in paths {
            match snapshot(path) {
                Ok(new_snap) => {
                    log::info!(
                        "file_tracker: accept_change({}) mtime={:?}",
                        path.display(),
                        new_snap.mtime
                    );
                    self.files.insert(path.clone(), new_snap);
                }
                Err(e) => {
                    log::debug!(
                        "file_tracker: could not accept change for {}: {e}",
                        path.display()
                    );
                }
            }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn hash_content(content: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hasher.finalize().into()
}

fn snapshot(path: &Path) -> std::io::Result<FileSnapshot> {
    let content = std::fs::read_to_string(path)?;
    let meta = std::fs::metadata(path)?;
    let mtime = meta.modified()?;
    let hash = hash_content(&content);
    Ok(FileSnapshot {
        mtime,
        hash,
        content,
    })
}

/// Discover the git repository root via `git rev-parse --show-toplevel`
/// run from `dir`.  Returns `None` when git is not installed or `dir` is
/// not inside a git repository.
fn discover_git_root(dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8(output.stdout).ok()?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(PathBuf::from(trimmed))
}

/// Build the notification message text for a set of changed files.
///
/// For each file, if the unified diff is ≤ [`DIFF_INLINE_MAX_LINES`] lines,
/// the diff is inlined; otherwise a warn-only note is added.
pub fn build_notification(changes: &[ChangedFile]) -> String {
    let mut msg = String::from(
        "⚠️ The following files were modified externally since you last read or wrote them:\n",
    );

    for change in changes {
        let path_str = change.path.display();
        let diff = TextDiff::from_lines(&change.old_content, &change.new_content);

        let diff_lines: Vec<String> = diff
            .unified_diff()
            .context_radius(3)
            .header(&format!("a/{path_str}"), &format!("b/{path_str}"))
            .to_string()
            .lines()
            .map(|l| l.to_string())
            .collect();

        // Count only actual diff lines (exclude the --- / +++ header pair).
        let changed_line_count = diff
            .iter_all_changes()
            .filter(|c| c.tag() != ChangeTag::Equal)
            .count();

        msg.push('\n');
        if changed_line_count <= DIFF_INLINE_MAX_LINES {
            msg.push_str(&format!(
                "`{path_str}` was modified externally:\n```diff\n{}\n```\n",
                diff_lines.join("\n")
            ));
        } else {
            msg.push_str(&format!(
                "`{path_str}` was modified externally (diff too large to inline; {changed_line_count} lines changed). \
                 Re-read the file before making further edits.\n"
            ));
        }
    }

    msg
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    // ── exclusion tests ───────────────────────────────────────────────────────

    #[test]
    fn excluded_filename_is_not_tracked() {
        let f = write_temp("instructions\n");
        // Rename to AGENTS.md via a path that ends in that filename.
        let dir = tempfile::tempdir().unwrap();
        let agents_md = dir.path().join("AGENTS.md");
        std::fs::write(&agents_md, "instructions\n").unwrap();

        let mut tracker = FileTracker::with_exclusions(vec![], &["AGENTS.md"]);
        tracker.record(&agents_md);

        // Modify the file — if it were tracked this would be reported.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&agents_md, "changed\n").unwrap();

        let changed = tracker.check_modified();
        assert!(
            changed.is_empty(),
            "excluded filename should never be tracked"
        );
        drop(f);
    }

    #[test]
    fn excluded_prefix_is_not_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let session_file = sessions_dir.join("session-abc.jsonl");
        std::fs::write(&session_file, "line1\n").unwrap();

        let mut tracker = FileTracker::with_exclusions(vec![sessions_dir.clone()], &[]);
        tracker.record(&session_file);

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&session_file, "line2\n").unwrap();

        let changed = tracker.check_modified();
        assert!(
            changed.is_empty(),
            "file under excluded prefix should never be tracked"
        );
    }

    #[test]
    fn non_excluded_file_still_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let regular_file = dir.path().join("main.rs");
        std::fs::write(&regular_file, "fn main() {}\n").unwrap();

        let sessions_dir = dir.path().join("sessions");
        let mut tracker =
            FileTracker::with_exclusions(vec![sessions_dir], &["AGENTS.md", "SKILL.md"]);
        tracker.record(&regular_file);

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&regular_file, "fn main() { todo!() }\n").unwrap();

        let changed = tracker.check_modified();
        assert_eq!(
            changed.len(),
            1,
            "non-excluded file should still be tracked"
        );
    }

    #[test]
    fn skill_md_excluded_by_filename() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_file = skill_dir.join("SKILL.md");
        std::fs::write(&skill_file, "# skill\n").unwrap();

        let mut tracker = FileTracker::with_exclusions(vec![], &["SKILL.md"]);
        tracker.record(&skill_file);

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&skill_file, "# changed skill\n").unwrap();

        let changed = tracker.check_modified();
        assert!(changed.is_empty(), "SKILL.md should be excluded");
    }

    #[test]
    fn no_change_not_reported() {
        let f = write_temp("hello\n");
        let mut tracker = FileTracker::new();
        tracker.record(f.path());
        let changed = tracker.check_modified();
        assert!(changed.is_empty(), "expected no changes");
    }

    #[test]
    fn content_change_reported() {
        let f = write_temp("hello\n");
        let mut tracker = FileTracker::new();
        tracker.record(f.path());

        // Sleep >1ms to ensure mtime differs on most filesystems.
        // On Linux ext4 mtime resolution is 1ns, so this is fine.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(f.path(), "world\n").unwrap();

        let changed = tracker.check_modified();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].old_content, "hello\n");
        assert_eq!(changed[0].new_content, "world\n");
    }

    #[test]
    fn mtime_only_bump_not_reported() {
        let f = write_temp("hello\n");
        let mut tracker = FileTracker::new();
        tracker.record(f.path());

        // Write same content — mtime changes but hash stays the same.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(f.path(), "hello\n").unwrap();

        let changed = tracker.check_modified();
        assert!(changed.is_empty(), "same content should not be reported");
    }

    #[test]
    fn second_check_after_change_not_reported_again() {
        let f = write_temp("hello\n");
        let mut tracker = FileTracker::new();
        tracker.record(f.path());

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(f.path(), "world\n").unwrap();

        let first = tracker.check_modified();
        assert_eq!(first.len(), 1);

        // Accept the change so the snapshot is updated.
        let paths: Vec<_> = first.iter().map(|c| c.path.clone()).collect();
        tracker.accept_changes(&paths);

        // Second call — snapshot was updated, should not report again.
        let second = tracker.check_modified();
        assert!(second.is_empty(), "should not report the same change twice");
    }

    #[test]
    fn refresh_baselines_absorbs_agent_changes() {
        // Simulate: agent reads a file, modifies it, then refreshes baselines.
        // After refresh, check_modified() should NOT report the change.
        let f = write_temp("original\n");
        let mut tracker = FileTracker::new();
        tracker.record(f.path());

        // Agent modifies the file.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(f.path(), "agent-changed\n").unwrap();

        // Agent pauses and refreshes baselines (simulating end of agent run).
        tracker.refresh_baselines();

        // check_modified() should see no change since the baseline was reset.
        let changed = tracker.check_modified();
        assert!(
            changed.is_empty(),
            "agent-caused changes should be absorbed by refresh_baselines"
        );
    }

    #[test]
    fn refresh_baselines_then_user_edit_is_reported() {
        // Simulate: agent runs, refreshes baselines, then user edits a file.
        // check_modified() should report the user's change.
        let f = write_temp("original\n");
        let mut tracker = FileTracker::new();
        tracker.record(f.path());

        // Agent finishes, refreshes baselines.
        tracker.refresh_baselines();

        // User edits the file after the agent paused.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(f.path(), "user-changed\n").unwrap();

        let changed = tracker.check_modified();
        assert_eq!(
            changed.len(),
            1,
            "user edit after refresh should be reported"
        );
        assert_eq!(changed[0].new_content, "user-changed\n");
    }

    #[test]
    fn build_notification_inlines_small_diff() {
        let changes = vec![ChangedFile {
            path: PathBuf::from("foo.rs"),
            old_content: "fn main() {}\n".to_string(),
            new_content: "fn main() { println!(\"hi\"); }\n".to_string(),
        }];
        let msg = build_notification(&changes);
        assert!(msg.contains("```diff"), "expected inlined diff");
        assert!(msg.contains("foo.rs"));
    }

    #[test]
    fn build_notification_warn_only_for_large_diff() {
        // Generate a diff with more than DIFF_INLINE_MAX_LINES changed lines.
        let old: String = (0..100).map(|i| format!("old line {i}\n")).collect();
        let new: String = (0..100).map(|i| format!("new line {i}\n")).collect();
        let changes = vec![ChangedFile {
            path: PathBuf::from("big.rs"),
            old_content: old,
            new_content: new,
        }];
        let msg = build_notification(&changes);
        assert!(!msg.contains("```diff"), "should not inline large diff");
        assert!(msg.contains("too large to inline"));
    }

    // ── staleness tests ──────────────────────────────────────────────────────

    #[test]
    fn staleness_never_read_for_untracked_file() {
        let f = write_temp("hello\n");
        let tracker = FileTracker::new();
        assert_eq!(tracker.staleness(f.path()), Staleness::NeverRead,);
    }

    #[test]
    fn staleness_current_when_not_modified() {
        let f = write_temp("hello\n");
        let mut tracker = FileTracker::new();
        tracker.record(f.path());
        assert_eq!(tracker.staleness(f.path()), Staleness::Current,);
    }

    #[test]
    fn staleness_stale_when_modified_after_record() {
        let f = write_temp("hello\n");
        let mut tracker = FileTracker::new();
        tracker.record(f.path());

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(f.path(), "world\n").unwrap();

        match tracker.staleness(f.path()) {
            Staleness::Stale { .. } => {}
            other => panic!("expected Staleness::Stale, got {other:?}"),
        }
    }
}
