use std::pin::Pin;

use globset::Glob;
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::Value;

use crate::agent::types::{CancelLevel, Tool, ToolCallContext, ToolResult};

const DEFAULT_MAX_RESULTS: usize = 100;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SearchMode {
    Literal,
    Regex,
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    path: Option<String>,
    mode: Option<SearchMode>,
    glob: Option<String>,
    case_sensitive: Option<bool>,
    context_before: Option<usize>,
    context_after: Option<usize>,
    max_results: Option<usize>,
}

struct SearchMatch {
    path: String,
    line: usize,
}

/// Search files serially. Keep traversal and matching synchronous so this can
/// run on Tokio's blocking pool now and be partitioned by file later.
///
/// ponytail: reads one candidate text file into memory; switch to buffered line
/// scanning if large-file search becomes a real workload.
fn search_files<F>(
    root: &str,
    matcher: Option<&globset::GlobMatcher>,
    regex: &Regex,
    max_results: usize,
    mut is_cancelled: F,
) -> (Vec<SearchMatch>, bool, bool)
where
    F: FnMut() -> bool,
{
    let mut matches = Vec::new();
    let mut truncated = false;
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .sort_by_file_path(std::cmp::Ord::cmp)
        .build();

    for entry in walker {
        if is_cancelled() {
            return (matches, truncated, true);
        }
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }

        let absolute = entry.path();
        let relative = match absolute.strip_prefix(root) {
            Ok(path) => path.to_string_lossy().replace('\\', "/"),
            Err(_) => absolute.to_string_lossy().replace('\\', "/"),
        };
        if matcher.is_some_and(|glob| !glob.is_match(&relative)) {
            continue;
        }

        let Ok(bytes) = std::fs::read(absolute) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let Ok(content) = std::str::from_utf8(&bytes) else {
            continue;
        };

        for (index, line) in content.lines().enumerate() {
            if is_cancelled() {
                return (matches, truncated, true);
            }
            if regex.is_match(line) {
                matches.push(SearchMatch {
                    path: relative.clone(),
                    line: index + 1,
                });
                if matches.len() > max_results {
                    matches.pop();
                    truncated = true;
                    return (matches, truncated, false);
                }
            }
        }
    }

    (matches, truncated, is_cancelled())
}

fn format_matches(
    matches: &[SearchMatch],
    root: &str,
    before: usize,
    after: usize,
) -> (String, usize) {
    let mut output = String::new();
    let mut unique_files = std::collections::HashSet::new();
    let mut start = 0;

    while start < matches.len() {
        let end = matches[start..]
            .iter()
            .position(|item| item.path != matches[start].path)
            .map_or(matches.len(), |offset| start + offset);
        let path = &matches[start].path;
        unique_files.insert(path.clone());
        if !output.is_empty() {
            output.push_str("\n--\n");
        }
        output.push_str(path);
        output.push('\n');
        let absolute = std::path::Path::new(root).join(path);
        let content = match std::fs::read_to_string(absolute) {
            Ok(content) => content,
            Err(_) => {
                start = end;
                continue;
            }
        };
        let lines: Vec<&str> = content.lines().collect();
        let file_matches = &matches[start..end];
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for item in file_matches {
            let range = (
                item.line.saturating_sub(before.saturating_add(1)),
                item.line.saturating_add(after).min(lines.len()),
            );
            if let Some(last) = ranges.last_mut()
                && range.0 <= last.1
            {
                last.1 = last.1.max(range.1);
            } else {
                ranges.push(range);
            }
        }

        let mut first_range = true;
        for (range_start, range_end) in ranges {
            if !first_range {
                output.push_str("--\n");
            }
            first_range = false;
            for (line_index, line) in lines.iter().enumerate().take(range_end).skip(range_start) {
                let line_number = line_index + 1;
                let is_match = file_matches.iter().any(|item| item.line == line_number);
                output.push_str(if is_match { "> " } else { "  " });
                output.push_str(&format!("{line_number} | {line}\n"));
            }
        }
        start = end;
    }

    (output, unique_files.len())
}

pub struct SearchTool;

impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Search text files synchronously for a literal string or regular expression. Defaults to literal matching with smart case (uppercase query is case-sensitive). Respects ignore files and skips hidden and binary files. Supports a path, file glob, before/after context, and a result limit."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("Search file contents")
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Text to find; interpreted literally unless mode is 'regex'" },
                "path": { "type": "string", "description": "Directory to search (default: current directory)" },
                "mode": { "type": "string", "enum": ["literal", "regex"], "description": "Search mode (default: literal)" },
                "glob": { "type": "string", "description": "Optional glob to restrict files, e.g. '*.rs'" },
                "case_sensitive": { "type": "boolean", "description": "Override smart case (default: uppercase queries are case-sensitive; others are not)" },
                "context_before": { "type": "integer", "description": "Number of preceding context lines (default: 0)" },
                "context_after": { "type": "integer", "description": "Number of following context lines" },
                "max_results": { "type": "integer", "description": "Maximum matching lines to return (default: 100)" }
            },
            "required": ["query"]
        })
    }

    fn streaming_field(&self) -> Option<&'static str> {
        Some("query")
    }

    fn run(
        &self,
        args: Value,
        ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let SearchArgs {
                query,
                path,
                mode,
                glob,
                case_sensitive,
                context_before,
                context_after,
                max_results,
            } = match super::parse_args(args) {
                Ok(args) => args,
                Err(error) => return *error,
            };
            if query.is_empty() {
                return ToolResult::err("Search query must not be empty");
            }
            let root = path.unwrap_or_else(|| ".".to_string());
            if !std::path::Path::new(&root).is_dir() {
                return ToolResult::err(format!("Search directory not found: {root}"));
            }
            let max_results = max_results.unwrap_or(DEFAULT_MAX_RESULTS);
            if max_results == 0 {
                return ToolResult::err("max_results must be greater than zero");
            }
            let mode = mode.unwrap_or(SearchMode::Literal);
            let sensitive = case_sensitive.unwrap_or_else(|| query.chars().any(char::is_uppercase));
            let expression = match mode {
                SearchMode::Literal => regex::escape(&query),
                SearchMode::Regex => query,
            };
            let regex = match RegexBuilder::new(&expression)
                .case_insensitive(!sensitive)
                .build()
            {
                Ok(regex) => regex,
                Err(error) => return ToolResult::err(format!("Invalid search regex: {error}")),
            };
            let matcher = match glob {
                Some(pattern) => match Glob::new(&pattern) {
                    Ok(glob) => Some(glob.compile_matcher()),
                    Err(error) => {
                        return ToolResult::err(format!("Invalid file glob '{pattern}': {error}"));
                    }
                },
                None => None,
            };
            let before = context_before.unwrap_or(0);
            let after = context_after.unwrap_or(0);
            let cancel_rx = ctx.cancel_rx.clone();
            let search = tokio::task::spawn_blocking(move || {
                let is_cancelled = || {
                    cancel_rx
                        .as_ref()
                        .is_some_and(|rx| *rx.borrow() >= CancelLevel::HardAbort)
                };
                let (matches, truncated, cancelled) =
                    search_files(&root, matcher.as_ref(), &regex, max_results, is_cancelled);
                if cancelled {
                    return Err("Search cancelled by user".to_string());
                }
                let (mut output, file_count) = format_matches(&matches, &root, before, after);
                if output.is_empty() {
                    output.push_str("No matches found");
                } else {
                    output.push_str(&format!(
                        "\n\n{} match{} in {} file{}",
                        matches.len(),
                        if matches.len() == 1 { "" } else { "es" },
                        file_count,
                        if file_count == 1 { "" } else { "s" }
                    ));
                }
                if truncated {
                    output.push_str(&format!("; result limit reached ({max_results})"));
                }
                Ok(output)
            })
            .await;
            match search {
                Ok(Ok(output)) => ToolResult::ok_str(output),
                Ok(Err(error)) => ToolResult::err(error),
                Err(error) => ToolResult::err(format!("Search failed: {error}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn literal_search_is_smart_case_and_supports_glob_and_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.rs"), "before\nhello\nafter\nHELLO\n").unwrap();
        std::fs::write(dir.path().join("two.txt"), "hello\n").unwrap();
        let result = SearchTool
            .execute(serde_json::json!({
                "query": "hello",
                "path": dir.path(),
                "glob": "*.rs",
                "context_before": 1,
                "context_after": 1
            }))
            .await;
        assert!(!result.is_error, "{}", result.content.as_text());
        let text = result.content.as_text();
        assert!(text.contains("before"));
        assert!(text.contains("> 2 | hello"));
        assert!(text.contains("  3 | after"));
        assert!(text.contains("> 4 | HELLO"));
        assert!(!text.contains("two.txt"));
        let uppercase = SearchTool
            .execute(serde_json::json!({ "query": "HELLO", "path": dir.path(), "glob": "*.rs" }))
            .await;
        assert!(!uppercase.is_error);
        assert!(uppercase.content.as_text().contains("> 4 | HELLO"));
        assert!(!uppercase.content.as_text().contains("> 2 | hello"));
    }

    #[tokio::test]
    async fn regex_search_reports_invalid_patterns_and_result_truncation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.txt"), "abc\naxc\nabc\n").unwrap();
        let tool = SearchTool;
        let invalid = tool
            .execute(serde_json::json!({ "query": "[", "path": dir.path(), "mode": "regex" }))
            .await;
        assert!(invalid.is_error);
        let result = tool
            .execute(serde_json::json!({
                "query": "a.c",
                "path": dir.path(),
                "mode": "regex",
                "max_results": 1
            }))
            .await;
        assert!(!result.is_error);
        assert!(result.content.as_text().contains("limit reached (1)"));
        assert!(result.content.as_text().contains("1 match in 1 file"));
    }
}
