//! Markdown → ratatui `Line` renderer for the chat log.
//!
//! The public API is a single function:
//!
//! ```ignore
//! pub fn render(text: &str, width: usize) -> Vec<Line<'static>>
//! ```
//!
//! It converts a markdown string to terminal-styled lines using pulldown-cmark
//! for parsing and ratatui spans for styling.  The output is always owned
//! (`Line<'static>`) so it can be cached without lifetime concerns.
//!
//! Supported markdown elements:
//! - Paragraphs (text-wrapped)
//! - **Bold**, *italic*, `inline code`
//! - `# Headings` (bold + underlined)
//! - ``` ```code blocks``` ``` (coloured, 2-space indent)
//! - `> Blockquotes` (│ prefix, dim)
//! - Unordered (`•`) and ordered (`N.`) lists
//! - GFM tables (compacttable style with coloured header)
//!
//! Unknown or raw HTML elements are silently ignored.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

// ── Colour palette ─────────────────────────────────────────────────────────────

/// Foreground colour for inline code and code block text.
const CODE_FG: Color = Color::Rgb(210, 160, 100);

/// Table header foreground.
const TABLE_HEADER_FG: Color = Color::Rgb(200, 210, 240);

/// Table header background.
const TABLE_HEADER_BG: Color = Color::Rgb(30, 40, 60);

/// Even data row background.
const TABLE_EVEN_BG: Color = Color::Rgb(22, 26, 34);

/// Odd data row background.
const TABLE_ODD_BG: Color = Color::Rgb(30, 35, 45);

/// Data row foreground.
const TABLE_DATA_FG: Color = Color::Rgb(210, 215, 225);

/// Column separator character foreground (black — the cell bg shows instead).
const TABLE_SEP_FG: Color = Color::Rgb(0, 0, 0);

// ── Span accumulator for inline content ───────────────────────────────────────

/// Accumulated inline style flags while walking inline events.
#[derive(Default, Clone)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    code: bool,
}

impl InlineStyle {
    fn to_ratatui_style(&self) -> Style {
        let mut s = Style::default();
        if self.code {
            s = s.fg(CODE_FG);
        }
        if self.bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        s
    }
}

// ── Text-wrapping helper ────────────────────────────────────────────────────────

