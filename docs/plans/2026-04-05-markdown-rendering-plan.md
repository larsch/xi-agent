# Plan: Markdown rendering in terminal UI

**Date:** 2026-04-05  
**Status:** Implemented and accepted

---

## Goal

Render markdown in assistant message blocks in the ratatui terminal UI, with streaming-compatible incremental rendering and compact table formatting styled after `~/prj/compacttable`.

---

## Scope

- New file: `src/markdown.rs`
- Modified: `src/ui.rs` (wire in renderer for assistant messages)
- Modified: `src/main.rs` (add `mod markdown;`)
- No new dependencies (uses `pulldown-cmark` already in `Cargo.toml`)

---

## Approach

### Parser

Use `pulldown-cmark` (already present, used in `export.rs`).  Enable `ENABLE_TABLES` option for GFM table support.  The event-streaming API maps naturally to building `Vec<Line<'static>>`.

`markdown-rs` was considered and rejected as overkill for this use case.

### Public API (`src/markdown.rs`)

```rust
pub fn render(text: &str, width: usize) -> Vec<Line<'static>>
```

### Element rendering

| Markdown element | Terminal rendering |
|---|---|
| Paragraph | Wrapped plain text; blank line after |
| **bold** | `BOLD` modifier only |
| *italic* | `ITALIC` modifier only |
| `inline code` | `Rgb(180, 220, 140)` fg |
| ` ```code block``` ` | `Rgb(180, 220, 140)` fg, 2-space indent per line |
| `# Heading` | `BOLD` + `UNDERLINED` |
| `> blockquote` | `│ ` prefix, `DIM` modifier |
| `- / 1.` list items | `• ` / `N. ` prefix |
| GFM table | compacttable style (see below) |
| Raw HTML / unknown | Stripped / ignored |

### Table rendering (compacttable style)

Tables are buffered (all rows collected before rendering) since column widths require a full pass.

| Part | Style |
|---|---|
| Header row bg | `Rgb(30, 40, 60)` |
| Header row fg | `Rgb(200, 210, 240)`, `BOLD` |
| Even data rows bg | `Rgb(22, 26, 34)` |
| Odd data rows bg | `Rgb(30, 35, 45)` |
| Data row fg | `Rgb(210, 215, 225)` |
| Column separator | `│` with `Rgb(0, 0, 0)` fg, cell background behind it |
| No padding | Compact — matches the Python reference |

Blank line appended after the table.

**Table text wrapping** (added post-initial implementation):

When the natural table width exceeds the available terminal width the renderer shrinks the widest column(s) iteratively until the table fits (or all columns are at minimum width 1).  Cell text wider than its allotted column is wrapped:

- Word-wrap (break at whitespace) is tried first.
- If any non-final line has < 70% fill (> 30% trailing whitespace), hard-wrap at exactly `col_width` columns is used instead.
- All cells in a logical row are padded to the same height (blank padding lines for shorter cells).

### Streaming / partial markdown

`pulldown-cmark` handles partial input gracefully — incomplete fences and tables degrade to plain text.  No special streaming logic needed beyond the existing `build_log_lines_cached` invalidation.

### Integration point (`src/ui.rs`)

In `build_log_lines()`, `Role::Assistant` branch:
- Replace `sanitize_for_display(&msg.content)` + `append_message(...)` with `markdown::render(&msg.content, width)` and extend the lines vec directly.
- Streaming cursor `▋`: appended as a trailing span on the last rendered line.
- Thinking block unchanged (plain dim text, not markdown).

---

## Verification approach

- All existing `build_log_lines` tests must pass (plain text renders as a single paragraph — same output for single-line messages).
- New unit tests in `src/markdown.rs`:
  - Bold / italic / inline-code spans produce correct modifiers and colors
  - Heading produces bold + underline
  - Table produces correct line count and header background color
  - Partial / streaming markdown does not panic
  - Plain text round-trips cleanly

- `cargo test --all-features`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo fmt --all -- --check`

---

## Risks

- Existing test assertions on `"💬 hello"` — plain text through pulldown-cmark renders as a paragraph with the same content and no trailing blank line for a single-paragraph message.  Verified this is safe.
- `Line<'static>` requirement — all string content must be owned; `CowStr` from pulldown-cmark must be converted via `.into_string()` / `.to_string()`.
