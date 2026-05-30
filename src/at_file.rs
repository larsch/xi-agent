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
}

/// Extract all `@<path>` tokens from `input`.
///
/// Tokens must be preceded by start-of-string or ASCII whitespace.
pub fn parse_at_tokens(input: &str) -> Vec<AtToken> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '@' {
            // Check that `@` is at start or preceded by whitespace.
            let preceded_by_space = i == 0 || chars[i - 1].is_ascii_whitespace();
            if preceded_by_space && i + 1 < len {
                let next = chars[i + 1];
                if next == '"' {
                    // Quoted form: @"..."
                    let start = i + 2;
                    let mut end = start;
                    while end < len && chars[end] != '"' {
                        end += 1;
                    }
                    let path: String = chars[start..end].iter().collect();
                    if !path.is_empty() {
                        tokens.push(AtToken { path });
                    }
                    i = if end < len { end + 1 } else { end };
                    continue;
                } else if !next.is_ascii_whitespace() {
                    // Unquoted form: @word
                    let start = i + 1;
                    let mut end = start;
                    while end < len && !chars[end].is_ascii_whitespace() {
                        end += 1;
                    }
                    let path: String = chars[start..end].iter().collect();
                    if !path.is_empty() {
                        tokens.push(AtToken { path });
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
                path: "src/main.rs".into()
            }]
        );
    }

    #[test]
    fn parse_at_start_of_string() {
        let tokens = parse_at_tokens("@Cargo.toml");
        assert_eq!(
            tokens,
            vec![AtToken {
                path: "Cargo.toml".into()
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
                    path: "foo.rs".into()
                },
                AtToken {
                    path: "bar.rs".into()
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
                path: "path with spaces.txt".into()
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
        }];
        let results = resolve_at_tokens(&tokens, dir.path());
        assert!(matches!(&results[0], AtFileResult::Error { .. }));
    }
}
