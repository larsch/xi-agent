use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agent::file_tracker::FileTracker;
use crate::agent::types::{Tool, ToolResult};

pub struct EditTool {
    tracker: Arc<Mutex<FileTracker>>,
}

impl EditTool {
    pub fn new(tracker: Arc<Mutex<FileTracker>>) -> Self {
        Self { tracker }
    }
}

fn strip_utf8_bom(s: &str) -> (&str, &str) {
    if let Some(stripped) = s.strip_prefix('\u{FEFF}') {
        ("\u{FEFF}", stripped)
    } else {
        ("", s)
    }
}

fn detect_line_ending(s: &str) -> &'static str {
    if s.contains("\r\n") {
        "\r\n"
    } else if s.contains('\r') {
        "\r"
    } else {
        "\n"
    }
}

fn normalize_to_lf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(s: &str, line_ending: &str) -> String {
    match line_ending {
        "\r\n" => s.replace('\n', "\r\n"),
        "\r" => s.replace('\n', "\r"),
        _ => s.to_string(),
    }
}

#[derive(serde::Deserialize)]
struct EditArgs {
    path: String,
    old_text: String,
    new_text: String,
}

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact text occurrence with new text. \
         `old_text` must match exactly (including whitespace and newlines) and \
         must appear exactly once in the file — the call fails if zero or \
         multiple matches are found."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_text": {
                    "type": "string",
                    "description": "Exact text to find (must appear exactly once)"
                },
                "new_text": {
                    "type": "string",
                    "description": "Text to replace old_text with"
                }
            },
            "required": ["path", "old_text", "new_text"]
        })
    }

    fn streaming_field(&self) -> Option<String> {
        Some("new_text".to_string())
    }

    fn execute(
        &self,
        args: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let EditArgs {
                path,
                old_text,
                new_text,
            } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            let raw_content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(e) => return ToolResult::err(format!("Failed to read {path}: {e}")),
            };

            let (bom, content_without_bom) = strip_utf8_bom(&raw_content);
            let original_line_ending = detect_line_ending(content_without_bom);

            let normalized_content = normalize_to_lf(content_without_bom);
            let normalized_old_text = normalize_to_lf(&old_text);
            let normalized_new_text = normalize_to_lf(&new_text);

            let count = normalized_content
                .matches(normalized_old_text.as_str())
                .count();
            if count == 0 {
                return ToolResult::err(format!(
                    "old_text not found in {path}. \
                     Verify the text matches exactly, including whitespace."
                ));
            }
            if count > 1 {
                return ToolResult::err(format!(
                    "old_text found {count} times in {path}. \
                     old_text must be unique to avoid ambiguous edits."
                ));
            }

            let normalized_updated = normalized_content.replacen(
                normalized_old_text.as_str(),
                normalized_new_text.as_str(),
                1,
            );
            let updated = format!(
                "{bom}{}",
                restore_line_endings(&normalized_updated, original_line_ending)
            );

            if let Err(e) = tokio::fs::write(&path, updated.as_bytes()).await {
                return ToolResult::err(format!("Failed to write {path}: {e}"));
            }

            self.tracker
                .lock()
                .unwrap()
                .record(std::path::Path::new(&path));

            ToolResult::ok_str(format!("Successfully edited {path}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::Tool;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    fn make_tool() -> EditTool {
        EditTool::new(Arc::new(Mutex::new(
            crate::agent::file_tracker::FileTracker::new(),
        )))
    }

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[tokio::test]
    async fn edit_replaces_exact_match() {
        let f = write_temp("hello world\n");
        let path = f.path().to_str().unwrap().to_string();
        let tool = make_tool();
        let args = serde_json::json!({
            "path": path,
            "old_text": "hello",
            "new_text": "goodbye"
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        let updated = std::fs::read_to_string(&path).unwrap();
        assert_eq!(updated, "goodbye world\n");
    }

    #[tokio::test]
    async fn edit_matches_lf_against_crlf_file_and_preserves_crlf() {
        let f = write_temp("a\r\nb\r\nc\r\n");
        let path = f.path().to_str().unwrap().to_string();
        let tool = make_tool();
        let args = serde_json::json!({
            "path": path,
            "old_text": "b\n",
            "new_text": "x\n"
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        let updated = std::fs::read_to_string(&path).unwrap();
        assert_eq!(updated, "a\r\nx\r\nc\r\n");
    }

    #[tokio::test]
    async fn edit_preserves_utf8_bom() {
        let f = write_temp("\u{FEFF}hello\r\nworld\r\n");
        let path = f.path().to_str().unwrap().to_string();
        let tool = make_tool();
        let args = serde_json::json!({
            "path": path,
            "old_text": "hello\n",
            "new_text": "hi\n"
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes[0..3], [0xEF, 0xBB, 0xBF]);
        let updated = std::fs::read_to_string(&path).unwrap();
        assert_eq!(updated, "\u{FEFF}hi\r\nworld\r\n");
    }

    #[tokio::test]
    async fn edit_no_match_is_error() {
        let f = write_temp("hello world\n");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "old_text": "not found",
            "new_text": "anything"
        });
        let result = tool.execute(args).await;
        assert!(result.is_error, "expected error for no match");
    }

    #[tokio::test]
    async fn edit_multiple_matches_is_error() {
        let f = write_temp("foo foo foo\n");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "old_text": "foo",
            "new_text": "bar"
        });
        let result = tool.execute(args).await;
        assert!(result.is_error, "expected error for multiple matches");
        assert!(
            result.content.as_text().contains("3 times"),
            "expected count in error: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn edit_wrong_type_for_old_text_is_error() {
        let f = write_temp("hello world\n");
        let tool = make_tool();
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "old_text": false,
            "new_text": "goodbye"
        });
        let result = tool.execute(args).await;
        assert!(result.is_error);
        assert!(
            result.content.as_text().contains("Invalid arguments"),
            "expected 'Invalid arguments' in error: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn edit_extra_fields_are_ignored() {
        let f = write_temp("hello world\n");
        let path = f.path().to_str().unwrap().to_string();
        let tool = make_tool();
        let args = serde_json::json!({
            "path": path,
            "old_text": "hello",
            "new_text": "goodbye",
            "dry_run": true
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "goodbye world\n");
    }
}
