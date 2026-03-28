use std::{
    collections::HashMap,
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

/// Tracks files touched by the agent's file tools and detects external
/// modifications using a two-stage check: mtime first, then SHA-256.
#[derive(Default)]
pub struct FileTracker {
    files: HashMap<PathBuf, FileSnapshot>,
}

impl FileTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the current state of `path`. Called after a successful
    /// `read_file`, `write_file`, or `edit_file` tool execution.
    ///
    /// Non-UTF-8 files are silently skipped (binary files are not tracked).
    pub fn record(&mut self, path: &Path) {
        match snapshot(path) {
            Ok(snap) => {
                self.files.insert(path.to_path_buf(), snap);
            }
            Err(e) => {
                log::debug!("file_tracker: could not record {}: {e}", path.display());
            }
        }
    }

    /// Check all tracked paths for external modifications.
    ///
    /// A file is considered externally modified when its mtime has changed
    /// **and** its content hash has changed (mtime-only bumps are ignored).
    ///
    /// Returns one [`ChangedFile`] per modified path and updates each snapshot
    /// so subsequent calls don't re-report the same change.
    pub fn check_modified(&mut self) -> Vec<ChangedFile> {
        let mut changed = Vec::new();

        for (path, snap) in &mut self.files {
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
                // Content identical — no-op save, just update mtime.
                snap.mtime = new_mtime;
                continue;
            }

            let old_content = snap.content.clone();

            // Update snapshot to the new state.
            snap.mtime = new_mtime;
            snap.hash = new_hash;
            snap.content = new_content.clone();

            changed.push(ChangedFile {
                path: path.clone(),
                old_content,
                new_content,
            });
        }

        changed
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

        // Second call — snapshot was updated, should not report again.
        let second = tracker.check_modified();
        assert!(second.is_empty(), "should not report the same change twice");
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
}