/// Wrap a list of `(text, style)` spans to `width`, producing ratatui `Line`s.
///
/// Inline spans within a word are never split; the wrapping happens between
/// words.  If `prefix` is non-empty it is prepended (with default style) on
/// the first output line only (subsequent wrapped lines get `indent` spaces).
fn wrap_spans(
    spans: &[(String, Style)],
    width: usize,
    prefix: &str,
    indent: &str,
) -> Vec<Line<'static>> {
    if width == 0 {
        // Degenerate: just dump everything on one line.
        let mut out_spans: Vec<Span<'static>> = Vec::new();
        if !prefix.is_empty() {
            out_spans.push(Span::raw(prefix.to_string()));
        }
        for (t, s) in spans {
            out_spans.push(Span::styled(t.clone(), *s));
        }
        return vec![Line::from(out_spans)];
    }

    // Tokenise: break each span into words, keeping track of the original style.
    // A "token" is either whitespace or a word fragment.
    struct Token {
        text: String,
        style: Style,
        is_space: bool,
    }

    let mut tokens: Vec<Token> = Vec::new();
    for (text, style) in spans {
        let mut rest: &str = text.as_str();
        while !rest.is_empty() {
            if rest.starts_with(|c: char| c.is_whitespace()) {
                let end = rest
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(rest.len());
                tokens.push(Token {
                    text: rest[..end].to_string(),
                    style: *style,
                    is_space: true,
                });
                rest = &rest[end..];
            } else {
                let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
                tokens.push(Token {
                    text: rest[..end].to_string(),
                    style: *style,
                    is_space: false,
                });
                rest = &rest[end..];
            }
        }
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut first_line = true;

    let effective_prefix = |first: bool| -> &str { if first { prefix } else { indent } };

    let pfx = effective_prefix(true);
    let pfx_width = pfx.width();
    if !pfx.is_empty() {
        current_spans.push(Span::raw(pfx.to_string()));
    }
    let mut current_width = pfx_width;

    let flush_line = |spans: Vec<Span<'static>>, lines: &mut Vec<Line<'static>>| {
        lines.push(Line::from(spans));
    };

    for token in &tokens {
        if token.is_space {
            // Spaces are only added if there is already content on the line,
            // and only if they fit (lazy: treat a single space as 1 column).
            if current_width > pfx_width && current_width < width {
                current_spans.push(Span::styled(" ".to_string(), token.style));
                current_width += 1;
            }
            continue;
        }
        let tw = token.text.width();
        if current_width == pfx_width {
            // First word on this line — always fits (even if it overflows,
            // there's nothing we can do about a word wider than the terminal).
            current_spans.push(Span::styled(token.text.clone(), token.style));
            current_width += tw;
        } else if current_width + tw <= width {
            current_spans.push(Span::styled(token.text.clone(), token.style));
            current_width += tw;
        } else {
            // Need a new line.
            flush_line(current_spans, &mut lines);
            first_line = false;
            let ind = effective_prefix(first_line);
            let ind_width = ind.width();
            current_spans = Vec::new();
            if !ind.is_empty() {
                current_spans.push(Span::raw(ind.to_string()));
            }
            current_width = ind_width;
            current_spans.push(Span::styled(token.text.clone(), token.style));
            current_width += tw;
        }
    }
    // Flush remaining content.
    if !current_spans.is_empty()
        || lines.is_empty()
        || current_width > effective_prefix(first_line).width()
    {
        flush_line(current_spans, &mut lines);
    }
    lines
}

// ── Table builder ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct TableBuilder {
    /// All rows: first row is the header.
    rows: Vec<Vec<String>>,
    /// Current row being accumulated.
    current_row: Vec<String>,
    /// Current cell text being accumulated.
    current_cell: String,
    in_header: bool,
}

impl TableBuilder {
    fn start_row(&mut self) {
        self.current_row = Vec::new();
    }

    fn end_row(&mut self) {
        let row = std::mem::take(&mut self.current_row);
        self.rows.push(row);
    }

    fn start_cell(&mut self) {
        self.current_cell = String::new();
    }

    fn end_cell(&mut self) {
        let cell = std::mem::take(&mut self.current_cell);
        self.current_row.push(cell.trim().to_string());
    }

    fn push_text(&mut self, text: &str) {
        self.current_cell.push_str(text);
    }

    /// Render the accumulated table to ratatui `Line`s (compacttable style).
    fn render(&self) -> Vec<Line<'static>> {
        if self.rows.is_empty() {
            return Vec::new();
        }

        let ncols = self.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if ncols == 0 {
            return Vec::new();
        }

        // Compute per-column widths.
        let mut col_widths: Vec<usize> = vec![1usize; ncols];
        for row in &self.rows {
            for (ci, cell) in row.iter().enumerate() {
                col_widths[ci] = col_widths[ci].max(cell.width());
            }
        }

        let mut lines: Vec<Line<'static>> = Vec::new();

        for (ri, row) in self.rows.iter().enumerate() {
            let is_header = ri == 0;
            let row_bg = if is_header {
                TABLE_HEADER_BG
            } else if ri % 2 == 0 {
                TABLE_EVEN_BG
            } else {
                TABLE_ODD_BG
            };
            let row_fg = if is_header {
                TABLE_HEADER_FG
            } else {
                TABLE_DATA_FG
            };
            let cell_style = if is_header {
                Style::default()
                    .fg(row_fg)
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(row_fg).bg(row_bg)
            };
            let sep_style = Style::default().fg(TABLE_SEP_FG).bg(row_bg);

            let mut spans: Vec<Span<'static>> = Vec::new();
            for (ci, width) in col_widths.iter().enumerate() {
                if ci > 0 {
                    spans.push(Span::styled("│".to_string(), sep_style));
                }
                let text = row.get(ci).map(|s| s.as_str()).unwrap_or("");
                let text_w = text.width();
                let padding = width.saturating_sub(text_w);
                let padded = format!("{}{}", text, " ".repeat(padding));
                spans.push(Span::styled(padded, cell_style));
            }
            lines.push(Line::from(spans));
        }

