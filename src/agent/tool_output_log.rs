use std::{
    fs,
    path::PathBuf,
};

/// Per-session store for full tool output files.
///
/// Each tool call that opts in gets its streams written to
/// `{base_dir}/{tool_call_id}.stdout` and/or `{base_dir}/{tool_call_id}.stderr`.
/// The directory is removed on [`Drop`].
///
/// # Directory selection
/// 1. `$XDG_RUNTIME_DIR/tau/tool-output/{session_id}/`  (Linux, when set)
/// 2. `{temp_dir}/tau-tool-output-{session_id}/`         (macOS, Windows, fallback)
pub struct ToolOutputLog {
    dir: PathBuf,
}

impl ToolOutputLog {
    /// Create a new log for `session_id`, placing files in the best available
    /// runtime or temp directory.  Creates the directory immediately.
    pub fn new(session_id: &str) -> Self {
        let dir = resolve_dir(session_id);
        if let Err(e) = fs::create_dir_all(&dir) {
            log::debug!(
                "tool_output_log: failed to create dir {}: {e}",
                dir.display()
            );
        }
        Self { dir }
    }

    /// Write non-empty streams to `{id}.stdout` and/or `{id}.stderr`.
    ///
    /// Returns `(stdout_path, stderr_path)` — `None` for each stream that was
    /// empty or failed to write.
    pub fn record_streams(
        &self,
        tool_call_id: &str,
        stdout: &str,
        stderr: &str,
    ) -> (Option<PathBuf>, Option<PathBuf>) {
        let id = sanitise_id(tool_call_id);
        let stdout_path = if !stdout.is_empty() {
            self.write_file(&format!("{id}.stdout"), stdout)
        } else {
            None
        };
        let stderr_path = if !stderr.is_empty() {
            self.write_file(&format!("{id}.stderr"), stderr)
        } else {
            None
        };
        (stdout_path, stderr_path)
    }

    fn write_file(&self, filename: &str, content: &str) -> Option<PathBuf> {
        let path = self.dir.join(filename);
        if let Err(e) = fs::write(&path, content) {
            log::debug!(
                "tool_output_log: failed to write {}: {e}",
                path.display()
            );
            return None;
        }
        Some(path)
    }

    /// Return the directory used by this log.
    #[cfg(test)]
    pub fn base_dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Return the directory used by this log (primarily for tests).
    #[cfg(test)]
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

impl Drop for ToolOutputLog {
    fn drop(&mut self) {
        if self.dir.exists()
            && let Err(e) = fs::remove_dir_all(&self.dir)
        {
            log::debug!(
                "tool_output_log: failed to remove dir {}: {e}",
                self.dir.display()
            );
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn resolve_dir(session_id: &str) -> PathBuf {
    // Prefer XDG_RUNTIME_DIR on Linux when it is set to an absolute path.
    #[cfg(target_os = "linux")]
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let base = PathBuf::from(runtime);
        if base.is_absolute() {
            return base.join("tau").join("tool-output").join(session_id);
        }
    }

    // Fallback: system temp dir.
    std::env::temp_dir().join(format!("tau-tool-output-{session_id}"))
}

/// Replace characters that are unsafe in filenames with `_`.
pub(crate) fn sanitise_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_streams_writes_both_files_when_non_empty() {
        let log = ToolOutputLog::new("test-session-001");
        let (out, err) = log.record_streams("call_abc123", "hello", "oops");
        let out = out.unwrap();
        let err = err.unwrap();
        assert!(out.exists());
        assert!(err.exists());
        assert_eq!(fs::read_to_string(&out).unwrap(), "hello");
        assert_eq!(fs::read_to_string(&err).unwrap(), "oops");
        assert!(out.to_str().unwrap().ends_with(".stdout"));
        assert!(err.to_str().unwrap().ends_with(".stderr"));
    }

    #[test]
    fn record_streams_skips_empty_streams() {
        let log = ToolOutputLog::new("test-session-002");
        let (out, err) = log.record_streams("call_xyz", "some output", "");
        assert!(out.is_some());
        assert!(err.is_none());
    }

    #[test]
    fn record_streams_filename_uses_sanitised_id() {
        let log = ToolOutputLog::new("test-session-003");
        let (out, _) = log.record_streams("call/bad\\id", "data", "");
        let filename = out.unwrap();
        let name = filename.file_name().unwrap().to_str().unwrap();
        assert!(!name.contains('/'));
        assert!(!name.contains('\\'));
    }

    #[test]
    fn drop_removes_directory() {
        let dir = {
            let log = ToolOutputLog::new("test-session-004");
            let d = log.dir().to_path_buf();
            assert!(d.exists());
            log.record_streams("call_x", "data", "");
            d
        };
        assert!(!dir.exists(), "directory should be removed on drop");
    }

    #[test]
    fn sanitise_id_preserves_alphanumeric_dash_underscore() {
        assert_eq!(sanitise_id("call_abc-123"), "call_abc-123");
    }

    #[test]
    fn sanitise_id_replaces_unsafe_chars() {
        assert_eq!(sanitise_id("a/b\\c.d"), "a_b_c_d");
    }
}
