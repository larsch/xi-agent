use std::pin::Pin;

use globset::Glob;
use ignore::WalkBuilder;
use serde_json::Value;

use crate::agent::types::{Tool, ToolResult};

const DEFAULT_LIMIT: usize = 1000;

pub struct FindTool;

#[derive(serde::Deserialize)]
struct FindArgs {
    pattern: String,
    path: Option<String>,
    limit: Option<usize>,
}

impl Tool for FindTool {
    fn name(&self) -> &str {
        "find_files"
    }

    fn description(&self) -> &str {
        "Search for files matching a glob pattern. Returns file paths relative \
         to the search directory, one per line, sorted alphabetically. \
         Excludes hidden files and paths ignored by .gitignore. Output is \
         capped at `limit` results (default 1000); a notice is appended when \
         the cap is reached."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match file names relative to the `path` argument, e.g. '*.rs', '**/*.json'"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: current directory)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 1000)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn streaming_field(&self) -> Option<String> {
        Some("pattern".to_string())
    }

    fn execute(
        &self,
        args: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let FindArgs {
                pattern,
                path,
                limit,
            } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };
            let search_dir = path.unwrap_or_else(|| ".".to_string());
            let limit = limit.unwrap_or(DEFAULT_LIMIT);

            // Compile the glob pattern up front so we can report errors early.
            // We use `**/<pattern>` anchoring so bare patterns like `*.rs`
            // match anywhere in the tree, consistent with pi-mono behaviour.
            let matcher = match Glob::new(&pattern) {
                Ok(g) => g.compile_matcher(),
                Err(e) => return ToolResult::err(format!("Invalid glob pattern '{pattern}': {e}")),
            };

            // Verify the search directory exists before spawning the walker.
            if !std::path::Path::new(&search_dir).exists() {
                return ToolResult::err(format!("Path not found: {search_dir}"));
            }

            // The ignore::Walk API is synchronous; run it on the blocking thread pool.
            let result = tokio::task::spawn_blocking(move || {
                let mut matches: Vec<String> = Vec::new();

                let walker = WalkBuilder::new(&search_dir)
                    // Exclude hidden files/directories (dotfiles, .git, etc.).
                    .hidden(true)
                    // Respect .gitignore, .ignore, and global gitignore.
                    .git_ignore(true)
                    .git_global(true)
                    .git_exclude(true)
                    .sort_by_file_path(std::cmp::Ord::cmp)
                    .build();

                for entry in walker {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // Skip directory entries themselves; we only report files.
                    if entry.file_type().is_some_and(|t| t.is_dir()) {
                        continue;
                    }

                    // Build a forward-slash relative path for matching.
                    let abs = entry.path();
                    let rel = match abs.strip_prefix(&search_dir) {
                        Ok(p) => p.to_string_lossy().replace('\\', "/"),
                        Err(_) => abs.to_string_lossy().replace('\\', "/"),
                    };

                    if matcher.is_match(&rel) {
                        matches.push(rel);
                        // Collect one extra to detect whether the limit was hit.
                        if matches.len() > limit {
                            break;
                        }
                    }
                }

                matches
            })
            .await;

            let mut matches = match result {
                Ok(m) => m,
                Err(e) => return ToolResult::err(format!("Find failed: {e}")),
            };

            let limit_reached = matches.len() > limit;
            if limit_reached {
                matches.truncate(limit);
            }

            if matches.is_empty() {
                return ToolResult::ok_str("No files found matching pattern");
            }

            let mut output = matches.join("\n");

            if limit_reached {
                output.push_str(&format!(
                    "\n\n[{limit} result limit reached — use limit=N for more or refine the pattern]"
                ));
            }

            ToolResult::ok_str(output)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::Tool;

    #[tokio::test]
    async fn find_missing_pattern_is_error() {
        let tool = FindTool;
        let args = serde_json::json!({});
        let result = tool.execute(args).await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn find_wrong_type_for_pattern_is_error() {
        let tool = FindTool;
        let args = serde_json::json!({"pattern": 42});
        let result = tool.execute(args).await;
        assert!(result.is_error);
        assert!(
            result.content.as_text().contains("Invalid arguments"),
            "expected 'Invalid arguments' in error: {}",
            result.content.as_text()
        );
    }

    #[tokio::test]
    async fn find_extra_fields_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.rs"), "fn main() {}").unwrap();
        let tool = FindTool;
        let args = serde_json::json!({
            "pattern": "*.rs",
            "path": dir.path().to_str().unwrap(),
            "recursive": true
        });
        let result = tool.execute(args).await;
        assert!(
            !result.is_error,
            "unexpected error: {}",
            result.content.as_text()
        );
        assert!(result.content.as_text().contains("hello.rs"));
    }

    #[tokio::test]
    async fn find_returns_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.rs"), "").unwrap();
        std::fs::write(dir.path().join("bar.txt"), "").unwrap();
        let tool = FindTool;
        let args = serde_json::json!({
            "pattern": "*.rs",
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute(args).await;
        assert!(!result.is_error);
        assert!(
            result.content.as_text().contains("foo.rs"),
            "expected foo.rs: {}",
            result.content.as_text()
        );
        assert!(
            !result.content.as_text().contains("bar.txt"),
            "unexpected bar.txt: {}",
            result.content.as_text()
        );
    }
}