        lines
    }
}

// ── Main render function ────────────────────────────────────────────────────────

/// Convert `text` (markdown) to a list of ratatui `Line<'static>` styled for
/// the terminal chat log.
///
/// `width` is the usable column count for text wrapping.  Pass 0 to disable
/// wrapping.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(text, options);

    let mut out: Vec<Line<'static>> = Vec::new();

    // Inline span accumulator: (text, style)
    let mut inline_spans: Vec<(String, Style)> = Vec::new();

    // Current inline style stack.
    let mut style = InlineStyle::default();

    // Are we inside a code block?
    let mut in_code_block = false;
    // Are we inside a blockquote?
    let mut in_blockquote = false;
    // List depth (for nested lists) and ordered-list item counter.
    let mut list_stack: Vec<Option<u64>> = Vec::new(); // None = unordered, Some(n) = ordered starting at n
    // Current list item number when ordered.
    let mut list_item_counters: Vec<u64> = Vec::new();
    // Are we collecting the content of a list item (first paragraph)?
    let mut in_list_item = false;

    // Table state.
    let mut in_table = false;
    let mut table: TableBuilder = TableBuilder::default();

    /// Flush accumulated inline spans as wrapped lines and clear the buffer.
    /// The `prefix` is prepended to the first output line (subsequent wrapped
    /// lines get `indent`-width leading spaces).
    macro_rules! flush_inline {
        ($prefix:expr) => {{
            let prefix: &str = $prefix;
            let indent = " ".repeat(prefix.width());
            if !inline_spans.is_empty() || in_list_item {
                let wrapped = wrap_spans(&inline_spans, width, prefix, &indent);
                out.extend(wrapped);
                inline_spans.clear();
            }
        }};
    }

    for event in parser {
        match event {
            // ── Block-level open tags ─────────────────────────────────────────
            Event::Start(Tag::Paragraph) => {
                // Nothing to do at the start; collect inline content.
            }
            Event::End(TagEnd::Paragraph) => {
                let prefix = if in_blockquote { "│ " } else { "" };
                flush_inline!(prefix);
                // Blank line after paragraph (skip inside list items).
                if !in_list_item {
                    out.push(Line::default());
                }
            }

            Event::Start(Tag::Heading { level, .. }) => {
                style.bold = true;
                // Inject the ATX marker as a visual depth cue.
                let marker = match level {
                    HeadingLevel::H1 => "# ",
                    HeadingLevel::H2 => "## ",
                    HeadingLevel::H3 => "### ",
                    _ => "#### ",
                };
                inline_spans.push((
                    marker.to_string(),
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::UNDERLINED),
                ));
            }
            Event::End(TagEnd::Heading(_)) => {
                // Render heading spans as a single non-wrapped line.
                let heading_style = Style::default()
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::UNDERLINED);
                let line_spans: Vec<Span<'static>> = inline_spans
                    .drain(..)
                    .map(|(t, s)| Span::styled(t, s.patch(heading_style)))
                    .collect();
                out.push(Line::from(line_spans));
                out.push(Line::default());
                style = InlineStyle::default();
            }

            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                out.push(Line::default());
            }

            Event::Start(Tag::BlockQuote(_)) => {
                in_blockquote = true;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                in_blockquote = false;
                out.push(Line::default());
            }

            Event::Start(Tag::List(start)) => {
                list_stack.push(start);
                list_item_counters.push(start.unwrap_or(1));
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
                list_item_counters.pop();
                if list_stack.is_empty() {
                    out.push(Line::default());
                }
            }

            Event::Start(Tag::Item) => {
                in_list_item = true;
                inline_spans.clear();
            }
            Event::End(TagEnd::Item) => {
                let prefix = if let Some(start) = list_stack.last().and_then(|s| *s) {
                    let n = list_item_counters.last_mut().unwrap();
                    let p = format!("{}. ", *n - start + 1);
                    *n += 1;
                    p
                } else {
                    "• ".to_string()
                };
                flush_inline!(&prefix);
                in_list_item = false;
            }

            // ── Table tags ────────────────────────────────────────────────────
            Event::Start(Tag::Table(_)) => {
                in_table = true;
                table = TableBuilder::default();
            }
            Event::End(TagEnd::Table) => {
                in_table = false;
                out.extend(table.render());
                out.push(Line::default());
                table = TableBuilder::default();
            }
            Event::Start(Tag::TableHead) => {
                table.in_header = true;
                table.start_row();
            }
            Event::End(TagEnd::TableHead) => {
                table.end_row();
                table.in_header = false;
            }
            Event::Start(Tag::TableRow) => {
                table.start_row();
            }
            Event::End(TagEnd::TableRow) => {
                table.end_row();
            }
            Event::Start(Tag::TableCell) => {
                table.start_cell();
            }
            Event::End(TagEnd::TableCell) => {
                table.end_cell();
            }

            // ── Inline style tags ─────────────────────────────────────────────
            Event::Start(Tag::Strong) => {
                style.bold = true;
            }
            Event::End(TagEnd::Strong) => {
                style.bold = false;
            }

            Event::Start(Tag::Emphasis) => {
                style.italic = true;
            }
            Event::End(TagEnd::Emphasis) => {
                style.italic = false;
            }

            Event::Start(Tag::Link { .. }) | Event::End(TagEnd::Link) => {
                // Render link text inline without any special styling.
            }

            Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => {
                // Ignore images.
            }

            // ── Leaf events ───────────────────────────────────────────────────
            Event::Text(t) => {
                let text = t.into_string();
                if in_code_block {
                    // Each text event inside a code block is one line (pulldown-cmark
                    // includes the newline).  Render with 2-space indent.
                    let code_style = Style::default().fg(CODE_FG);
                    for line in text.split('\n') {
                        if line.is_empty() {
                            // Preserve blank lines inside code blocks.
                            out.push(Line::from(Span::raw("")));
                            continue;
                        }
                        let indented = format!("  {line}");
                        out.push(Line::from(Span::styled(indented, code_style)));
                    }
                } else if in_table {
                    table.push_text(&text);
                } else {
                    inline_spans.push((text, style.to_ratatui_style()));
                }
            }

            Event::Code(t) => {
                // Inline code.
                let text = t.into_string();
                let s = Style::default().fg(CODE_FG);
                if in_table {
                    table.push_text(&text);
                } else {
                    inline_spans.push((text, s));
                }
            }

            Event::SoftBreak => {
                if !in_table && !in_code_block {
                    inline_spans.push((" ".to_string(), Style::default()));
                }
            }

            Event::HardBreak => {
                if !in_table && !in_code_block {
                    let prefix = if in_blockquote { "│ " } else { "" };
                    flush_inline!(prefix);
                }
            }

            Event::Rule => {
                // Horizontal rule: render as a row of dashes.
                let rule: String = "─".repeat(width.min(80));
                out.push(Line::from(Span::styled(
                    rule,
                    Style::default().add_modifier(Modifier::DIM),
                )));
                out.push(Line::default());
            }

            // Everything else (HTML, footnotes, …) is ignored.
            _ => {}
        }
    }

    // Flush any remaining inline content (e.g. a paragraph without a closing tag
    // in partial/streaming input).
    if !inline_spans.is_empty() {
        let wrapped = wrap_spans(&inline_spans, width, "", "");
        out.extend(wrapped);
    }

    // Remove a trailing blank line if present — keeps output compact.
    if out.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
        out.pop();
    }

    out
}

