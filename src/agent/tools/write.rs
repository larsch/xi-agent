use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agent::file_tracker::{FileTracker, Staleness};
use crate::agent::tools::utf8::write_payload_file;
use crate::agent::types::{Tool, ToolResult};

pub struct WriteTool {
    tracker: Arc<Mutex<FileTracker>>,
}

impl WriteTool {
    pub fn new(tracker: Arc<Mutex<FileTracker>>) -> Self {
        Self { tracker }
    }
}

fn count_lines_any_ending(content: &str) -> usize {
    if content.is_empty() {
        return 0;
    }

    let bytes = content.as_bytes();
    let mut lines = 0;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines += 1;
                i += 1;
            }
            b'\r' => {
                lines += 1;
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    if !matches!(bytes.last(), Some(b'\n' | b'\r')) {
        lines += 1;
    }

    lines
}

#[derive(serde::Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file at the given path, creating parent directories \
         as needed. Overwrites the file if it already exists."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Relative paths are resolved from the current working directory."
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn streaming_field(&self) -> Option<&'static str> {
        Some("path")
    }

    fn run(
        &self,
        args: Value,
        _ctx: crate::agent::types::ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let WriteArgs { path, content } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            // Guard against overwriting a file that was modified externally
            // since it was last read. Skip for new files.
            let file_path = std::path::Path::new(&path);
            if file_path.exists() {
                let tracker = self.tracker.lock().unwrap();
                match tracker.staleness(file_path) {
                    Staleness::NeverRead => {
                        log::info!("write_file: rejecting {path} — never read");
                        return ToolResult::err(format!(
                            "You must read {path} before overwriting it. Use read_file first."
                        ));
                    }
                    Staleness::Stale {
                        mod_time,
                        read_time,
                    } => {
                        log::info!(
                            "write_file: rejecting {path} — stale (mod={mod_time:?}, read={read_time:?})"
                        );
                        return ToolResult::err(format!(
                            "{path} was modified since last read. \
                             Re-read with read_file before overwriting.\n\
                             Modified: {mod_time:?}\n\
                             Last read: {read_time:?}"
                        ));
                    }
                    Staleness::Current => {
                        log::info!("write_file: {path} is current");
                    }
                }
                // guard dropped here — before any await
            }

            // Create parent directories if needed.
            if let Some(parent) = file_path.parent()
                && !parent.as_os_str().is_empty()
                && let Err(e) = tokio::fs::create_dir_all(parent).await
            {
                return ToolResult::err(format!("Failed to create directories for {path}: {e}"));
            }

            if let Err(e) = write_payload_file(std::path::Path::new(&path), &content) {
                return ToolResult::err(format!("Failed to write UTF-8 file {path}: {e}"));
            }

            self.tracker
                .lock()
                .unwrap()
                .record(std::path::Path::new(&path));

            let line_count = count_lines_any_ending(&content);
            ToolResult::ok_str(format!("Written {line_count} lines to {path}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::Tool;
    use std::sync::{Arc, Mutex};

    fn make_tool() -> WriteTool {
        WriteTool::new(Arc::new(Mutex::new(
            crate::agent::file_tracker::FileTracker::new(),
        )))
    }

    /// Create a tool and pre-record the given path as "read" so the
    /// staleness guard passes.
    fn make_tool_with_read(path: &std::path::Path) -> WriteTool {
        let tracker = Arc::new(Mutex::new(crate::agent::file_tracker::FileTracker::new()));
        tracker.lock().unwrap().record(path);
        WriteTool::new(tracker)
    }

    #[tokio::test]
    async fn write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new_file.txt");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "hello\n"
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        assert!(path.exists(), "file was not created");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    }

    #[tokio::test]
    async fn write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "old content\n").unwrap();
        let tool = make_tool_with_read(&path);
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "new content\n"
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content\n");
    }

    #[tokio::test]
    async fn write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c").join("file.txt");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "deep\n"
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        assert!(path.exists(), "file not created in nested dirs");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "deep\n");
    }

    #[tokio::test]
    async fn write_preserves_crlf_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("windows.txt");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "a\r\nb\r\n"
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\r\nb\r\n");
    }

    #[tokio::test]
    async fn write_reports_line_count_for_cr_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("classic-mac.txt");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "a\rb\r"
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        assert_eq!(
            result.content.as_text(),
            format!("Written 2 lines to {}", path.to_str().unwrap())
        );
    }

    #[tokio::test]
    async fn write_wrong_type_for_content_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let tool = make_tool();
        let args = serde_json::json!({"path": path.to_str().unwrap(), "content": 99});
        let result = tool.execute(args).await;
        assert!(result.is_error);
        assert!(
            result.content.as_text().contains("Invalid arguments"),
            "expected 'Invalid arguments' in error: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn write_extra_fields_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let tool = make_tool();
        let args =
            serde_json::json!({"path": path.to_str().unwrap(), "content": "hi\n", "mode": "644"});
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi\n");
    }

    // ── staleness guard tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn write_rejects_never_read_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "old content\n").unwrap();
        let tool = make_tool(); // no record
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "new content\n"
        });
        let result = tool.execute(args).await;
        assert!(result.is_error, "expected error for never-read file");
        assert!(
            result.content.as_text().contains("must read"),
            "expected 'must read' in error: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn write_rejects_stale_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "old content\n").unwrap();
        let tool = make_tool_with_read(&path);

        // Modify the file after recording it.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, "modified externally\n").unwrap();

        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "new content\n"
        });
        let result = tool.execute(args).await;
        assert!(result.is_error, "expected error for stale file");
        assert!(
            result
                .content
                .as_text()
                .contains("modified since last read"),
            "expected 'modified since last read' in error: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn write_allows_new_file_without_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new_file.txt");
        let tool = make_tool(); // no record — new file is fine
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "hello\n"
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error for new file: {}",
            result.content.as_text()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    }

    #[tokio::test]
    async fn write_twice_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let tool = make_tool();

        // First write: new file.
        let args1 = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "first\n"
        });
        let result1 = tool.execute(args1).await;
        assert!(
            !result1.is_error,
            "unexpected error on first write: {}",
            result1.content.as_text()
        );

        // Second write: overwrite the file just written.
        let args2 = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "second\n"
        });
        let result2 = tool.execute(args2).await;
        assert!(
            !result2.is_error,
            "unexpected error on second write: {}",
            result2.content.as_text()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
    }

    #[tokio::test]
    async fn write_then_edit_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let tool = make_tool();

        // Write a new file.
        let write_args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "hello world\n"
        });
        let write_result = tool.execute(write_args).await;
        assert!(
            !write_result.is_error,
            "unexpected error on write: {}",
            write_result.content.as_text()
        );

        // Edit the file just written — must pass staleness guard.
        let edit_tool = crate::agent::tools::edit::EditTool::new(tool.tracker.clone());
        let edit_args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_text": "hello",
            "new_text": "goodbye"
        });
        let edit_result = edit_tool.execute(edit_args).await;
        assert!(
            !edit_result.is_error,
            "unexpected error on edit after write: {}",
            edit_result.content.as_text()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "goodbye world\n");
    }
}
