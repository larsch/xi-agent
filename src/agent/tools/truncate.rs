/// Shared truncation utilities for tool outputs.
///
/// Two independent limits — whichever is hit first wins:
/// - Line limit (default: 2000 lines)
/// - Byte limit (default: 50 KiB)
///
/// `truncate_tail` keeps the *last* N lines — suited for bash output where
/// errors and final results appear at the end.
/// `truncate_head` keeps the *first* N lines — suited for file reads.
pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024; // 50 KiB

#[derive(Debug, Clone)]
pub struct TruncationResult {
    /// The truncated content (never contains a trailing newline added by us).
    pub content: String,
    /// Whether any truncation occurred.
    pub truncated: bool,
    /// Total lines in the original content.
    pub total_lines: usize,
    /// Total bytes in the original content.
    #[allow(dead_code)]
    pub total_bytes: usize,
    /// Number of complete lines kept in the output.
    pub output_lines: usize,
    /// 1-indexed line number of the first kept line.
    pub first_kept_line: usize,
}

/// Keep the **last** `max_lines` / `max_bytes` of `content`.
/// Returns complete lines only (never a partial line).
pub fn truncate_tail(content: &str) -> TruncationResult {
    truncate_tail_with_limits(content, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES)
}

pub const SINGLE_LINE_MAX_BYTES: usize = 240;

pub fn truncate_tail_with_limits(
    content: &str,
    max_lines: usize,
    max_bytes: usize,
) -> TruncationResult {
    let total_bytes = content.len();

    // Strip a single trailing newline before splitting so it doesn't produce
    // a phantom empty last line (e.g. "a\nb\n".split('\n') == ["a","b",""]).
    // We restore it on the way out.
    let (trimmed, had_trailing_newline) = if let Some(s) = content.strip_suffix('\n') {
        (s, true)
    } else {
        (content, false)
    };

    let lines: Vec<&str> = trimmed.split('\n').collect();
    let total_lines = lines.len();

    // No truncation needed — compare against trimmed bytes so the trailing
    // newline doesn't push content past the byte limit spuriously.
    let trimmed_bytes = trimmed.len();
    if total_lines <= max_lines && trimmed_bytes <= max_bytes {
        // Still apply the single-line cap even when no other truncation fired.
        if total_lines == 1 && trimmed_bytes > SINGLE_LINE_MAX_BYTES {
            let capped = cap_line(trimmed, SINGLE_LINE_MAX_BYTES);
            let mut content_out = capped.to_string();
            if had_trailing_newline {
                content_out.push('\n');
            }
            return TruncationResult {
                content: content_out,
                truncated: true,
                total_lines,
                total_bytes,
                output_lines: 1,
                first_kept_line: 1,
            };
        }
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            first_kept_line: 1,
        };
    }

    // Work backwards from the end collecting complete lines.
    let mut kept: Vec<&str> = Vec::new();
    let mut kept_bytes: usize = 0;

    for line in lines.iter().rev() {
        // +1 for the newline separator (except before the first kept line)
        let separator = if kept.is_empty() { 0 } else { 1 };
        let line_bytes = line.len() + separator;

        if kept.len() >= max_lines || kept_bytes + line_bytes > max_bytes {
            break;
        }

        kept.push(line);
        kept_bytes += line_bytes;
    }

    // Edge case: a single line exceeds max_bytes — keep it anyway rather than
    // returning empty content with nonsensical line numbers.
    if kept.is_empty()
        && let Some(last) = lines.last()
    {
        kept.push(last);
    }

    // Reverse to restore original order.
    kept.reverse();

    // When the result is a single long line, cap it at SINGLE_LINE_MAX_BYTES.
    if kept.len() == 1 && kept[0].len() > SINGLE_LINE_MAX_BYTES {
        kept[0] = cap_line(kept[0], SINGLE_LINE_MAX_BYTES);
    }

    let output_lines = kept.len();
    let first_kept_line = total_lines - output_lines + 1;
    let mut content_out = kept.join("\n");
    if had_trailing_newline {
        content_out.push('\n');
    }

    TruncationResult {
        content: content_out,
        truncated: true,
        total_lines,
        total_bytes,
        output_lines,
        first_kept_line,
    }
}

/// Keep the **first** `max_lines` / `max_bytes` of `content`.
/// Returns complete lines only.
#[allow(dead_code)]
pub fn truncate_head(content: &str) -> TruncationResult {
    truncate_head_with_limits(content, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES)
}

#[allow(dead_code)]
pub fn truncate_head_with_limits(
    content: &str,
    max_lines: usize,
    max_bytes: usize,
) -> TruncationResult {
    let total_bytes = content.len();
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();

    // No truncation needed.
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            first_kept_line: 1,
        };
    }

    let mut kept: Vec<&str> = Vec::new();
    let mut kept_bytes: usize = 0;

    for line in lines.iter().take(max_lines) {
        let separator = if kept.is_empty() { 0 } else { 1 };
        let line_bytes = line.len() + separator;

        if kept_bytes + line_bytes > max_bytes {
            break;
        }

        kept.push(line);
        kept_bytes += line_bytes;
    }

    let output_lines = kept.len();
    let content_out = kept.join("\n");

    TruncationResult {
        content: content_out,
        truncated: true,
        total_lines,
        total_bytes,
        output_lines,
        first_kept_line: 1,
    }
}