// ── Tests ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn lines_text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(line_text).collect()
    }

    // ── Plain text ──────────────────────────────────────────────────────────────

    #[test]
    fn plain_text_renders_as_single_line() {
        let lines = render("hello", 80);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(line_text(&lines[0]), "hello");
    }

    #[test]
    fn plain_text_roundtrip_no_markup() {
        // A message that contains no markdown should come back as-is.
        let lines = render("💬 hello", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "💬 hello");
    }

    // ── Bold / italic / inline code ─────────────────────────────────────────────

    #[test]
    fn bold_text_has_bold_modifier() {
        let lines = render("**bold**", 80);
        assert!(!lines.is_empty());
        let spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        let bold_span = spans.iter().find(|s| s.content.contains("bold")).unwrap();
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn italic_text_has_italic_modifier() {
        let lines = render("*italic*", 80);
        assert!(!lines.is_empty());
        let spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        let italic_span = spans.iter().find(|s| s.content.contains("italic")).unwrap();
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn inline_code_has_code_color() {
        let lines = render("`code`", 80);
        assert!(!lines.is_empty());
        let spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        let code_span = spans.iter().find(|s| s.content.contains("code")).unwrap();
        assert_eq!(code_span.style.fg, Some(CODE_FG));
    }

    // ── Headings ────────────────────────────────────────────────────────────────

    #[test]
    fn heading_has_bold_and_underline() {
        let lines = render("# Title", 80);
        assert!(!lines.is_empty());
        // Find a span that contains "Title"
        let spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        let title_span = spans.iter().find(|s| s.content.contains("Title")).unwrap();
        assert!(title_span.style.add_modifier.contains(Modifier::BOLD));
        assert!(title_span.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    // ── Code block ──────────────────────────────────────────────────────────────

    #[test]
    fn code_block_lines_have_code_color() {
        let md = "```\nfn main() {}\n```";
        let lines = render(md, 80);
        assert!(!lines.is_empty());
        let code_line = lines
            .iter()
            .find(|l| line_text(l).contains("fn main"))
            .expect("code line should be present");
        let code_span = code_line
            .spans
            .iter()
            .find(|s| s.content.contains("fn main"))
            .unwrap();
        assert_eq!(code_span.style.fg, Some(CODE_FG));
    }

    #[test]
    fn code_block_has_two_space_indent() {
        let md = "```\nhello\n```";
        let lines = render(md, 80);
        let code_line = lines
            .iter()
            .find(|l| line_text(l).contains("hello"))
            .expect("code line should be present");
        assert!(
            line_text(code_line).starts_with("  "),
            "expected 2-space indent: {:?}",
            line_text(code_line)
        );
    }

    // ── Table ───────────────────────────────────────────────────────────────────

    #[test]
    fn table_produces_correct_line_count() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
        let lines = render(md, 80);
        // Header + 2 data rows = 3 table lines.
        // The trailing blank line is stripped by the renderer's end-cleanup.
        assert_eq!(lines.len(), 3, "{:?}", lines_text(&lines));
    }

    #[test]
    fn table_header_has_correct_background_color() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let lines = render(md, 80);
        // First line is the header.
        let header_line = &lines[0];
        assert!(!header_line.spans.is_empty());
        let first_span = &header_line.spans[0];
        assert_eq!(
            first_span.style.bg,
            Some(TABLE_HEADER_BG),
            "header bg mismatch: {:?}",
            first_span.style
        );
    }

    // ── Partial / streaming markdown ─────────────────────────────────────────────

    #[test]
    fn partial_markdown_does_not_panic() {
        // Incomplete fence — must not panic.
        let md = "```rust\nfn main() {";
        let _lines = render(md, 80);
    }

    #[test]
    fn partial_table_does_not_panic() {
        // Incomplete table row — must not panic.
        let md = "| col1 | col2";
        let _lines = render(md, 80);
    }

    #[test]
    fn empty_input_returns_empty() {
        let lines = render("", 80);
        assert!(lines.is_empty(), "{lines:?}");
    }
}
