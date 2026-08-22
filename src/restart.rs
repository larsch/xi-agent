//! Support for the `restart_host` tool (compiled only under the `restart` feature).
//!
//! The tool replaces the running process via `exec()`, so its result message is
//! produced by the *resumed* process, which reports the on-disk binary it was
//! started from. This module holds the two helpers for that flow:
//!
//! - [`restart_message`] — the result string shown to the model after resume.
//! - [`exec_self`] — re-exec the current binary with `--resume-session <id>`.

/// The name of the restart tool.  Used both by the tool implementation and by
/// the resume-time detection that synthesizes its result.
pub(crate) const RESTART_TOOL_NAME: &str = "restart_host";

/// Resolve the on-disk binary path.
///
/// On Linux, after the running binary has been replaced on disk by a rebuild,
/// `/proc/self/exe` (which `current_exe` reads) resolves to `<path> (deleted)`
/// because the original inode was unlinked.  Strip that suffix so the resolved
/// path targets the freshly built binary at the same path.
fn current_exe_path() -> std::io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;

    #[cfg(target_os = "linux")]
    {
        let s = exe.to_string_lossy();
        if let Some(stripped) = s.strip_suffix(" (deleted)") {
            return Ok(std::path::PathBuf::from(stripped));
        }
    }

    Ok(exe)
}

/// The on-disk binary path as a display string, for embedding in the tool
/// description and the post-restart result message.
pub(crate) fn current_exe_display() -> String {
    current_exe_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string())
}

/// Build the auto-generated `restart_host` result: the on-disk binary path and its
/// last-modified timestamp, formatted as ISO 8601 (RFC 3339).
///
/// Falls back to `<unknown>` placeholders if the executable path or metadata
/// cannot be resolved (which should not happen in practice after a successful
/// `exec`).
pub(crate) fn restart_message() -> String {
    let exe = current_exe_path().ok();
    let mtime = exe
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok());

    let (path, timestamp) = match (exe.as_deref(), mtime) {
        (Some(p), Some(t)) => {
            let dt = chrono::DateTime::<chrono::Utc>::from(t);
            (p.display().to_string(), dt.to_rfc3339())
        }
        _ => ("<unknown>".to_string(), "<unknown>".to_string()),
    };

    format!("Restarted from binary {path} last modified at {timestamp}")
}

/// Replace the current process image with a fresh invocation of the on-disk
/// binary that resumes `session_id`.
///
/// Uses `exec(3)` so the new binary (which may have been rebuilt since launch)
/// replaces this process in place.  On success this function never returns; on
/// failure it returns the underlying [`std::io::Error`].
#[cfg(unix)]
pub(crate) fn exec_self(session_id: &str) -> std::io::Error {
    use std::os::unix::process::CommandExt;

    let exe = match current_exe_path() {
        Ok(p) => p,
        Err(e) => return e,
    };

    std::process::Command::new(&exe)
        .arg("--resume-session")
        .arg(session_id)
        .exec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_message_reports_binary_path_and_iso8601_timestamp() {
        let msg = restart_message();
        assert!(
            msg.starts_with("Restarted from binary "),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains(" last modified at "),
            "unexpected message: {msg}"
        );

        // The path and timestamp are real (not the <unknown> fallback) on a
        // normal process, and the timestamp is RFC 3339.
        let rest = msg.strip_prefix("Restarted from binary ").unwrap();
        let (path, ts) = rest.split_once(" last modified at ").unwrap();
        assert!(!path.is_empty());
        assert_ne!(path, "<unknown>");
        assert_ne!(ts, "<unknown>");
        // RFC 3339 timestamps contain a 'T' separating date and time.
        assert!(ts.contains('T'), "timestamp should be RFC 3339: {ts}");
    }
}
