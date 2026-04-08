/// Applies terminal-like carriage return behavior to a string.
///
/// Simulates how a terminal renders text with carriage returns (\r):
/// - Regular characters are written at the current cursor position
/// - '\r' moves the cursor to the start of the current line (overwrites from there)
/// - '\n' finalizes the current line and moves to the next
///
/// For example: "foobar\rbaz\r\n" becomes "bazbar\n"
/// - "foobar" is written
/// - \r moves cursor to position 0
/// - "baz" overwrites positions 0-2, resulting in "bazbar"
/// - \r moves cursor to position 0 (no visible effect for final output)
/// - \n finalizes the line
pub fn apply_terminal_render(s: &str) -> String {
    let mut output = String::new();
    let mut current_line: Vec<char> = Vec::new();
    let mut cursor_pos = 0usize;

    for ch in s.chars() {
        match ch {
            '\r' => {
                // Carriage return: move cursor to start of line
                cursor_pos = 0;
            }
            '\n' => {
                // Line feed: finalize current line and start fresh
                output.push_str(&current_line.iter().collect::<String>());
                output.push('\n');
                current_line.clear();
                cursor_pos = 0;
            }
            _ => {
                // Regular character: write at cursor position
                // Expand the line if necessary
                if cursor_pos >= current_line.len() {
                    current_line.resize(cursor_pos + 1, ' ');
                }
                current_line[cursor_pos] = ch;
                cursor_pos += 1;
            }
        }
    }

    // Flush any remaining line content
    if !current_line.is_empty() {
        output.push_str(&current_line.iter().collect::<String>());
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_carriage_return_overwrite() {
        // "foobar\rbaz\r\n" → "bazbar\n"
        let input = "foobar\rbaz\r\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "bazbar\n");
    }

    #[test]
    fn test_progress_bar_simulation() {
        // Simulates a progress bar that overwrites itself
        let input = "[10%]\r[20%]\r[30%]\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "[30%]\n");
    }

    #[test]
    fn test_multiple_lines() {
        // Multi-line output should be preserved
        let input = "line 1\nline 2\nline 3\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "line 1\nline 2\nline 3\n");
    }

    #[test]
    fn test_mixed_cr_and_normal_lines() {
        // Mix of overwritten and normal lines
        let input = "progress\rprogress\n[100%]\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "progress\n[100%]\n");
    }

    #[test]
    fn test_cr_with_shorter_string() {
        // Overwrite with shorter string leaves trailing chars
        let input = "longline\rxx\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "xxngline\n");
    }

    #[test]
    fn test_trailing_cr_no_newline() {
        // Carriage return at end without newline
        let input = "foobar\r";
        let result = apply_terminal_render(input);
        assert_eq!(result, "foobar");
    }

    #[test]
    fn test_empty_string() {
        let input = "";
        let result = apply_terminal_render(input);
        assert_eq!(result, "");
    }

    #[test]
    fn test_only_newlines() {
        let input = "\n\n\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "\n\n\n");
    }

    #[test]
    fn test_only_carriage_returns() {
        // Multiple carriage returns on same line just move cursor
        let input = "hello\r\r\r";
        let result = apply_terminal_render(input);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_carriage_return_between_words() {
        // "hello world" but "world" overwrites "hello"
        let input = "hello\rworld\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "world\n");
    }

    #[test]
    fn test_unicode_characters() {
        // Unicode should work correctly
        let input = "hello 🎉\rhello 👋\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "hello 👋\n");
    }

    #[test]
    fn test_spinner_simulation() {
        // Simulate a spinner: | / - \
        let input = "working |\rworking /\rworking -\rworking \\\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "working \\\n");
    }

    #[test]
    fn test_multiple_overwrites_same_position() {
        // Partial overwrites at different positions
        let input = "abcdef\rXY\rZ\n";
        let result = apply_terminal_render(input);
        // "abcdef" → \r (cursor at 0) → "XYcdef" → \r (cursor at 0) → "ZYcdef"
        assert_eq!(result, "ZYcdef\n");
    }

    #[test]
    fn test_preserves_normal_output() {
        // Ensure normal output without \r is unchanged
        let input = "Hello, World!\nThis is a test.\n";
        let result = apply_terminal_render(input);
        assert_eq!(result, "Hello, World!\nThis is a test.\n");
    }

    #[test]
    fn test_cr_only_no_lf() {
        // Just carriage return, no line feed at end
        let input = "abc\rde";
        let result = apply_terminal_render(input);
        assert_eq!(result, "dec"); // "abc" → \r → "de" overwrites positions 0-1 → "dec"
    }
}
