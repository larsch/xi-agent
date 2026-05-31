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

/// Detect the MIME type of a file from its magic bytes.
///
/// Reads up to 16 bytes from the start of `data` and returns the MIME type
/// string for JPEG, PNG, GIF, and WebP images, or `None` for everything else.
fn detect_image_mime_type(data: &[u8]) -> Option<&'static str> {
    // JPEG: FF D8 FF
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if data.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    // GIF: 47 49 46 38 ("GIF8")
    if data.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    // WebP: 52 49 46 46 ?? ?? ?? ?? 57 45 42 50 ("RIFF....WEBP")
    if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Split `content` into lines, preserving each line's original terminator.
///
/// Handles `\n`, `\r\n`, and bare `\r` as line endings. The standard library's
/// `str::split_inclusive('\n')` handles `\n` and `\r\n` correctly (the `\r`
/// stays attached to the preceding line) but does not treat bare `\r` as a
/// line ending. This function is used instead so that files with old Mac-style
/// `\r`-only line endings are split correctly.
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
         Supports text files and images (JPEG, PNG, GIF, WebP). \
         For image files the raw image is returned for the model to inspect. \
         Optionally specify `offset` (1-indexed line number to start from) \
         and `limit` (maximum number of lines to return). \
         When the output is truncated a notice `[lines X-Y of Z. Use offset/limit parameters to read more.]` is appended."
    }

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

    fn streaming_field(&self) -> Option<&'static str> {
        Some("path")
    }

    fn run(
        &self,
        args: Value,
        _ctx: crate::agent::types::ToolCallContext,
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

            // Read the raw bytes first so we can sniff the magic header.
            let raw_bytes = match tokio::fs::read(&path).await {
                Ok(b) => b,
                Err(e) => return ToolResult::err(format!("Failed to read {path}: {e}")),
            };

            // Record the file in the tracker regardless of type.
            self.tracker
                .lock()
                .unwrap()
                .record(std::path::Path::new(&path));

            // Check for an image by magic bytes.
            if let Some(mime_type) = detect_image_mime_type(&raw_bytes) {
                return ToolResult::ok_image(raw_bytes, mime_type);
            }

            // Fall through to text handling.
            let content = match String::from_utf8(raw_bytes) {
                Ok(s) => s,
                Err(_) => {
                    return ToolResult::err(format!(
                        "Failed to read {path}: file is not valid UTF-8 and is not a recognised image format"
                    ));
                }
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

            // (File already recorded above before image detection.)

            let mut result_content = tr.content;
            if truncated {
                let output_lines = if last_line >= first_line {
                    last_line - first_line + 1
                } else {
                    0
                };
                // Append an in-band notice so the model always sees the range
                // and total, even when it cannot inspect ToolResult metadata.
                let notice = format!(
                    "\n[lines {first_line}-{last_line} of {total}. Use offset/limit parameters to read more.]"
                );
                result_content.push_str(&notice);
                let mut result = ToolResult::ok_str(result_content.clone());
                result.truncation = Some(TruncationResult {
                    content: result_content,
                    truncated: true,
                    total_lines: total,
                    output_lines,
                    first_kept_line: first_line,
                });
                result.is_truncated = true;
                result
            } else {
                ToolResult::ok_str(result_content)
            }
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
        assert!(result.content.as_text().contains("line1"));
        assert!(result.content.as_text().contains("line3"));
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
            result.content.as_text().contains('b'),
            "missing b: {}",
            result.content.as_text()
        );
        assert!(
            result.content.as_text().contains('c'),
            "missing c: {}",
            result.content.as_text()
        );
        assert!(
            !result.content.as_text().contains("\na\n")
                && !result.content.as_text().starts_with("a"),
            "should not contain line a: {}",
            result.content.as_text()
        );
        assert!(
            !result.content.as_text().contains("\ne\n")
                && !result.content.as_text().ends_with("\ne"),
            "should not contain line e: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn read_preserves_crlf_line_endings() {
        let f = write_temp("a\r\nb\r\n");
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert_eq!(result.content.as_text(), "a\r\nb\r\n");
    }

    #[tokio::test]
    async fn read_preserves_cr_only_line_endings() {
        let f = write_temp("a\rb\r");
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert_eq!(result.content.as_text(), "a\rb\r");
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
        // Content must start with the raw line body followed by the truncation notice.
        assert!(
            result.content.as_text().starts_with("b\r\n"),
            "unexpected content: {}",
            result.content.as_text()
        );
        assert!(
            result.content.as_text().contains("[lines 2-2 of 3"),
            "missing truncation notice: {}",
            result.content.as_text()
        );
        // Range information must also be carried in the truncation field.
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
            result.content.as_text().trim().is_empty(),
            "unexpected content: {}",
            result.content.as_text()
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
            result.content.as_text().contains("Invalid arguments"),
            "expected 'Invalid arguments' in error: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn read_extra_fields_are_ignored() {
        let f = write_temp("hello\n");
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap(), "unknown": true});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.as_text().contains("hello"));
    }

    #[tokio::test]
    async fn read_png_returns_image_content() {
        // Minimal 1×1 PNG (smallest valid PNG)
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        ];
        let mut f = tempfile::NamedTempFile::new().unwrap();
        use std::io::Write;
        f.write_all(png_bytes).unwrap();
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            matches!(result.content, crate::agent::types::ToolContent::Image { ref mime_type, .. } if mime_type == "image/png"),
            "expected Image(image/png) content, got: {:?}",
            result.content
        );
    }

    #[tokio::test]
    async fn read_jpeg_returns_image_content() {
        // JPEG magic bytes
        let jpeg_bytes: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        let mut f = tempfile::NamedTempFile::new().unwrap();
        use std::io::Write;
        f.write_all(jpeg_bytes).unwrap();
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            matches!(result.content, crate::agent::types::ToolContent::Image { ref mime_type, .. } if mime_type == "image/jpeg"),
            "expected Image(image/jpeg) content"
        );
    }

    #[tokio::test]
    async fn read_text_file_still_works_after_image_detection() {
        let f = write_temp("hello world\n");
        let tool = make_tool();
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.as_text().contains("hello world"));
    }
}