/// Truncate `line` to at most `max_bytes`, respecting UTF-8 char boundaries.
fn cap_line(line: &str, max_bytes: usize) -> &str {
    if line.len() <= max_bytes {
        return line;
    }
    let mut end = max_bytes;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_no_truncation_when_small() {
        let r = truncate_tail("line1\nline2\nline3");
        assert!(!r.truncated);
        assert_eq!(r.content, "line1\nline2\nline3");
        assert_eq!(r.total_lines, 3);
        assert_eq!(r.output_lines, 3);
        assert_eq!(r.first_kept_line, 1);
    }

    #[test]
    fn tail_keeps_last_lines_when_line_limit_hit() {
        let lines: Vec<String> = (1..=10).map(|i| format!("line{i}")).collect();
        let content = lines.join("\n");
        let r = truncate_tail_with_limits(&content, 3, usize::MAX);
        assert!(r.truncated);
        assert_eq!(r.output_lines, 3);
        assert_eq!(r.first_kept_line, 8);
        assert_eq!(r.content, "line8\nline9\nline10");
    }

    #[test]
    fn tail_keeps_last_bytes_when_byte_limit_hit() {
        // Each line is "lineXX\n" — keep only what fits in ~15 bytes.
        let content = "line1\nline2\nline3\nline4\nline5";
        // "line4\nline5" = 11 bytes, "line3\nline4\nline5" = 17 bytes
        let r = truncate_tail_with_limits(content, usize::MAX, 12);
        assert!(r.truncated);
        assert!(r.content.contains("line5"));
        assert!(!r.content.contains("line1"));
    }

    #[test]
    fn head_no_truncation_when_small() {
        let r = truncate_head("a\nb\nc");
        assert!(!r.truncated);
        assert_eq!(r.content, "a\nb\nc");
        assert_eq!(r.first_kept_line, 1);
    }

    #[test]
    fn head_keeps_first_lines_when_line_limit_hit() {
        let lines: Vec<String> = (1..=10).map(|i| format!("line{i}")).collect();
        let content = lines.join("\n");
        let r = truncate_head_with_limits(&content, 3, usize::MAX);
        assert!(r.truncated);
        assert_eq!(r.output_lines, 3);
        assert_eq!(r.first_kept_line, 1);
        assert_eq!(r.content, "line1\nline2\nline3");
    }

    #[test]
    fn tail_total_lines_is_accurate() {
        let content = "a\nb\nc\nd\ne";
        let r = truncate_tail_with_limits(content, 2, usize::MAX);
        assert_eq!(r.total_lines, 5);
        assert_eq!(r.output_lines, 2);
        assert_eq!(r.first_kept_line, 4);
    }

    #[test]
    fn tail_trailing_newline_not_counted_as_extra_line() {
        // "a\nb\nc\n" should be treated as 3 lines, not 4.
        let content = "a\nb\nc\n";
        let r = truncate_tail("a\nb\nc\n");
        assert!(!r.truncated);
        assert_eq!(r.total_lines, 3);
        assert_eq!(r.output_lines, 3);
        assert_eq!(r.content, content);
    }

    #[test]
    fn tail_truncated_output_with_trailing_newline_has_correct_line_numbers() {
        // 5 lines with trailing newline, keep last 2.
        let content = "a\nb\nc\nd\ne\n";
        let r = truncate_tail_with_limits(content, 2, usize::MAX);
        assert!(r.truncated);
        assert_eq!(r.total_lines, 5);
        assert_eq!(r.output_lines, 2);
        assert_eq!(r.first_kept_line, 4);
        assert_eq!(r.content, "d\ne\n");
    }

    #[test]
    fn tail_truncated_content_is_not_empty() {
        // Regression: trailing newline must not produce empty content.
        let lines: Vec<String> = (1..=10).map(|i| format!("line{i}")).collect();
        let content = lines.join("\n") + "\n";
        let r = truncate_tail_with_limits(&content, 3, usize::MAX);
        assert!(r.truncated);
        assert!(!r.content.trim().is_empty(), "content must not be empty");
        assert_eq!(r.content, "line8\nline9\nline10\n");
    }

    #[test]
    fn tail_no_truncation_when_trailing_newline_is_at_byte_boundary() {
        // Regression: trailing newline must not push content past the byte
        // limit when the actual text is within bounds.
        // "ab\n" = 3 bytes; with max_bytes=2, trimmed="ab" = 2 bytes → no truncation.
        let r = truncate_tail_with_limits("ab\n", usize::MAX, 2);
        assert!(!r.truncated);
        assert_eq!(r.content, "ab\n");
    }

    #[test]
    fn tail_single_line_exceeding_byte_limit_is_kept() {
        // A single line that exceeds max_bytes must still be returned rather
        // than producing empty content with nonsensical line numbers.
        let r = truncate_tail_with_limits("hello world\n", usize::MAX, 5);
        assert!(r.truncated);
        assert_eq!(r.output_lines, 1);
        assert_eq!(r.first_kept_line, 1);
        assert!(!r.content.is_empty());
    }

    #[test]
    fn tail_single_line_capped_at_240_bytes() {
        let long_line = "x".repeat(300) + "\n";
        let r = truncate_tail(&long_line);
        assert!(r.truncated);
        assert_eq!(r.output_lines, 1);
        // Content should be capped at SINGLE_LINE_MAX_BYTES (plus trailing newline).
        assert!(r.content.trim_end_matches('\n').len() <= SINGLE_LINE_MAX_BYTES);
    }

    #[test]
    fn tail_multi_line_not_capped_at_240_bytes() {
        // The 240-byte cap only applies when output_lines == 1.
        let long_line = "x".repeat(300);
        let content = format!("first\n{long_line}\n");
        let r = truncate_tail_with_limits(&content, 2, usize::MAX);
        // Two lines kept — single-line cap must not apply.
        assert_eq!(r.output_lines, 2);
        assert!(r.content.contains(&long_line));
    }
}
