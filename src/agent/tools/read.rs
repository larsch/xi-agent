use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agent::file_tracker::FileTracker;
use crate::agent::tools::truncate::{TruncationResult, truncate_head_with_limits};
use crate::agent::types::{Tool, ToolResult};

pub struct ReadFileTool {
    tracker: Arc<Mutex<FileTracker>>,
}

impl ReadFileTool {
    pub fn new(tracker: Arc<Mutex<FileTracker>>) -> Self {
        Self { tracker }
    }
}

fn split_lines_preserving_endings(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }

    let bytes = content.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&content[start..i + 1]);
                i += 1;
                start = i;
            }
            b'\r' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    lines.push(&content[start..i + 2]);
                    i += 2;
                } else {
                    lines.push(&content[start..i + 1]);
                    i += 1;
                }
                start = i;
            }
            _ => i += 1,
        }
    }

    if start < content.len() {
        lines.push(&content[start..]);
    }

    lines
}

#[derive(serde::Deserialize)]
struct ReadFileArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file at the given path. \
         Optionally specify `offset` (1-indexed line number to start from) \
         and `limit` (maximum number of lines to return). \
         When the output is truncated a header `[lines X-Y of Z]` is prepended."
    }
    // NOTE: the description above is intentionally kept identical to the
    // original wording so that existing sessions that reference it remain
    // coherent.  The actual `[lines …]` header is no longer embedded in
    // content; range information is now carried in ToolResult::truncation.

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "1-indexed line number to start reading from (optional)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (optional)"
                }
            },
            "required": ["path"]
        })
    }

    fn execute(
        &self,
        args: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let ReadFileArgs {
                path,
                offset,
                limit,
            } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(e) => return ToolResult::err(format!("Failed to read {path}: {e}")),
            };

            let all_lines = split_lines_preserving_endings(&content);
            let total = all_lines.len();

            // offset is 1-indexed; default to line 1
            let start = offset.unwrap_or(1).saturating_sub(1);
            let start = start.min(total);

            let end = match limit {
                Some(l) => (start + l).min(total),
                None => total,
            };

            let selected: Vec<&str> = all_lines[start..end].to_vec();
            let window_content = selected.concat();
            let is_windowed = start > 0 || end < total;

            // Empty window (offset beyond EOF) — return early with no metadata.
            if start == end && is_windowed {
                self.tracker
                    .lock()
                    .unwrap()
                    .record(std::path::Path::new(&path));
                return ToolResult::ok_str("");
            }

            // Apply a size cap on the window content using truncate_head so
            // very large files don't produce unbounded tool results.
            let tr = truncate_head_with_limits(
                &window_content,
                crate::agent::tools::truncate::DEFAULT_MAX_LINES,
                crate::agent::tools::truncate::DEFAULT_MAX_BYTES,
            );

            let truncated = is_windowed || tr.truncated;

            // Compute the effective displayed line range (1-indexed, inclusive).
            let first_line = start + 1; // start is 0-indexed
            let last_line = if tr.truncated {
                start + tr.output_lines
            } else {
                end
            };

            // Record the full file snapshot (always the whole file, regardless
            // of offset/limit, so we can diff correctly later).
            self.tracker
                .lock()
                .unwrap()
                .record(std::path::Path::new(&path));

            let mut result = ToolResult::ok_str(tr.content);
            if truncated {
                let output_lines = if last_line >= first_line {
                    last_line - first_line + 1
                } else {
                    0
                };
                result.truncation = Some(TruncationResult {
                    content: result.content.clone(),
                    truncated: true,
                    total_lines: total,

                    output_lines,
                    first_kept_line: first_line,
                });
                result.is_truncated = true;
            }
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::Tool;
    use std::sync::{Arc, Mutex};

    fn make_tool() -> ReadFileTool {
        ReadFileTool::new(Arc::new(Mutex::new(
            crate::agent::file_tracker::FileTracker::new(),
        )))
    }
    use std::io::Write;

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[tokio::test]
    async fn read_full_file() {
        let f = write_temp("line1\nline2\nline3\n");
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.contains("line1"));
        assert!(result.content.contains("line3"));
    }

    #[tokio::test]
    async fn read_with_offset_and_limit() {
        let f = write_temp("a\nb\nc\nd\ne\n");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "offset": 2,
            "limit": 2
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        // Should contain lines 2-3 (b, c) but not a or e
        assert!(
            result.content.contains('b'),
            "missing b: {}",
            result.content
        );
        assert!(
            result.content.contains('c'),
            "missing c: {}",
            result.content
        );
        assert!(
            !result.content.contains("\na\n") && !result.content.starts_with("a"),
            "should not contain line a: {}",
            result.content
        );
        assert!(
            !result.content.contains("\ne\n") && !result.content.ends_with("\ne"),
            "should not contain line e: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn read_preserves_crlf_line_endings() {
        let f = write_temp("a\r\nb\r\n");
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "a\r\nb\r\n");
    }

    #[tokio::test]
    async fn read_preserves_cr_only_line_endings() {
        let f = write_temp("a\rb\r");
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "a\rb\r");
    }

    #[tokio::test]
    async fn read_offset_with_crlf_keeps_original_endings() {
        let f = write_temp("a\r\nb\r\nc\r\n");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "offset": 2,
            "limit": 1
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        // Content must be the raw body — no in-band header anymore.
        assert_eq!(result.content, "b\r\n");
        // Range information must be carried in the truncation field.
        let tr = result.truncation.expect("truncation metadata expected");
        assert_eq!(tr.first_kept_line, 2);
        assert_eq!(tr.total_lines, 3);
    }

    #[tokio::test]
    async fn read_offset_beyond_eof_returns_empty() {
        let f = write_temp("only one line\n");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "offset": 100
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        // No lines selected; content should be empty (no in-band header anymore).
        assert!(
            result.content.trim().is_empty(),
            "unexpected content: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn read_long_single_line_does_not_underflow_truncation_metadata() {
        let long_line = "x".repeat(crate::agent::tools::truncate::DEFAULT_MAX_BYTES + 1);
        let f = write_temp(&long_line);
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;

        assert!(!result.is_error);
        assert!(
            result.is_truncated,
            "expected truncation for long single line"
        );

        let tr = result.truncation.expect("missing truncation metadata");
        assert_eq!(tr.first_kept_line, 1);
        assert_eq!(tr.output_lines, 0);
        assert_eq!(tr.total_lines, 1);
    }

    #[tokio::test]
    async fn read_missing_file_is_error() {
        let tool = make_tool();
        let args = serde_json::json!({"path": "/nonexistent/path/to/file.txt"});
        let result = tool.execute(args).await;
        assert!(result.is_error, "expected error for missing file");
    }

    #[tokio::test]
    async fn read_wrong_type_for_path_is_error() {
        let tool = make_tool();
        let args = serde_json::json!({"path": 42});
        let result = tool.execute(args).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("Invalid arguments"),
            "expected 'Invalid arguments' in error: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn read_extra_fields_are_ignored() {
        let f = write_temp("hello\n");
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap(), "unknown": true});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }
}
