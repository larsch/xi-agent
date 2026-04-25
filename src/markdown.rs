//! Markdown → ratatui `Line` renderer for the chat log.
//!
//! The public API is a single function:
//!
//! ```ignore
//! pub fn render(text: &str, width: usize, prefix: &str) -> Vec<Line<'static>>
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
use unicode_width::UnicodeWidthChar;
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

    /// Wrap `text` to fit within `col_width` columns.
    ///
    /// Tries word-wrapping first.  If any word-wrapped line has a fill ratio
    /// below 70 % (i.e. more than 30 % trailing whitespace), falls back to
    /// hard-wrapping at exactly `col_width` characters.
    fn wrap_cell(text: &str, col_width: usize) -> Vec<String> {
        if col_width == 0 {
            return vec![text.to_string()];
        }
        if text.width() <= col_width {
            return vec![text.to_string()];
        }

        // ── Word-wrap attempt ──────────────────────────────────────────────
        let word_wrapped = Self::word_wrap(text, col_width);

        // Check fill ratio: if any line (except the last) is below 70% full,
        // fall back to hard-wrap.
        let threshold = (col_width as f64 * 0.70) as usize;
        let poorly_filled = word_wrapped
            .iter()
            .rev()
            .skip(1) // skip last line — it's naturally short
            .any(|line| line.width() < threshold);

        if poorly_filled {
            Self::hard_wrap(text, col_width)
        } else {
            word_wrapped
        }
    }

    /// Greedy word-wrap: break at whitespace boundaries.
    fn word_wrap(text: &str, col_width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut current_w: usize = 0;

        for word in text.split_whitespace() {
            let word_w = word.width();
            if current.is_empty() {
                // First word on this line — always place it (even if it overflows).
                current.push_str(word);
                current_w = word_w;
            } else if current_w + 1 + word_w <= col_width {
                current.push(' ');
                current.push_str(word);
                current_w += 1 + word_w;
            } else {
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
                current_w = word_w;
            }
        }
        if !current.is_empty() || lines.is_empty() {
            lines.push(current);
        }
        lines
    }

    /// Hard-wrap: break at exactly `col_width` columns.
    fn hard_wrap(text: &str, col_width: usize) -> Vec<String> {
        if col_width == 0 {
            return vec![text.to_string()];
        }
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut current_w: usize = 0;

        for ch in text.chars() {
            let cw = ch.width().unwrap_or(0);
            if current_w + cw > col_width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_w = 0;
            }
            current.push(ch);
            current_w += cw;
        }
        if !current.is_empty() || lines.is_empty() {
            lines.push(current);
        }
        lines
    }

    /// Render the accumulated table to ratatui `Line`s (compacttable style).
    ///
    /// `available_width` is the usable terminal column count.  When the
    /// natural table width exceeds it the widest column(s) are shrunk
    /// (iteratively, largest-first) until the table fits or every column has
    /// reached its minimum width of 1.  Cells that are wider than their
    /// allotted column are wrapped to multiple terminal lines; all cells in
    /// the same logical row are padded to the same height.
    fn render(&self, available_width: usize) -> Vec<Line<'static>> {
        if self.rows.is_empty() {
            return Vec::new();
        }

        let ncols = self.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if ncols == 0 {
            return Vec::new();
        }

        // ── Step 1: compute natural per-column widths ──────────────────────
        let mut col_widths: Vec<usize> = vec![1usize; ncols];
        for row in &self.rows {
            for (ci, cell) in row.iter().enumerate() {
                col_widths[ci] = col_widths[ci].max(cell.width());
            }
        }

        // ── Step 2: shrink columns to fit available_width ──────────────────
        // Total width = sum(col_widths) + (ncols - 1) separator columns.
        if available_width > 0 {
            let separators = ncols.saturating_sub(1);
            // Each '│' is 3 bytes but 1 column wide.
            let sep_width = separators; // 1 column each
            let content_budget = available_width.saturating_sub(sep_width);

            let total: usize = col_widths.iter().sum();
            if total > content_budget {
                // Iteratively shrink the widest column.
                // For efficiency, compute the amount to shed in bulk.
                // We allow each column to shrink down to 1.
                let budget = content_budget;
                loop {
                    let total_now: usize = col_widths.iter().sum();
                    if total_now <= budget {
                        break;
                    }
                    // Find the widest column (if tie, leftmost).
                    let max_w = *col_widths.iter().max().unwrap();
                    if max_w <= 1 {
                        break; // Cannot shrink further.
                    }
                    // Find the second-widest (or 1 if all equal).
                    let second = col_widths
                        .iter()
                        .filter(|&&w| w < max_w)
                        .copied()
                        .max()
                        .unwrap_or(1);
                    // How many columns share the maximum?
                    let count = col_widths.iter().filter(|&&w| w == max_w).count();
                    // How much can we shed from each of these `count` columns
                    // before they hit `second` (or 1)?
                    let target_w = second.max(1);
                    let shed_per_col = max_w - target_w; // ≥ 1
                    let total_shedable = shed_per_col * count;
                    let deficit = total_now - budget;
                    if total_shedable <= deficit {
                        // Shrink all max-width columns to `target_w`.
                        for w in col_widths.iter_mut() {
                            if *w == max_w {
                                *w = target_w;
                            }
                        }
                    } else {
                        // Only need to shed `deficit` columns worth; shed
                        // one column at a time from the front.
                        let mut remaining = deficit;
                        for w in col_widths.iter_mut() {
                            if remaining == 0 {
                                break;
                            }
                            if *w == max_w {
                                *w -= 1;
                                remaining -= 1;
                            }
                        }
                    }
                }
            }
        }

        // ── Step 3: render rows, wrapping cell text where needed ───────────
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

            // Wrap each cell.
            let wrapped_cells: Vec<Vec<String>> = col_widths
                .iter()
                .enumerate()
                .map(|(ci, &cw)| {
                    let text = row.get(ci).map(|s| s.as_str()).unwrap_or("");
                    Self::wrap_cell(text, cw)
                })
                .collect();

            let row_height = wrapped_cells.iter().map(|wc| wc.len()).max().unwrap_or(1);

            for line_idx in 0..row_height {
                let mut spans: Vec<Span<'static>> = Vec::new();
                for (ci, col_w) in col_widths.iter().enumerate() {
                    if ci > 0 {
                        spans.push(Span::styled("│".to_string(), sep_style));
                    }
                    let text = wrapped_cells[ci]
                        .get(line_idx)
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let text_w = text.width();
                    let padding = col_w.saturating_sub(text_w);
                    let padded = format!("{}{}", text, " ".repeat(padding));
                    spans.push(Span::styled(padded, cell_style));
                }
                lines.push(Line::from(spans));
            }
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
///
/// `prefix` is an icon string (e.g. `"💬 "`) that is prepended to the very
/// first rendered line.  When the first block is a table the prefix is emitted
/// on its own line so that table column alignment is not disturbed.  Pass `""`
/// when no prefix is needed.
pub fn render(text: &str, width: usize, prefix: &str) -> Vec<Line<'static>> {
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
    // Are we collecting the content of a list item?
    let mut in_list_item = false;
    // True after the first paragraph of a list item has been flushed,
    // so subsequent paragraphs know to use an indent prefix instead of
    // the bullet/number prefix.
    let mut list_item_first_para_done = false;

    // Compute the indentation string contributed by all ancestor list levels
    // (i.e. everything in `list_stack` / `list_item_counters` *except* the
    // innermost entry, which is the current item's own level).
    //
    // Each ancestor level contributes spaces equal to its bullet/number
    // prefix width so that nested items align under their parent's text.
    let list_ancestor_indent = |list_stack: &[Option<u64>], list_item_counters: &[u64]| -> String {
        // All levels except the last are ancestors.
        let depth = list_stack.len().saturating_sub(1);
        let mut indent = String::new();
        for (list_kind, &counter) in list_stack.iter().zip(list_item_counters.iter()).take(depth) {
            let prefix_width = match list_kind {
                None => 2usize, // "• " = 2 columns
                Some(_) => {
                    // counter holds the next visible item number at this
                    // level, so subtract 1 to get the current parent number.
                    let item_num = counter.saturating_sub(1).max(1);
                    format!("{}. ", item_num).len()
                }
            };
            for _ in 0..prefix_width {
                indent.push(' ');
            }
        }
        indent
    };

    let current_ordered_item_prefix =
        |ancestor_indent: &str, list_stack: &[Option<u64>], list_item_counters: &[u64]| -> String {
            if list_stack.last().and_then(|s| *s).is_some() {
                let n = list_item_counters.last().copied().unwrap_or(1);
                format!("{}{}. ", ancestor_indent, n)
            } else {
                format!("{}• ", ancestor_indent)
            }
        };

    // Table state.
    let mut in_table = false;
    let mut table: TableBuilder = TableBuilder::default();

    // True until the first line of the first block has been emitted.
    // Used to attach `prefix` to the correct output line.
    let mut first_block = !prefix.is_empty();

    /// Flush accumulated inline spans as wrapped lines and clear the buffer.
    /// On the very first flush while `first_block` is true the answer-icon
    /// prefix is prepended to line 0; all subsequent lines use an indent of
    /// equal width so the text remains aligned.
    macro_rules! flush_inline {
        ($block_prefix:expr) => {{
            let block_prefix: &str = $block_prefix;
            let first_plain_paragraph = first_block
                && !prefix.is_empty()
                && block_prefix.is_empty()
                && !in_list_item
                && !in_blockquote;
            // Decide the effective line-0 prefix:
            // • If we haven't emitted anything yet AND caller-supplied `prefix`
            //   is non-empty, prepend it (plus the block's own prefix).
            // • Otherwise use the block's own prefix as-is.
            let effective_prefix: std::borrow::Cow<str> = if first_block && !prefix.is_empty() {
                first_block = false;
                // The prefix width already accounts for the icon.
                let combined = format!("{}{}", prefix, block_prefix);
                std::borrow::Cow::Owned(combined)
            } else {
                std::borrow::Cow::Borrowed(block_prefix)
            };
            let indent = if first_plain_paragraph {
                String::new()
            } else {
                " ".repeat(effective_prefix.width())
            };
            if !inline_spans.is_empty() || in_list_item {
                let wrapped = wrap_spans(&inline_spans, width, &effective_prefix, &indent);
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
                if in_list_item {
                    // Compute the prefix for this item (bullet or number) so we
                    // can use it for the first paragraph, and its width as indent
                    // for subsequent paragraphs.  We don't advance the counter
                    // here — that happens in End(Item).
                    let ancestor_indent = list_ancestor_indent(&list_stack, &list_item_counters);
                    let item_prefix = current_ordered_item_prefix(
                        &ancestor_indent,
                        &list_stack,
                        &list_item_counters,
                    );
                    let p = if list_item_first_para_done {
                        " ".repeat(item_prefix.len())
                    } else {
                        item_prefix
                    };
                    flush_inline!(&p);
                    list_item_first_para_done = true;
                } else {
                    let prefix = if in_blockquote { "│ " } else { "" };
                    flush_inline!(prefix);
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
                let mut line_spans: Vec<Span<'static>> = inline_spans
                    .drain(..)
                    .map(|(t, s)| Span::styled(t, s.patch(heading_style)))
                    .collect();
                // Prepend icon prefix to the first line of the first block.
                if first_block && !prefix.is_empty() {
                    first_block = false;
                    line_spans.insert(0, Span::raw(prefix.to_string()));
                }
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
                // If we are currently accumulating inline content for an outer
                // list item (tight item followed by a nested list), flush that
                // content now before descending into the sublist.
                if in_list_item && !inline_spans.is_empty() {
                    let ancestor_indent = list_ancestor_indent(&list_stack, &list_item_counters);
                    let item_prefix = current_ordered_item_prefix(
                        &ancestor_indent,
                        &list_stack,
                        &list_item_counters,
                    );
                    let p = if list_item_first_para_done {
                        " ".repeat(item_prefix.len())
                    } else {
                        item_prefix
                    };
                    flush_inline!(&p);
                    list_item_first_para_done = true;
                }
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
                list_item_first_para_done = false;
                inline_spans.clear();
            }
            Event::End(TagEnd::Item) => {
                let ancestor_indent = list_ancestor_indent(&list_stack, &list_item_counters);
                let prefix = if list_stack.last().and_then(|s| *s).is_some() {
                    let n = list_item_counters.last_mut().unwrap();
                    let p = format!("{}{}. ", ancestor_indent, *n);
                    *n += 1;
                    p
                } else {
                    format!("{}• ", ancestor_indent)
                };
                // Flush any remaining inline content (tight list items never
                // hit End(Paragraph), so this is the only flush point for them).
                if !inline_spans.is_empty() {
                    let p = if list_item_first_para_done {
                        " ".repeat(prefix.len())
                    } else {
                        prefix
                    };
                    flush_inline!(&p);
                }
                in_list_item = false;
                list_item_first_para_done = false;
            }

            // ── Table tags ────────────────────────────────────────────────────
            Event::Start(Tag::Table(_)) => {
                in_table = true;
                table = TableBuilder::default();
            }
            Event::End(TagEnd::Table) => {
                in_table = false;
                // When the table is the first block and a prefix is set, emit
                // the icon on its own line so the table column alignment is
                // undisturbed.  For non-first blocks render as usual.
                if first_block && !prefix.is_empty() {
                    first_block = false;
                    out.push(Line::from(Span::raw(prefix.to_string())));
                }
                out.extend(table.render(width));
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
                        if first_block && !prefix.is_empty() {
                            first_block = false;
                            out.push(Line::from(vec![
                                Span::raw(prefix.to_string()),
                                Span::styled(indented, code_style),
                            ]));
                        } else {
                            out.push(Line::from(Span::styled(indented, code_style)));
                        }
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

            Event::SoftBreak if !in_table && !in_code_block => {
                inline_spans.push((" ".to_string(), Style::default()));
            }

            Event::HardBreak if !in_table && !in_code_block => {
                let prefix = if in_blockquote { "│ " } else { "" };
                flush_inline!(prefix);
            }

            Event::Rule => {
                // Horizontal rule: render as a row of dashes.
                let rule: String = "─".repeat(width.min(80));
                let mut spans: Vec<Span<'static>> = Vec::new();
                if first_block && !prefix.is_empty() {
                    first_block = false;
                    spans.push(Span::raw(prefix.to_string()));
                }
                spans.push(Span::styled(
                    rule,
                    Style::default().add_modifier(Modifier::DIM),
                ));
                out.push(Line::from(spans));
                out.push(Line::default());
            }

            // Everything else (HTML, footnotes, …) is ignored.
            _ => {}
        }
    }

    // Flush any remaining inline content (e.g. a paragraph without a closing tag
    // in partial/streaming input).
    if !inline_spans.is_empty() {
        let eff_prefix: std::borrow::Cow<str> = if first_block && !prefix.is_empty() {
            std::borrow::Cow::Borrowed(prefix)
        } else {
            std::borrow::Cow::Borrowed("")
        };
        let indent = " ".repeat(eff_prefix.width());
        let wrapped = wrap_spans(&inline_spans, width, &eff_prefix, &indent);
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
        let lines = render("hello", 80, "");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(line_text(&lines[0]), "hello");
    }

    #[test]
    fn plain_text_roundtrip_no_markup() {
        // A message that contains no markdown should come back as-is.
        let lines = render("💬 hello", 80, "");
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "💬 hello");
    }

    #[test]
    fn plain_text_first_paragraph_wraps_to_first_column_after_icon() {
        let lines = render("hello world from tau", 12, "💬 ");
        let texts = lines_text(&lines);
        assert_eq!(texts, vec!["💬 hello ", "world from ", "tau"]);
    }

    // ── Bold / italic / inline code ─────────────────────────────────────────────

    #[test]
    fn bold_text_has_bold_modifier() {
        let lines = render("**bold**", 80, "");
        assert!(!lines.is_empty());
        let spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        let bold_span = spans.iter().find(|s| s.content.contains("bold")).unwrap();
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn italic_text_has_italic_modifier() {
        let lines = render("*italic*", 80, "");
        assert!(!lines.is_empty());
        let spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        let italic_span = spans.iter().find(|s| s.content.contains("italic")).unwrap();
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn inline_code_has_code_color() {
        let lines = render("`code`", 80, "");
        assert!(!lines.is_empty());
        let spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
        let code_span = spans.iter().find(|s| s.content.contains("code")).unwrap();
        assert_eq!(code_span.style.fg, Some(CODE_FG));
    }

    // ── Headings ────────────────────────────────────────────────────────────────

    #[test]
    fn heading_has_bold_and_underline() {
        let lines = render("# Title", 80, "");
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
        let lines = render(md, 80, "");
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
        let lines = render(md, 80, "");
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
    fn list_wrapping_remains_indented_under_prefix() {
        let lines = render("- hello world from tau", 12, "💬 ");
        let texts = lines_text(&lines);
        assert_eq!(
            texts,
            vec!["💬 • hello ", "     world ", "     from ", "     tau"]
        );
    }

    #[test]
    fn table_produces_correct_line_count() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
        let lines = render(md, 80, "");
        // Header + 2 data rows = 3 table lines.
        // The trailing blank line is stripped by the renderer's end-cleanup.
        assert_eq!(lines.len(), 3, "{:?}", lines_text(&lines));
    }

    #[test]
    fn table_header_has_correct_background_color() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let lines = render(md, 80, "");
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

    // ── Table wrapping ──────────────────────────────────────────────────────────

    #[test]
    fn table_wide_cell_wraps_to_multiple_lines() {
        // Two columns, very narrow terminal — each data cell should wrap.
        // Header: "Name" | "Description"
        // Row:    "Alice" | "A very long description that exceeds the column width"
        // We give only 20 columns of width.
        let md = "| Name | Description |\n|------|-------------|\n| Alice | A very long description that exceeds the column width |";
        let lines = render(md, 20, "");
        // Header is 1 line; the data row should produce more than 1 terminal line.
        let total = lines.len();
        assert!(
            total > 2,
            "expected wrapped row to produce > 2 lines, got {total}: {:?}",
            lines_text(&lines)
        );
    }

    #[test]
    fn table_wrapping_aligns_row_heights() {
        // Two columns; only the second is wide.  First column cells are short,
        // second column cells wrap.  All terminal lines for the same row must
        // have the same number of spans structure (one cell per column).
        let md = "| X | Y |\n|---|---|\n| a | word1 word2 word3 word4 word5 word6 word7 word8 |";
        let lines = render(md, 15, "");
        let texts = lines_text(&lines);
        // There should be at least one line where the first cell is blank padding.
        let has_blank_first_cell = texts
            .iter()
            .skip(1) // skip header
            .any(|t| t.starts_with("  ")); // padded blank first cell
        assert!(
            has_blank_first_cell || texts.len() > 2,
            "expected multi-line row: {:?}",
            texts
        );
    }

    #[test]
    fn table_fits_within_available_width() {
        // Build a table where natural width > terminal width.
        // Verify that every rendered line fits within the given width.
        let md = "| Column One | Column Two | Column Three |\n|---|---|---|\n| a long value here | another long value here | yet another long value |";
        let available = 40;
        let lines = render(md, available, "");
        for line in &lines {
            let w: usize = line.spans.iter().map(|s| s.content.width()).sum();
            assert!(
                w <= available,
                "line width {w} exceeds available {available}: {:?}",
                line_text(line)
            );
        }
    }

    #[test]
    fn table_no_wrapping_when_fits() {
        // When the table fits naturally, line count should equal row count.
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
        let lines = render(md, 80, "");
        assert_eq!(lines.len(), 3, "{:?}", lines_text(&lines));
    }

    // ── Partial / streaming markdown ─────────────────────────────────────────────

    #[test]
    fn partial_markdown_does_not_panic() {
        // Incomplete fence — must not panic.
        let md = "```rust\nfn main() {";
        let _lines = render(md, 80, "");
    }

    #[test]
    fn partial_table_does_not_panic() {
        // Incomplete table row — must not panic.
        let md = "| col1 | col2";
        let _lines = render(md, 80, "");
    }

    #[test]
    fn empty_input_returns_empty() {
        let lines = render("", 80, "");
        assert!(lines.is_empty(), "{lines:?}");
    }

    // ── List rendering ──────────────────────────────────────────────────────────

    #[test]
    fn loose_ordered_list_prefixes_each_item_correctly() {
        // Blank lines between items make this a "loose" list — pulldown-cmark
        // wraps each item's text in a Paragraph, which must not flush before
        // the item prefix is prepended.
        let md = "1. First item\n\n2. Second item\n\n3. Third item";
        let texts: Vec<String> = render(md, 80, "").iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec!["1. First item", "2. Second item", "3. Third item"]
        );
    }

    #[test]
    fn separated_ordered_lists_preserve_explicit_start_numbers() {
        let md = "1. First section item\n\nSome intervening paragraph.\n\n2. Second section item\n\n3. Third section item";
        let texts: Vec<String> = render(md, 80, "").iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec![
                "1. First section item",
                "",
                "Some intervening paragraph.",
                "",
                "2. Second section item",
                "3. Third section item",
            ]
        );
    }

    #[test]
    fn loose_unordered_list_prefixes_each_item_correctly() {
        let md = "- Alpha\n\n- Beta\n\n- Gamma";
        let texts: Vec<String> = render(md, 80, "").iter().map(line_text).collect();
        assert_eq!(texts, vec!["• Alpha", "• Beta", "• Gamma"]);
    }

    #[test]
    fn multi_paragraph_list_item_indents_continuation() {
        let md = "1. First paragraph.\n\n   Second paragraph.\n\n2. Item two.";
        let texts: Vec<String> = render(md, 80, "").iter().map(line_text).collect();
        assert_eq!(texts[0], "1. First paragraph.");
        assert_eq!(texts[1], "   Second paragraph.");
        assert_eq!(texts[2], "2. Item two.");
    }

    // ── Nested list indentation ─────────────────────────────────────────────────

    #[test]
    fn nested_unordered_list_indents_child_items() {
        // Tight nested unordered list: children should be indented by the
        // parent bullet width (2 spaces for "• ").
        let md = "- Item 1\n  - Nested 1\n  - Nested 2\n- Item 2";
        let texts: Vec<String> = render(md, 80, "").iter().map(line_text).collect();
        assert_eq!(texts[0], "• Item 1");
        assert_eq!(texts[1], "  • Nested 1");
        assert_eq!(texts[2], "  • Nested 2");
        assert_eq!(texts[3], "• Item 2");
    }

    #[test]
    fn nested_ordered_list_indents_child_items() {
        // Tight nested ordered list: children should be indented by the parent
        // number prefix width (3 spaces for "1. ").
        let md = "1. First\n   1. Nested first\n   2. Nested second\n2. Second";
        let texts: Vec<String> = render(md, 80, "").iter().map(line_text).collect();
        assert_eq!(texts[0], "1. First");
        assert_eq!(texts[1], "   1. Nested first");
        assert_eq!(texts[2], "   2. Nested second");
        assert_eq!(texts[3], "2. Second");
    }

    #[test]
    fn nested_mixed_list_indents_correctly() {
        // Ordered outer, unordered inner.
        let md = "1. Outer\n   - Inner A\n   - Inner B\n2. Outer 2";
        let texts: Vec<String> = render(md, 80, "").iter().map(line_text).collect();
        assert_eq!(texts[0], "1. Outer");
        assert_eq!(texts[1], "   • Inner A");
        assert_eq!(texts[2], "   • Inner B");
        assert_eq!(texts[3], "2. Outer 2");
    }
}
