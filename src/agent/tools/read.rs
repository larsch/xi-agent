use std::pin::Pin;

use serde_json::Value;

use crate::agent::types::{Tool, ToolResult};

pub struct ReadFileTool;

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
                Err(e) => return e,
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
            let truncated = start > 0 || end < total;

            let mut result = String::new();
            if truncated {
                result.push_str(&format!("[lines {}-{} of {}]\n", start + 1, end, total));
            }
            result.push_str(&selected.concat());

            ToolResult::ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::Tool;
    use std::io::Write;

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[tokio::test]
    async fn read_full_file() {
        let f = write_temp("line1\nline2\nline3\n");
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.contains("line1"));
        assert!(result.content.contains("line3"));
    }

    #[tokio::test]
    async fn read_with_offset_and_limit() {
        let f = write_temp("a\nb\nc\nd\ne\n");
        let tool = ReadFileTool;
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
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "a\r\nb\r\n");
    }

    #[tokio::test]
    async fn read_preserves_cr_only_line_endings() {
        let f = write_temp("a\rb\r");
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": f.path().to_str().unwrap()});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "a\rb\r");
    }

    #[tokio::test]
    async fn read_offset_with_crlf_keeps_original_endings() {
        let f = write_temp("a\r\nb\r\nc\r\n");
        let tool = ReadFileTool;
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "offset": 2,
            "limit": 1
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "[lines 2-2 of 3]\nb\r\n");
    }

    #[tokio::test]
    async fn read_offset_beyond_eof_returns_empty() {
        let f = write_temp("only one line\n");
        let tool = ReadFileTool;
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "offset": 100
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        // No lines selected; content should be empty or just the header
        assert!(
            result.content.trim().is_empty() || result.content.contains("lines"),
            "unexpected content: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn read_missing_file_is_error() {
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": "/nonexistent/path/to/file.txt"});
        let result = tool.execute(args).await;
        assert!(result.is_error, "expected error for missing file");
    }

    #[tokio::test]
    async fn read_wrong_type_for_path_is_error() {
        let tool = ReadFileTool;
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
        let tool = ReadFileTool;
        let args = serde_json::json!({"path": f.path().to_str().unwrap(), "unknown": true});
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }
}
