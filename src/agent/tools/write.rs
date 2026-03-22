use std::pin::Pin;

use serde_json::Value;

use crate::agent::types::{Tool, ToolResult};

pub struct WriteTool;

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
                    "description": "Path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn execute(
        &self,
        args: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let WriteArgs { path, content } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return e,
            };

            // Create parent directories if needed.
            if let Some(parent) = std::path::Path::new(&path).parent()
                && !parent.as_os_str().is_empty()
                && let Err(e) = tokio::fs::create_dir_all(parent).await
            {
                return ToolResult::err(format!("Failed to create directories for {path}: {e}"));
            }

            if let Err(e) = tokio::fs::write(&path, content.as_bytes()).await {
                return ToolResult::err(format!("Failed to write {path}: {e}"));
            }

            let line_count = content.lines().count();
            ToolResult::ok(format!("Written {line_count} lines to {path}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::Tool;

    #[tokio::test]
    async fn write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new_file.txt");
        let tool = WriteTool;
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "hello\n"
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(path.exists(), "file was not created");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    }

    #[tokio::test]
    async fn write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "old content\n").unwrap();
        let tool = WriteTool;
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
        let tool = WriteTool;
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "deep\n"
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(path.exists(), "file not created in nested dirs");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "deep\n");
    }

    #[tokio::test]
    async fn write_wrong_type_for_content_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let tool = WriteTool;
        let args = serde_json::json!({"path": path.to_str().unwrap(), "content": 99});
        let result = tool.execute(args).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("Invalid arguments"),
            "expected 'Invalid arguments' in error: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn write_extra_fields_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let tool = WriteTool;
        let args =
            serde_json::json!({"path": path.to_str().unwrap(), "content": "hi\n", "mode": "644"});
        let result = tool.execute(args).await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi\n");
    }
}
