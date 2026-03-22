use std::pin::Pin;

use serde_json::Value;

use crate::agent::types::{Tool, ToolResult};

pub struct EditTool;

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
                Err(e) => return e,
            };

            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(e) => return ToolResult::err(format!("Failed to read {path}: {e}")),
            };

            let count = content.matches(old_text.as_str()).count();
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

            let updated = content.replacen(old_text.as_str(), new_text.as_str(), 1);

            if let Err(e) = tokio::fs::write(&path, updated.as_bytes()).await {
                return ToolResult::err(format!("Failed to write {path}: {e}"));
            }

            ToolResult::ok(format!("Successfully edited {path}"))
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
    async fn edit_replaces_exact_match() {
        let f = write_temp("hello world\n");
        let path = f.path().to_str().unwrap().to_string();
        let tool = EditTool;
        let args = serde_json::json!({
            "path": path,
            "old_text": "hello",
            "new_text": "goodbye"
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        let updated = std::fs::read_to_string(&path).unwrap();
        assert_eq!(updated, "goodbye world\n");
    }

    #[tokio::test]
    async fn edit_no_match_is_error() {
        let f = write_temp("hello world\n");
        let tool = EditTool;
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
        let tool = EditTool;
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "old_text": "foo",
            "new_text": "bar"
        });
        let result = tool.execute(args).await;
        assert!(result.is_error, "expected error for multiple matches");
        assert!(
            result.content.contains("3 times"),
            "expected count in error: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn edit_wrong_type_for_old_text_is_error() {
        let f = write_temp("hello world\n");
        let tool = EditTool;
        let args = serde_json::json!({
            "path": f.path().to_str().unwrap(),
            "old_text": false,
            "new_text": "goodbye"
        });
        let result = tool.execute(args).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("Invalid arguments"),
            "expected 'Invalid arguments' in error: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn edit_extra_fields_are_ignored() {
        let f = write_temp("hello world\n");
        let path = f.path().to_str().unwrap().to_string();
        let tool = EditTool;
        let args = serde_json::json!({
            "path": path,
            "old_text": "hello",
            "new_text": "goodbye",
            "dry_run": true
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "goodbye world\n");
    }
}
