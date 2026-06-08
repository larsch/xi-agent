//! Parsing and resolution of `@<path>` file-attachment tokens in user input.
//!
//! When a user types `@src/main.rs` (or `@"path with spaces.txt"`) in the
//! textarea, xi-agent resolves the path and injects a synthetic `read_file`
//! tool call + result before the user message so the model receives the file
//! content without a round-trip tool call.
//!
//! # Token syntax
//!
//! - `@path/to/file` — unquoted; terminated by whitespace or end of string.
//! - `@"path/to/file with spaces"` — double-quoted; terminated by closing `"`.
//!
//! A token must be preceded by start-of-string or ASCII whitespace.

use std::path::{Path, PathBuf};

// ── Token parsing ─────────────────────────────────────────────────────────────

/// A single `@<path>` token extracted from user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtToken {
    /// The raw path string as typed (without the leading `@` or surrounding
    /// quotes).
    pub path: String,
    /// Byte offset of the `@` character in the original input string.
    pub span_start: usize,
    /// Byte offset after the last character of this token in the original
    /// input string (i.e. the exclusive end of the `@path` / `@"path"` span).
    pub span_end: usize,
}

/// Extract all `@<path>` tokens from `input`.
///
/// Tokens must be preceded by start-of-string or ASCII whitespace.
pub fn parse_at_tokens(input: &str) -> Vec<AtToken> {
    let mut tokens = Vec::new();
    let char_offsets: Vec<(usize, char)> = input.char_indices().collect();
    let len = char_offsets.len();
    let mut i = 0;

    while i < len {
        let (at_byte, ch) = char_offsets[i];
        if ch == '@' {
            // Check that `@` is at start or preceded by whitespace.
            let preceded_by_space = i == 0 || char_offsets[i - 1].1.is_ascii_whitespace();
            if preceded_by_space && i + 1 < len {
                let next = char_offsets[i + 1].1;
                if next == '"' {
                    // Quoted form: @"..."
                    let start = i + 2;
                    let mut end = start;
                    while end < len && char_offsets[end].1 != '"' {
                        end += 1;
                    }
                    let path: String = char_offsets[start..end].iter().map(|(_, c)| c).collect();
                    if !path.is_empty() {
                        let span_end = if end < len {
                            char_offsets[end].0 + '"'.len_utf8()
                        } else {
                            input.len()
                        };
                        tokens.push(AtToken {
                            path,
                            span_start: at_byte,
                            span_end,
                        });
                    }
                    i = if end < len { end + 1 } else { end };
                    continue;
                } else if !next.is_ascii_whitespace() {
                    // Unquoted form: @word
                    let start = i + 1;
                    let mut end = start;
                    while end < len && !char_offsets[end].1.is_ascii_whitespace() {
                        end += 1;
                    }
                    let path: String = char_offsets[start..end].iter().map(|(_, c)| c).collect();
                    if !path.is_empty() {
                        let span_end = if end < len {
                            char_offsets[end].0
                        } else {
                            input.len()
                        };
                        tokens.push(AtToken {
                            path,
                            span_start: at_byte,
                            span_end,
                        });
                    }
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }

    tokens
}

/// Rewrite user input text, replacing each successfully resolved `@<path>`
/// token with the equivalent backtick-delimited file reference.
///
/// Only tokens whose corresponding [`AtFileResult`] is *not* an error are
/// rewritten.  Missing or unreadable files keep the original `@` notation.
pub fn rewrite_user_text(text: &str, results: &[AtFileResult]) -> String {
    let tokens = parse_at_tokens(text);
    if tokens.is_empty() || results.is_empty() {
        return text.to_string();
    }

    // Collect (start, end, replacement) for each successfully resolved token.
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for (tok, result) in tokens.iter().zip(results.iter()) {
        if !matches!(result, AtFileResult::Error { .. }) {
            replacements.push((tok.span_start, tok.span_end, format!("`{}`", tok.path)));
        }
    }

    if replacements.is_empty() {
        return text.to_string();
    }

    // Apply from end to start so earlier byte offsets stay valid.
    replacements.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));

    let mut result = text.to_string();
    for (start, end, replacement) in &replacements {
        result.replace_range(*start..*end, replacement);
    }
    result
}

// ── File resolution ───────────────────────────────────────────────────────────

/// The result of resolving one [`AtToken`].
#[derive(Debug, Clone)]
pub enum AtFileResult {
    /// A text file was read successfully.
    Text {
        /// Absolute or display path.
        path: String,
        content: String,
    },
    /// An image file was read successfully.
    Image {
        path: String,
        base64: String,
        mime_type: String,
    },
    /// The file could not be read.
    Error { path: String, message: String },
}

impl AtFileResult {
    /// The path this result corresponds to.
    pub fn path(&self) -> &str {
        match self {
            Self::Text { path, .. } | Self::Image { path, .. } | Self::Error { path, .. } => path,
        }
    }
}

/// Image MIME type detected from file extension.
fn mime_from_extension(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

/// Expand a leading `~/` to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let (Some(rest), Some(home)) = (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

/// Resolve a slice of [`AtToken`]s relative to `cwd`, reading each file.
///
/// Uses blocking `std::fs` — files are expected to be local and small.
pub fn resolve_at_tokens(tokens: &[AtToken], cwd: &Path) -> Vec<AtFileResult> {
    tokens
        .iter()
        .map(|tok| {
            let expanded = expand_tilde(&tok.path);
            let abs = if expanded.is_absolute() {
                expanded
            } else {
                cwd.join(&expanded)
            };

            let display = tok.path.clone();

            match mime_from_extension(&abs) {
                Some(mime_type) => {
                    // Image file
                    match std::fs::read(&abs) {
                        Ok(bytes) => {
                            use base64::{Engine as _, engine::general_purpose::STANDARD};
                            AtFileResult::Image {
                                path: display,
                                base64: STANDARD.encode(&bytes),
                                mime_type: mime_type.to_string(),
                            }
                        }
                        Err(e) => AtFileResult::Error {
                            path: display,
                            message: e.to_string(),
                        },
                    }
                }
                None => {
                    // Text file
                    match std::fs::read_to_string(&abs) {
                        Ok(content) => AtFileResult::Text {
                            path: display,
                            content,
                        },
                        Err(e) => AtFileResult::Error {
                            path: display,
                            message: e.to_string(),
                        },
                    }
                }
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_tokens() {
        assert!(parse_at_tokens("hello world").is_empty());
        assert!(parse_at_tokens("").is_empty());
        assert!(parse_at_tokens("/no/at/sign").is_empty());
    }

    #[test]
    fn parse_single_unquoted() {
        let tokens = parse_at_tokens("look at @src/main.rs please");
        assert_eq!(
            tokens,
            vec![AtToken {
                path: "src/main.rs".into(),
                span_start: 8,
                span_end: 20,
            }]
        );
    }

    #[test]
    fn parse_at_start_of_string() {
        let tokens = parse_at_tokens("@Cargo.toml");
        assert_eq!(
            tokens,
            vec![AtToken {
                path: "Cargo.toml".into(),
                span_start: 0,
                span_end: 11,
            }]
        );
    }

    #[test]
    fn parse_multiple_tokens() {
        let tokens = parse_at_tokens("@foo.rs and @bar.rs");
        assert_eq!(
            tokens,
            vec![
                AtToken {
                    path: "foo.rs".into(),
                    span_start: 0,
                    span_end: 7,
                },
                AtToken {
                    path: "bar.rs".into(),
                    span_start: 12,
                    span_end: 19,
                },
            ]
        );
    }

    #[test]
    fn parse_quoted_form() {
        let tokens = parse_at_tokens(r#"see @"path with spaces.txt" please"#);
        assert_eq!(
            tokens,
            vec![AtToken {
                path: "path with spaces.txt".into(),
                span_start: 4,
                span_end: 27,
            }]
        );
    }

    #[test]
    fn parse_ignores_mid_word_at() {
        // `user@host` — not preceded by whitespace, should be ignored.
        let tokens = parse_at_tokens("email user@host.com here");
        assert!(tokens.is_empty());
    }

    #[test]
    fn parse_ignores_lone_at() {
        let tokens = parse_at_tokens("@ nothing");
        assert!(tokens.is_empty());
    }

    #[test]
    fn mime_detection() {
        assert_eq!(mime_from_extension(Path::new("foo.png")), Some("image/png"));
        assert_eq!(
            mime_from_extension(Path::new("foo.jpg")),
            Some("image/jpeg")
        );
        assert_eq!(
            mime_from_extension(Path::new("foo.jpeg")),
            Some("image/jpeg")
        );
        assert_eq!(mime_from_extension(Path::new("foo.gif")), Some("image/gif"));
        assert_eq!(
            mime_from_extension(Path::new("foo.webp")),
            Some("image/webp")
        );
        assert_eq!(mime_from_extension(Path::new("foo.rs")), None);
        assert_eq!(mime_from_extension(Path::new("foo.PNG")), Some("image/png"));
    }

    #[test]
    fn resolve_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "hello world").unwrap();
        let tokens = vec![AtToken {
            path: "hello.txt".into(),
            span_start: 0,
            span_end: 10,
        }];
        let results = resolve_at_tokens(&tokens, dir.path());
        assert!(
            matches!(&results[0], AtFileResult::Text { content, .. } if content == "hello world")
        );
    }

    #[test]
    fn resolve_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = vec![AtToken {
            path: "nope.txt".into(),
            span_start: 0,
            span_end: 9,
        }];
        let results = resolve_at_tokens(&tokens, dir.path());
        assert!(matches!(&results[0], AtFileResult::Error { .. }));
    }

    // ── rewrite_user_text tests ───────────────────────────────────────────

    #[test]
    fn rewrite_single_unquoted() {
        let text = "look at @src/main.rs please";
        let results = vec![AtFileResult::Text {
            path: "src/main.rs".into(),
            content: "fn main() {}".into(),
        }];
        assert_eq!(
            rewrite_user_text(text, &results),
            "look at `src/main.rs` please"
        );
    }

    #[test]
    fn rewrite_drops_error() {
        let text = "@missing.txt is gone";
        let results = vec![AtFileResult::Error {
            path: "missing.txt".into(),
            message: "No such file".into(),
        }];
        // Error tokens are left as-is (original `@` notation).
        assert_eq!(rewrite_user_text(text, &results), "@missing.txt is gone");
    }

    #[test]
    fn rewrite_quoted_form() {
        let text = r#"see @"path with spaces.txt" here"#;
        let results = vec![AtFileResult::Text {
            path: "path with spaces.txt".into(),
            content: "hello".into(),
        }];
        assert_eq!(
            rewrite_user_text(text, &results),
            "see `path with spaces.txt` here"
        );
    }

    #[test]
    fn rewrite_mixed_resolved_and_error() {
        let text = "@good.txt and @bad.txt";
        let results = vec![
            AtFileResult::Text {
                path: "good.txt".into(),
                content: "ok".into(),
            },
            AtFileResult::Error {
                path: "bad.txt".into(),
                message: "not found".into(),
            },
        ];
        assert_eq!(rewrite_user_text(text, &results), "`good.txt` and @bad.txt");
    }

    #[test]
    fn rewrite_no_tokens_is_identity() {
        assert_eq!(rewrite_user_text("plain text", &[]), "plain text");
    }

    #[test]
    fn rewrite_empty_results_is_identity() {
        assert_eq!(rewrite_user_text("@foo.txt", &[]), "@foo.txt");
    }
}
