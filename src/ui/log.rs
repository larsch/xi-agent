use std::collections::HashSet;
use std::sync::Arc;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    agent_turn_state::VisualUpdate,
    llm::{AssistantPhase, Message, Role},
    mouse_select::LineSource,
    theme::Theme,
    tool_presentation,
};

use crate::config::DisplayConfig;

use super::input::{normalize_terminal_segment, wrap_str};

// ── ToolBodyConfig ────────────────────────────────────────────────────────────

/// Display configuration for tool body rendering.
///
/// All line-count limits apply to the visible window; when a body exceeds
/// the limit the overflow is replaced by a `… N total lines` marker.
/// Setting `full_output = true` disables all limits.
#[derive(Debug, Clone)]
pub struct ToolBodyConfig {
    /// Show untruncated output for all tools.
    pub full_output: bool,
    /// Max lines shown for head-truncated bodies (read_file, write_file, find_files).
    pub head_lines: usize,
    /// Max lines shown for tail-truncated bodies (bash, exec, custom).
    pub tail_lines: usize,
    /// Max lines per side for edit_file diff body.
    pub diff_lines: usize,
}

impl Default for ToolBodyConfig {
    fn default() -> Self {
        Self {
            full_output: false,
            head_lines: 8,
            tail_lines: 8,
            diff_lines: 4,
        }
    }
}

// ── Logical layout ────────────────────────────────────────────────────────────

/// Stable classification of a renderable subsection of the conversation log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogBlockKind {
    AssistantThinking,
    AssistantMarkdown,
    UserContent,
    ToolIntent,
    ToolBody,
    Diff,
    AskUserContext,
    AskUserQuestion,
    AskUserResponse,
}

/// Direction and limit information retained for a block that may be truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TruncationMetadata {
    pub limit: Option<usize>,
    pub total: Option<usize>,
    pub direction: Option<TruncationDirection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TruncationDirection {
    Head,
    Tail,
    Diff,
}

/// A stable logical subsection and its rendered representation.
///
/// Rendered lines and sources are stored behind `Arc` so an unchanged block
/// can be reused across frames without re-cloning every span/string. This is
/// the sharing boundary that makes rebuilds O(changed messages) instead of
/// O(total log).
#[derive(Debug, Clone)]
pub(crate) struct LogBlock {
    /// Identity is based on message and subsection, never on flattened rows.
    pub identity: String,
    pub kind: LogBlockKind,
    pub lines: Arc<[Line<'static>]>,
    pub sources: Arc<[LineSource]>,
    pub truncation: TruncationMetadata,
    pub foldable: bool,
}

/// Ordered logical layout of the log. Flattening is the compatibility boundary
/// for viewport drawing and text selection.
#[derive(Debug, Clone, Default)]
pub(crate) struct LogLayout {
    pub blocks: Vec<LogBlock>,
}

impl LogLayout {
    /// Total number of rendered lines across all blocks.
    pub(crate) fn total_lines(&self) -> usize {
        self.blocks.iter().map(|block| block.lines.len()).sum()
    }

    /// Absolute (0-based) starting line index of the block with the given
    /// identity, or `None` if no such block exists.
    pub(crate) fn block_start_line(&self, identity: &str) -> Option<usize> {
        let mut offset = 0;
        for block in &self.blocks {
            if block.identity == identity {
                return Some(offset);
            }
            offset += block.lines.len();
        }
        None
    }

    /// Collect the lines (and their sources) in the half-open range
    /// `[start, end)` without materializing the rest of the log. This is the
    /// virtualization boundary: only the on-screen window is cloned.
    pub(crate) fn visible_window(
        &self,
        start: usize,
        end: usize,
    ) -> (Vec<Line<'static>>, Vec<LineSource>) {
        let mut lines = Vec::new();
        let mut sources = Vec::new();
        let mut offset = 0usize;
        for block in &self.blocks {
            let len = block.lines.len();
            let seg_start = offset;
            let seg_end = offset + len;
            offset = seg_end;
            if seg_end <= start || seg_start >= end {
                continue;
            }
            let lo = start.saturating_sub(seg_start);
            let hi = end.min(seg_end) - seg_start;
            lines.extend(block.lines[lo..hi].iter().cloned());
            sources.extend(block.sources[lo..hi].iter().cloned().map(|mut source| {
                source.foldable = block.foldable && !source.streaming;
                source
            }));
        }
        (lines, sources)
    }

    pub fn dim(&mut self) {
        for block in &mut self.blocks {
            block.lines = Arc::from(dim_lines(block.lines.to_vec()));
        }
    }

    /// Compare this rendered logical layout with the previous eligible layout.
    pub(crate) fn visual_update(&self, previous: Option<&[(String, usize)]>) -> VisualUpdate {
        let Some(previous) = previous else {
            return VisualUpdate::NonContentLayoutChange;
        };
        let before_total = previous.iter().map(|(_, lines)| *lines).sum::<usize>();
        let after_total = self
            .blocks
            .iter()
            .map(|block| block.lines.len())
            .sum::<usize>();
        let delta = after_total as isize - before_total as isize;
        if delta != 0 {
            let changes: Vec<_> = self
                .blocks
                .iter()
                .filter_map(|block| {
                    let before = previous
                        .iter()
                        .find(|(identity, _)| identity == &block.identity)
                        .map_or(0, |(_, lines)| *lines);
                    (before != block.lines.len())
                        .then(|| format!("{}:{}->{}", block.identity, before, block.lines.len()))
                })
                .collect();
            log::debug!(
                target: "throbber.trace",
                "visual line delta: before={before_total} after={after_total} delta={delta} changes={changes:?}"
            );
        }
        VisualUpdate::Delta(delta)
    }

    /// Absorb a streaming shrink by appending blank lines at the bottom of
    /// the log.
    ///
    /// When a streaming block (for example the tail-windowed thinking block)
    /// loses rendered lines to edge trimming, the total log height would
    /// otherwise drop by the same amount. With auto-scroll that drop
    /// re-anchors the viewport one line higher and makes everything on screen
    /// appear to jump down. Appending the missing lines as bottom padding
    /// keeps the previous total height, so the viewport stays put and the
    /// removed lines are replaced by blanks at the bottom of the output log.
    pub(crate) fn pad_shrink(&mut self, previous: &[(String, usize)]) {
        let before_total = previous.iter().map(|(_, lines)| *lines).sum::<usize>();
        let after_total = self
            .blocks
            .iter()
            .map(|block| block.lines.len())
            .sum::<usize>();
        if after_total >= before_total {
            return;
        }
        let pad = before_total - after_total;
        log::debug!(
            target: "throbber.trace",
            "bottom padding: before={before_total} after={after_total} pad={pad}"
        );
        self.append_bottom_padding(pad);
    }

    fn append_bottom_padding(&mut self, pad: usize) {
        if pad == 0 {
            return;
        }
        let Some(last) = self.blocks.last_mut() else {
            return;
        };
        let identity = last.identity.clone();
        let foldable = last.foldable;
        let mut lines = last.lines.to_vec();
        lines.extend(std::iter::repeat_n(Line::default(), pad));
        last.lines = Arc::from(lines);
        let mut sources = last.sources.to_vec();
        sources.extend(std::iter::repeat_n(
            LineSource {
                decoration_width: 0,
                streaming: false,
                block_identity: Some(identity),
                foldable,
            },
            pad,
        ));
        last.sources = Arc::from(sources);
    }

    #[cfg(test)]
    pub fn flatten(&self) -> (Vec<Line<'static>>, Vec<LineSource>) {
        let line_count = self.blocks.iter().map(|b| b.lines.len()).sum();
        let mut lines = Vec::with_capacity(line_count);
        let mut sources = Vec::with_capacity(line_count);
        for block in &self.blocks {
            // Touch block metadata here deliberately: flattening is the sole
            // compatibility boundary and must carry the complete block model.
            let _block_key = (&block.identity, block.kind, block.truncation);
            lines.extend(block.lines.iter().cloned());
            sources.extend(block.sources.iter().cloned().map(|mut source| {
                source.foldable = block.foldable && !source.streaming;
                source
            }));
        }
        (lines, sources)
    }
}

fn tool_block_kind(msg: &Message, body: bool) -> LogBlockKind {
    let name = msg.tool_name.as_deref().unwrap_or("");
    if name == "ask_user" {
        return if body {
            LogBlockKind::AskUserResponse
        } else if msg
            .tool_args
            .as_ref()
            .and_then(|args| args.get("context"))
            .and_then(|value| value.as_str())
            .is_some_and(|context| !context.trim().is_empty())
        {
            LogBlockKind::AskUserContext
        } else {
            LogBlockKind::AskUserQuestion
        };
    }
    if body && matches!(name, "edit" | "edit_file") {
        LogBlockKind::Diff
    } else if body {
        LogBlockKind::ToolBody
    } else {
        LogBlockKind::ToolIntent
    }
}

#[cfg(test)]
mod layout_tests {
    use super::{LogBlock, LogBlockKind, LogLayout, TruncationDirection, TruncationMetadata};
    use crate::agent_turn_state::VisualUpdate;
    use crate::mouse_select::LineSource;
    use ratatui::text::Line;

    #[test]
    fn visual_update_classifies_bottom_growth_and_reflow() {
        let old = vec![("a".to_string(), 1), ("b".to_string(), 2)];
        let grown = LogLayout {
            blocks: vec![
                block("a", LogBlockKind::UserContent, "a", false),
                block("b", LogBlockKind::ToolBody, "b\nb\nb", false),
            ],
        };
        assert_eq!(grown.visual_update(Some(&old)), VisualUpdate::Delta(1));

        let replaced = LogLayout {
            blocks: vec![
                block("a", LogBlockKind::UserContent, "a", false),
                block("b", LogBlockKind::ToolBody, "z\nz", false),
            ],
        };
        assert_eq!(replaced.visual_update(Some(&old)), VisualUpdate::Delta(0));
    }

    #[test]
    fn pad_shrink_appends_bottom_padding() {
        let mut layout = LogLayout {
            blocks: vec![
                block("message:0:user", LogBlockKind::UserContent, "a", false),
                block(
                    "message:1:thinking",
                    LogBlockKind::AssistantThinking,
                    "b",
                    true,
                ),
            ],
        };
        let previous = vec![
            ("message:0:user".to_string(), 1),
            ("message:1:thinking".to_string(), 3),
        ];
        layout.pad_shrink(&previous);

        assert_eq!(layout.blocks.len(), 2, "padding extends the last block");
        let last = &layout.blocks[1];
        assert_eq!(last.lines.len(), 3);
        assert_eq!(last.sources.len(), 3);
        assert_eq!(last.lines[0].spans[0].content, "b");
        assert!(
            last.lines[1].spans.is_empty(),
            "bottom row is blank padding"
        );
        assert!(
            last.lines[2].spans.is_empty(),
            "bottom row is blank padding"
        );
    }

    #[test]
    fn pad_shrink_ignores_growth() {
        let mut layout = LogLayout {
            blocks: vec![block(
                "message:0:thinking",
                LogBlockKind::AssistantThinking,
                "a\nb\nc",
                true,
            )],
        };
        let previous = vec![("message:0:thinking".to_string(), 2)];
        layout.pad_shrink(&previous);
        assert_eq!(layout.blocks[0].lines.len(), 3);
    }

    fn block(identity: &str, kind: LogBlockKind, text: &str, streaming: bool) -> LogBlock {
        LogBlock {
            identity: identity.to_string(),
            kind,
            lines: text
                .lines()
                .map(|line| Line::raw(line.to_string()))
                .collect(),
            sources: text
                .lines()
                .map(|_| LineSource {
                    decoration_width: 3,
                    streaming,
                    block_identity: None,
                    foldable: false,
                })
                .collect(),
            truncation: TruncationMetadata {
                limit: None,
                total: None,
                direction: None,
            },
            foldable: false,
        }
    }

    #[test]
    fn flatten_preserves_block_order_and_line_metadata() {
        let layout = LogLayout {
            blocks: vec![
                block("message:1:user", LogBlockKind::UserContent, "user", false),
                block("message:2:tool", LogBlockKind::ToolBody, "tool", true),
            ],
        };
        let (lines, sources) = layout.flatten();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content, "user");
        assert_eq!(lines[1].spans[0].content, "tool");
        assert!(!sources[0].streaming);
        assert!(sources[1].streaming);
        assert_eq!(sources[0].decoration_width, 3);
    }

    #[test]
    fn truncation_metadata_records_direction_and_totals() {
        let metadata = TruncationMetadata {
            limit: Some(8),
            total: Some(20),
            direction: Some(TruncationDirection::Tail),
        };
        let layout = LogLayout {
            blocks: vec![LogBlock {
                identity: "message:3:body".into(),
                kind: LogBlockKind::ToolBody,
                lines: vec![Line::raw("… 20 total lines")].into(),
                sources: vec![LineSource {
                    decoration_width: 3,
                    streaming: false,
                    block_identity: None,
                    foldable: false,
                }]
                .into(),
                truncation: metadata,
                foldable: true,
            }],
        };
        assert_eq!(layout.blocks[0].truncation.total, Some(20));
        assert_eq!(
            layout.blocks[0].truncation.direction,
            Some(TruncationDirection::Tail)
        );
    }
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// Apply uniform dim styling to all spans in a set of pre-rendered lines.
///
/// Used to render the "to be discarded" portion of the conversation log when
/// the user is in step-back mode.
/// Dim a colour by blending it toward a dark background.
fn dim_color(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => {
            // Blend 60 % toward a near-black neutral to reduce brightness while
            // keeping hue.  The result stays noticeably darker than normal but
            // not invisible.
            let blend = |v: u8| -> u8 { ((v as u16 * 40) / 100) as u8 };
            Color::Rgb(blend(r), blend(g), blend(b))
        }
        // For named colours fall back to a fixed muted grey.
        _ => Color::Rgb(80, 80, 90),
    }
}

pub(super) fn dim_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            Line::from(
                line.spans
                    .into_iter()
                    .map(|span| {
                        let mut style = span.style;
                        // Dim fg: scale explicit colours down; for default-fg spans
                        // (plain text, model responses) apply a fixed muted grey so
                        // they are visibly dimmed rather than left at full brightness.
                        style = match style.fg {
                            Some(fg) => style.fg(dim_color(fg)),
                            None => style.fg(Color::Rgb(110, 110, 120)),
                        };
                        // Dim bg so user-message background blocks match the bar lines.
                        if let Some(bg) = style.bg {
                            style = style.bg(dim_color(bg));
                        }
                        Span::styled(span.content, style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

#[cfg(test)]
pub(super) fn build_log_layout(
    messages: &[Message],
    streaming: bool,
    width: usize,
    cfg: &ToolBodyConfig,
    theme: &Theme,
    display: &DisplayConfig,
) -> LogLayout {
    let mut cache = LogBlockCache::default();
    build_log_layout_with_expansion(
        &[],
        messages,
        0,
        streaming,
        width,
        cfg,
        theme,
        display,
        &HashSet::new(),
        &mut cache,
    )
}

// ── Render cache ──────────────────────────────────────────────────────────────

/// Cached rendered blocks for one message, keyed by its index in the display
/// message list. The key/width/streaming fields detect staleness: any change
/// re-renders and overwrites this entry, so the cache never grows past one
/// entry per message index.
///
/// `key` is a change token: the committed generation for immutable committed
/// messages (cheap, no content hashing), or a content fingerprint for the live
/// overlay tail.
#[derive(Clone)]
struct CachedBlocks {
    key: u64,
    width: usize,
    streaming: bool,
    blocks: Vec<LogBlock>,
}

/// Per-message render cache. Unchanged messages reuse their previously rendered
/// blocks instead of re-running markdown + wrapping on every frame.
///
/// Stores final [`LogBlock`]s whose lines/sources are shared behind `Arc`, so a
/// cache hit clones only the block shell (identity + `Arc` pointers), not the
/// rendered line data. A tool call's paired result is rendered into the same
/// block, so no cross-message merge happens at layout time. Keyed by message
/// index so a streaming message overwrites its own entry on each token instead
/// of accumulating one entry per token.
#[derive(Default)]
pub(crate) struct LogBlockCache {
    entries: std::collections::HashMap<usize, CachedBlocks>,
}

impl LogBlockCache {
    fn get(&self, idx: usize, key: u64, width: usize, streaming: bool) -> Option<&Vec<LogBlock>> {
        self.entries.get(&idx).and_then(|e| {
            (e.key == key && e.width == width && e.streaming == streaming).then_some(&e.blocks)
        })
    }

    fn insert(
        &mut self,
        idx: usize,
        key: u64,
        width: usize,
        streaming: bool,
        blocks: Vec<LogBlock>,
    ) {
        self.entries.insert(
            idx,
            CachedBlocks {
                key,
                width,
                streaming,
                blocks,
            },
        );
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Hash a `serde_json::Value` (which does not implement `Hash`) structurally,
/// without allocating a serialized string.
fn hash_json<H: std::hash::Hasher>(h: &mut H, value: &serde_json::Value) {
    use std::hash::Hash;
    match value {
        serde_json::Value::Null => 0u8.hash(h),
        serde_json::Value::Bool(b) => b.hash(h),
        serde_json::Value::Number(n) => {
            // serde_json stores non-negative integers as u64, negative as i64,
            // and everything else as f64; hash each form without allocating.
            if let Some(u) = n.as_u64() {
                u.hash(h);
            } else if let Some(i) = n.as_i64() {
                i.hash(h);
            } else if let Some(f) = n.as_f64() {
                f.to_bits().hash(h);
            } else {
                n.to_string().hash(h);
            }
        }
        serde_json::Value::String(s) => s.hash(h),
        serde_json::Value::Array(items) => {
            for item in items {
                hash_json(h, item);
            }
            items.len().hash(h);
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                key.hash(h);
                hash_json(h, value);
            }
            map.len().hash(h);
        }
    }
}

/// Fingerprint of a single message's render-relevant fields.
fn message_render_fingerprint(msg: &Message) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    msg.role.hash(&mut h);
    msg.content.hash(&mut h);
    msg.thinking.hash(&mut h);
    msg.assistant_phase.hash(&mut h);
    msg.tool_name.hash(&mut h);
    if let Some(args) = &msg.tool_args {
        hash_json(&mut h, args);
    }
    msg.tool_partial_args.hash(&mut h);
    if let Some(snapshot) = &msg.tool_partial_snapshot {
        hash_json(&mut h, snapshot);
    }
    msg.tool_streaming_field.hash(&mut h);
    msg.tool_running_output.hash(&mut h);
    msg.is_error.hash(&mut h);
    if let Some(dr) = &msg.display_range {
        dr.first_line.hash(&mut h);
        dr.last_line.hash(&mut h);
        dr.total_lines.hash(&mut h);
    }
    h.finish()
}

/// Fingerprint of everything that affects rendering of `messages[idx]`,
/// including the paired tool result that renders into the same block.
fn render_fingerprint(messages: &[&Message], idx: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let msg = messages[idx];
    let mut h = std::collections::hash_map::DefaultHasher::new();
    message_render_fingerprint(msg).hash(&mut h);
    if msg.role == Role::ToolCall
        && let Some(next) = messages.get(idx + 1)
        && next.role == Role::ToolResult
    {
        // The result body is rendered as part of this call's block, so the
        // result's render-relevant fields must be part of the fingerprint:
        // content and error state drive the body text, and display_range drives
        // the read_file intent suffix.
        true.hash(&mut h);
        next.content.hash(&mut h);
        next.is_error.hash(&mut h);
        if let Some(dr) = &next.display_range {
            dr.first_line.hash(&mut h);
            dr.last_line.hash(&mut h);
            dr.total_lines.hash(&mut h);
        }
    }
    h.finish()
}

fn is_static_assistant_notice(msg: &Message) -> bool {
    matches!(msg.role, Role::Assistant)
        && msg.thinking.as_deref().unwrap_or("").is_empty()
        && msg.assistant_phase.is_none()
        && msg.content.starts_with('[')
        && msg.content.ends_with(']')
}

/// Push [`LineSource`] entries for all lines added since `prev_len`,
/// assigning them to `msg_idx` with the given properties.
#[allow(clippy::too_many_arguments)]
fn push_sources(
    sources: &mut Vec<LineSource>,
    ranges: &mut Vec<(usize, usize, LogBlockKind, String)>,
    lines: &[Line<'static>],
    prev_len: usize,
    msg_idx: usize,
    kind: LogBlockKind,
    subsection: &str,
    decoration_width: u16,
    streaming: bool,
) {
    for _ in prev_len..lines.len() {
        sources.push(LineSource {
            decoration_width,
            streaming,
            block_identity: None,
            foldable: false,
        });
    }
    if prev_len < lines.len() {
        ranges.push((
            prev_len,
            lines.len(),
            kind,
            format!("message:{msg_idx}:{subsection}"),
        ));
    }
}

/// Render a single message (and, for a tool call, its paired result) into
/// final [`LogBlock`]s. Does not consult the cache.
#[allow(clippy::too_many_arguments)]
fn render_message_blocks(
    messages: &[&Message],
    idx: usize,
    width: usize,
    cfg: &ToolBodyConfig,
    theme: &Theme,
    display: &DisplayConfig,
    expanded_blocks: &HashSet<String>,
    streaming: bool,
) -> Vec<LogBlock> {
    let msg = messages[idx];
    let is_last = idx == messages.len() - 1;
    let msg_streaming = streaming && is_last && !is_static_assistant_notice(msg);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut sources: Vec<LineSource> = Vec::new();
    let mut ranges: Vec<(usize, usize, LogBlockKind, String)> = Vec::new();

    match msg.role {
        Role::User => {
            if msg.hidden {
                return Vec::new();
            }
            let user_bg = theme.log.user.bg.unwrap_or(Color::Rgb(50, 50, 64));
            let prev = lines.len();
            append_message_markdown(&mut lines, &msg.content, width, user_bg, &theme.markdown);
            push_sources(
                &mut sources,
                &mut ranges,
                &lines,
                prev,
                idx,
                LogBlockKind::UserContent,
                "content",
                0,
                msg_streaming,
            );
        }
        Role::System => {}
        Role::Assistant => {
            let thinking = msg.thinking.as_deref().unwrap_or("");
            let is_streaming_last = msg_streaming;
            let content = trim_assistant_block_edges(&msg.content);
            let has_answer = !content.is_empty();

            if !thinking.is_empty() {
                let thinking_display = {
                    let sanitized = sanitize_for_display(thinking);
                    let all_lines: Vec<&str> = sanitized.lines().collect();
                    let wrap_width = width.saturating_sub(3).max(1);
                    let mut wrapped: Vec<String> = Vec::new();
                    for logical in all_lines {
                        if logical.is_empty() {
                            wrapped.push(String::new());
                        } else {
                            wrapped.extend(wrap_str(logical, wrap_width));
                        }
                    }
                    let thinking_id = format!("message:{idx}:thinking");
                    let skip = if expanded_blocks.contains(&thinking_id) {
                        0
                    } else {
                        wrapped.len().saturating_sub(5)
                    };
                    let shown = trim_empty_edges(&wrapped[skip..], |s| s.is_empty());
                    shown.join("\n")
                };
                let prev = lines.len();
                append_message_colored(
                    &mut lines,
                    &format!("🧠 {}", thinking_display),
                    width,
                    Color::DarkGray,
                    false,
                    is_streaming_last && !has_answer,
                );
                push_sources(
                    &mut sources,
                    &mut ranges,
                    &lines,
                    prev,
                    idx,
                    LogBlockKind::AssistantThinking,
                    "thinking",
                    3,
                    msg_streaming,
                );
            }

            let effective_phase = match msg.assistant_phase {
                Some(p) => p,
                None if is_streaming_last => AssistantPhase::Unknown,
                None => AssistantPhase::Final,
            };
            let answer_icon = match effective_phase {
                AssistantPhase::Provisional => theme
                    .log
                    .assistant
                    .provisional
                    .prefix
                    .text
                    .as_deref()
                    .unwrap_or("💭 ")
                    .trim_end(),
                AssistantPhase::Final => theme
                    .log
                    .assistant
                    .r#final
                    .prefix
                    .text
                    .as_deref()
                    .unwrap_or("💬 ")
                    .trim_end(),
                AssistantPhase::Unknown if is_streaming_last => theme
                    .log
                    .assistant
                    .provisional
                    .prefix
                    .text
                    .as_deref()
                    .unwrap_or("💭 ")
                    .trim_end(),
                AssistantPhase::Unknown => theme
                    .log
                    .assistant
                    .r#final
                    .prefix
                    .text
                    .as_deref()
                    .unwrap_or("💬 ")
                    .trim_end(),
            };
            let deco_width = unicode_width::UnicodeWidthStr::width(answer_icon) as u16 + 1;

            if has_answer {
                let md_width = width.saturating_sub(3).max(1);
                let md_lines =
                    crate::markdown::render_with_theme(&content, md_width, "", &theme.markdown);
                let prev = lines.len();
                append_markdown_answer(&mut lines, answer_icon, md_lines, is_streaming_last);
                push_sources(
                    &mut sources,
                    &mut ranges,
                    &lines,
                    prev,
                    idx,
                    LogBlockKind::AssistantMarkdown,
                    "answer",
                    deco_width,
                    msg_streaming,
                );
            }
        }
        Role::ToolCall => {
            let prev = lines.len();
            let block_id = format!("message:{idx}:tool");
            let mut block_cfg = cfg.clone();
            block_cfg.full_output |= expanded_blocks.contains(&block_id);
            render_tool_call(
                messages,
                idx,
                width,
                &block_cfg,
                theme,
                display,
                &mut lines,
                msg_streaming,
            );
            push_sources(
                &mut sources,
                &mut ranges,
                &lines,
                prev,
                idx,
                tool_block_kind(msg, false),
                "intent",
                3,
                msg_streaming,
            );
            if let Some((_, _, _, identity)) = ranges.last_mut() {
                *identity = block_id.clone();
            }

            // A following result is rendered into the same visual block so the
            // fold identity and hover/fold target stay on a single block.
            if let Some(next) = messages.get(idx + 1)
                && next.role == Role::ToolResult
            {
                let body_prev = lines.len();
                let result_streaming = streaming && (idx + 1 == messages.len() - 1);
                render_tool_result(
                    messages,
                    idx + 1,
                    width,
                    &block_cfg,
                    theme,
                    display,
                    &mut lines,
                    result_streaming,
                );
                push_sources(
                    &mut sources,
                    &mut ranges,
                    &lines,
                    body_prev,
                    idx,
                    tool_block_kind(next, true),
                    "body",
                    3,
                    result_streaming,
                );
                if let Some((_, _, _, identity)) = ranges.last_mut() {
                    *identity = block_id;
                }
            }
        }
        Role::ToolResult => {
            // A result preceded by its tool call is rendered by that call above.
            if messages
                .get(idx.saturating_sub(1))
                .is_some_and(|m| m.role == Role::ToolCall)
            {
                return Vec::new();
            }
            let prev = lines.len();
            let block_id = format!("message:{idx}:body");
            let mut block_cfg = cfg.clone();
            block_cfg.full_output |= expanded_blocks.contains(&block_id);
            render_tool_result(
                messages,
                idx,
                width,
                &block_cfg,
                theme,
                display,
                &mut lines,
                msg_streaming,
            );
            push_sources(
                &mut sources,
                &mut ranges,
                &lines,
                prev,
                idx,
                tool_block_kind(msg, true),
                "body",
                3,
                msg_streaming,
            );
            if let Some((_, _, _, identity)) = ranges.last_mut() {
                *identity = block_id;
            }
        }
    }

    // Assemble final blocks, merging adjacent ranges that share an identity
    // (the tool call + result pairing above produces two such ranges).
    let mut blocks: Vec<LogBlock> = Vec::new();
    for (start, end, kind, identity) in ranges {
        let foldable = !sources[start].streaming;
        let truncation = TruncationMetadata {
            limit: None,
            total: None,
            direction: Some(match kind {
                LogBlockKind::Diff => TruncationDirection::Diff,
                LogBlockKind::ToolBody => TruncationDirection::Tail,
                _ => TruncationDirection::Head,
            }),
        };
        if let Some(previous) = blocks.last_mut()
            && previous.identity == identity
        {
            // Extend the merged block. The ranges are contiguous in `lines` and
            // `sources`, so the combined span is rebuilt from the two halves
            // without touching unrelated blocks.
            let mut combined_lines = previous.lines.to_vec();
            combined_lines.extend(lines[start..end].iter().cloned());
            previous.lines = Arc::from(combined_lines);
            let mut combined_sources = previous.sources.to_vec();
            combined_sources.extend(sources[start..end].iter().cloned().map(|mut source| {
                source.block_identity = Some(identity.clone());
                source
            }));
            previous.sources = Arc::from(combined_sources);
            previous.kind = kind;
            previous.truncation = truncation;
            previous.foldable |= foldable;
            continue;
        }
        let block_sources: Vec<LineSource> = sources[start..end]
            .iter()
            .cloned()
            .map(|mut source| {
                source.block_identity = Some(identity.clone());
                source
            })
            .collect();
        blocks.push(LogBlock {
            identity,
            kind,
            lines: Arc::from(lines[start..end].to_vec()),
            sources: Arc::from(block_sources),
            truncation,
            foldable,
        });
    }
    blocks
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_log_layout_with_expansion(
    committed: &[Message],
    overlay: &[Message],
    committed_generation: u64,
    streaming: bool,
    width: usize,
    cfg: &ToolBodyConfig,
    theme: &Theme,
    display: &DisplayConfig,
    expanded_blocks: &HashSet<String>,
    cache: &mut LogBlockCache,
) -> LogLayout {
    let committed_len = committed.len();
    // Borrow both committed and overlay messages into one flat view without
    // cloning any message content.
    let messages: Vec<&Message> = committed.iter().chain(overlay.iter()).collect();
    let mut blocks = Vec::new();
    for idx in 0..messages.len() {
        let msg = messages[idx];
        let is_last = idx == messages.len() - 1;
        let msg_streaming = streaming && is_last && !is_static_assistant_notice(msg);
        // A tool call renders its paired result's body inline, so the cache key
        // must also capture whether that result is still the streaming tail.
        let paired_result_streaming = msg.role == Role::ToolCall
            && messages
                .get(idx + 1)
                .is_some_and(|m| m.role == Role::ToolResult)
            && streaming
            && (idx + 1 == messages.len() - 1);
        let streaming_key = msg_streaming || paired_result_streaming;
        // Committed messages are immutable, so their cache entry is keyed on the
        // cheap committed generation instead of re-hashing content every token.
        // Only the live overlay tail (indices >= committed_len) needs a content
        // fingerprint.
        let key = if idx < committed_len {
            committed_generation
        } else {
            render_fingerprint(&messages, idx)
        };
        let rendered: Vec<LogBlock> = match cache.get(idx, key, width, streaming_key) {
            Some(cached) => cached.clone(),
            None => {
                let rendered = render_message_blocks(
                    &messages,
                    idx,
                    width,
                    cfg,
                    theme,
                    display,
                    expanded_blocks,
                    streaming,
                );
                cache.insert(idx, key, width, streaming_key, rendered.clone());
                rendered
            }
        };
        blocks.extend(rendered);
    }
    LogLayout { blocks }
}

// ── Tool call rendering ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_tool_call(
    messages: &[&Message],
    idx: usize,
    width: usize,
    cfg: &ToolBodyConfig,
    theme: &Theme,
    display: &DisplayConfig,
    out: &mut Vec<Line<'static>>,
    streaming: bool,
) {
    let msg = messages[idx];
    let name = msg.tool_name.as_deref().unwrap_or("unknown");

    if name == "ask_user" {
        // During streaming, tool_args is still empty; extract question and context
        // from partial streaming data (same pattern as write_file/edit_file).
        let streaming_context = msg
            .tool_partial_snapshot
            .as_ref()
            .and_then(|a| a.get("context"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                msg.tool_partial_args
                    .as_deref()
                    .and_then(|p| tool_presentation::extract_partial_field(p, "context"))
            });
        let streaming_question = msg
            .tool_partial_snapshot
            .as_ref()
            .and_then(|a| a.get("question"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                msg.tool_partial_args
                    .as_deref()
                    .and_then(|p| tool_presentation::extract_partial_field(p, "question"))
            });

        let args = msg.tool_args.as_ref();
        let context = args
            .and_then(|a| a.get("context"))
            .and_then(|v| v.as_str())
            .or(streaming_context.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let question = args
            .and_then(|a| a.get("question"))
            .and_then(|v| v.as_str())
            .or(streaming_question.as_deref())
            .unwrap_or("");

        // Context always renders in the log — the selection header only
        // shows the question, so there is no duplication risk.
        if let Some(ctx) = context {
            append_ask_user_context_block(
                out,
                ctx,
                width,
                theme.log.ask_user.bg.unwrap_or(Color::Rgb(27, 71, 31)),
                theme,
                "📋 ",
            );
        }

        // The question always renders in the log body.
        if !question.is_empty() {
            let md_width = width.saturating_sub(3).max(1);
            let md_lines =
                crate::markdown::render_with_theme(question, md_width, "", &theme.markdown);
            append_markdown_answer(out, "❓", md_lines, false);
        }

        // Response is rendered in render_tool_result; nothing more here.
        return;
    }

    // Regular tool call intent line.
    let sf = msg
        .tool_streaming_field
        .as_deref()
        .or_else(|| tool_presentation::tool_streaming_field(name));

    let (label, is_placeholder) = if name == "local_shell" {
        let prefix = msg
            .tool_args
            .as_ref()
            .and_then(|a| a.get("prefix"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let command = msg
            .tool_args
            .as_ref()
            .and_then(|a| a.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let lbl = if prefix.is_empty() {
            format!("⚙ {command}")
        } else {
            format!("⚙ {prefix} {command}")
        };
        (lbl, false)
    } else if let Some(partial) = msg.tool_partial_args.as_deref() {
        let (lbl, placeholder) =
            tool_presentation::tool_invocation_label_from_partial(name, partial, sf, display);
        if placeholder {
            if let Some(snapshot) = msg.tool_partial_snapshot.as_ref() {
                // The latest partial JSON chunk couldn't be completed, but we
                // still have a valid snapshot from a previous frame.  Use it
                // so the headline doesn't blink back to a placeholder.
                tool_presentation::tool_invocation_label(name, snapshot, sf, display)
            } else {
                (lbl, placeholder)
            }
        } else {
            (lbl, placeholder)
        }
    } else {
        match msg.tool_args.as_ref() {
            Some(args) => tool_presentation::tool_invocation_label(name, args, sf, display),
            None => (tool_presentation::tool_pending_label(name), true),
        }
    };

    // For write_file: show the content body from tool args while streaming
    // (before result arrives). This is the intent body streaming case.
    // We only show it when there is NO following ToolResult yet; once the
    // result arrives the ToolResult handler shows the content.
    let show_write_intent_body = matches!(name, "write_file" | "write")
        && !matches!(
            messages.get(idx + 1),
            Some(next) if next.role == Role::ToolResult
        );

    // For edit_file: show the diff body while streaming, before the result
    // arrives. Same dual-source pattern as write_file.
    let show_edit_intent_body = matches!(name, "edit_file" | "edit")
        && !matches!(
            messages.get(idx + 1),
            Some(next) if next.role == Role::ToolResult
        );

    // Append read_file range suffix when result is available.
    let mut intent_label = label;
    if matches!(name, "read" | "read_file")
        && let Some(next) = messages.get(idx + 1)
        && next.role == Role::ToolResult
        && let Some(ref dr) = next.display_range
    {
        intent_label.push_str(&format!(
            " [{}-{}/{}]",
            dr.first_line, dr.last_line, dr.total_lines
        ));
    }

    let color = if name == "local_shell" {
        Color::LightBlue
    } else {
        theme
            .tools
            .get(name)
            .headline_color()
            .unwrap_or(Color::Cyan)
    };
    if is_placeholder {
        // Render icon normally but text in italic+dim so the placeholder
        // nature is conveyed without distorting the emoji icon itself.
        let (icon, text) = tool_presentation::split_icon_from_label(&intent_label);
        if !text.is_empty() {
            append_message_colored_dim_with_icon(out, icon, text, width, color);
        } else {
            append_message_colored(out, &intent_label, width, color, true, streaming);
        }
    } else {
        append_message_colored(out, &intent_label, width, color, false, streaming);
    }

    // Show streaming write_file intent body.
    // Content is available either from finalized tool_args or, while still
    // streaming, extracted from tool_partial_args so the body is visible
    // throughout streaming without any disappear/reappear flicker.
    if show_write_intent_body {
        let streaming_content = msg
            .tool_partial_snapshot
            .as_ref()
            .and_then(|a| a.get("content"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                msg.tool_partial_args
                    .as_deref()
                    .and_then(|p| tool_presentation::extract_partial_field(p, "content"))
            });
        let content = msg
            .tool_args
            .as_ref()
            .and_then(|a| a.get("content"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or(streaming_content);
        if let Some(content) = content {
            let body_color = theme
                .tools
                .get("write_file")
                .body_color()
                .unwrap_or(Color::Cyan);
            render_head_truncated_body(
                out,
                &content,
                cfg.head_lines,
                cfg.full_output,
                body_color,
                width,
                true, // streaming — intent body, result not yet available
            );
        }
    }

    // Show streaming edit_file diff body.
    // old_text and new_text are extracted from tool_partial_args during
    // streaming and from tool_args once finalized, so the diff is visible
    // throughout the entire stream without flicker.
    if show_edit_intent_body {
        let extract = |field: &str| -> Option<String> {
            msg.tool_args
                .as_ref()
                .and_then(|a| a.get(field))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    msg.tool_partial_snapshot
                        .as_ref()
                        .and_then(|a| a.get(field))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    msg.tool_partial_args
                        .as_deref()
                        .and_then(|p| tool_presentation::extract_partial_field(p, field))
                })
        };
        let old_text = extract("old_text").unwrap_or_default();
        let new_text = extract("new_text").unwrap_or_default();
        if !old_text.is_empty() || !new_text.is_empty() {
            render_diff_body(
                out,
                &old_text,
                &new_text,
                cfg.diff_lines,
                cfg.full_output,
                width,
                theme,
                true, // streaming — args may be partial
            );
        }
    }

    // Show live subprocess output while the tool is still running (no result yet).
    if let Some(output) = msg.tool_running_output.as_deref()
        && !output.is_empty()
        && !matches!(
            messages.get(idx + 1),
            Some(next) if next.role == Role::ToolResult
        )
    {
        let body_color = theme.log.diff.unchanged.fg.unwrap_or(Color::DarkGray);
        render_tail_truncated_body(
            out,
            output,
            cfg.tail_lines,
            cfg.full_output,
            body_color,
            width,
            true, // streaming — subprocess still running
        );
    }
}

// ── Tool result rendering ─────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn render_tool_result(
    messages: &[&Message],
    idx: usize,
    width: usize,
    cfg: &ToolBodyConfig,
    theme: &Theme,
    _display: &DisplayConfig,
    out: &mut Vec<Line<'static>>,
    streaming: bool,
) {
    let msg = messages[idx];
    let prev = messages.get(idx.saturating_sub(1));
    let prev_name = prev
        .filter(|p| p.role == Role::ToolCall)
        .and_then(|p| p.tool_name.as_deref())
        .unwrap_or("unknown");

    // ask_user: response is committed as part of the ToolCall rendering above.
    // Here we just append the response block.
    if prev_name == "ask_user" {
        append_ask_user_response(
            out,
            &msg.content,
            width,
            theme.log.user.bg.unwrap_or(Color::Rgb(50, 50, 64)),
        );
        return;
    }

    // local_shell: existing color treatment, tail-truncated.
    if prev_name == "local_shell" {
        let color = if msg.is_error {
            theme.log.diff.removed.fg.unwrap_or(Color::LightRed)
        } else {
            Color::LightBlue
        };
        let content = sanitize_for_display(&msg.content);
        render_tail_truncated_body(
            out,
            &content,
            cfg.tail_lines,
            cfg.full_output,
            color,
            width,
            streaming,
        );
        return;
    }

    // edit_file: compact diff from tool args old_text/new_text.
    if matches!(prev_name, "edit" | "edit_file") {
        // If error, fall through to plain content rendering.
        if !msg.is_error {
            let old_text = prev
                .and_then(|p| p.tool_args.as_ref())
                .and_then(|a| a.get("old_text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_text = prev
                .and_then(|p| p.tool_args.as_ref())
                .and_then(|a| a.get("new_text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !old_text.is_empty() || !new_text.is_empty() {
                render_diff_body(
                    out,
                    old_text,
                    new_text,
                    cfg.diff_lines,
                    cfg.full_output,
                    width,
                    theme,
                    false, // finalized — args are complete, exact match only
                );
                return;
            }
        }
        // Fallthrough to plain content on error or missing args.
        let color = if msg.is_error {
            theme.log.diff.removed.fg.unwrap_or(Color::Red)
        } else {
            theme.log.diff.added.fg.unwrap_or(Color::Green)
        };
        let content = sanitize_for_display(&msg.content);
        render_tail_truncated_body(
            out,
            &content,
            cfg.tail_lines,
            cfg.full_output,
            color,
            width,
            streaming,
        );
        return;
    }

    // write_file: show written content from tool args (head-truncated).
    if matches!(prev_name, "write" | "write_file") && !msg.is_error {
        let content = prev
            .and_then(|p| p.tool_args.as_ref())
            .and_then(|a| a.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let color = theme
            .tools
            .get(prev_name)
            .body_color()
            .unwrap_or(Color::Green);
        render_head_truncated_body(
            out,
            content,
            cfg.head_lines,
            cfg.full_output,
            color,
            width,
            streaming,
        );
        return;
    }

    // read_file / find_files: head-truncated.
    if matches!(prev_name, "read" | "read_file" | "find" | "find_files") {
        let color = if msg.is_error {
            theme.log.diff.removed.fg.unwrap_or(Color::Red)
        } else {
            theme
                .tools
                .get(prev_name)
                .body_color()
                .unwrap_or(Color::Green)
        };
        let content = sanitize_for_display(&msg.content);
        render_head_truncated_body(
            out,
            &content,
            cfg.head_lines,
            cfg.full_output,
            color,
            width,
            streaming,
        );
        return;
    }

    // read_skill: show only the invocation label (already rendered), no body.
    if prev_name == "read_skill" {
        return;
    }

    // bash / cmd / powershell / exec / python: tail-truncated.
    if matches!(
        prev_name,
        "bash" | "cmd" | "powershell" | "exec" | "run_python"
    ) {
        let color = if msg.is_error {
            theme.log.diff.removed.fg.unwrap_or(Color::LightRed)
        } else {
            theme
                .tools
                .get(prev_name)
                .body_color()
                .unwrap_or(Color::LightBlue)
        };
        let content = sanitize_for_display(&msg.content);
        render_tail_truncated_body(
            out,
            &content,
            cfg.tail_lines,
            cfg.full_output,
            color,
            width,
            streaming,
        );
        return;
    }

    // Custom / unknown tools: tail-truncated, green/red.
    let color = if msg.is_error {
        theme.log.diff.removed.fg.unwrap_or(Color::Red)
    } else {
        theme
            .tools
            .get(prev_name)
            .body_color()
            .unwrap_or(Color::Green)
    };
    let content = sanitize_for_display(&msg.content);
    render_tail_truncated_body(
        out,
        &content,
        cfg.tail_lines,
        cfg.full_output,
        color,
        width,
        streaming,
    );
}

// ── Body rendering helpers ────────────────────────────────────────────────────

/// Trim leading and trailing items for which `is_empty` returns true.
///
/// Empty items between non-empty items are preserved — only the edges are
/// trimmed.  Returns an empty slice when all items are empty.
fn trim_empty_edges<T>(slice: &[T], is_empty: impl Fn(&T) -> bool) -> &[T] {
    let start = slice
        .iter()
        .position(|x| !is_empty(x))
        .unwrap_or(slice.len());
    let end = slice
        .iter()
        .rposition(|x| !is_empty(x))
        .map(|i| i + 1)
        .unwrap_or(start);
    &slice[start..end]
}

/// A single wrapped (visual) line produced from a logical line of content.
struct WrappedLine {
    text: String,
    /// Which logical line this chunk belongs to (0-indexed).
    logical_idx: usize,
    /// First wrapped chunk of its logical line.
    is_first_chunk: bool,
    /// Last wrapped chunk of its logical line.
    is_last_chunk: bool,
}

/// Wrap every logical line in `content` to `width` columns, returning a flat
/// list of `WrappedLine` entries with logical-line metadata.
fn wrap_content(content: &str, width: usize) -> Vec<WrappedLine> {
    let mut out: Vec<WrappedLine> = Vec::new();
    for (li, line) in content.lines().enumerate() {
        let normalized = normalize_terminal_segment(line, 3);
        let chunks = wrap_str(&normalized, width);
        let chunk_count = chunks.len();
        for (ci, chunk) in chunks.into_iter().enumerate() {
            out.push(WrappedLine {
                text: chunk,
                logical_idx: li,
                is_first_chunk: ci == 0,
                is_last_chunk: ci == chunk_count - 1,
            });
        }
    }
    out
}

/// Render head-truncated body: show first `max_lines` wrapped lines, then truncation marker.
///
/// The limit is enforced on wrapped (visual) lines, not logical lines, so very
/// long logical lines that wrap to many visual lines are still bounded.
///
/// The first visible content line uses `╭` (the true start is shown).
/// The last content line uses `╰` (confirmed) or `┆` (streaming) when the body
/// is not truncated; truncated bodies end with a truncation marker.
/// A single-line body uses `·` (self-contained, no continuation implied).
/// Wrapped chunks of the same logical line continue with `│`.
fn render_head_truncated_body(
    out: &mut Vec<Line<'static>>,
    content: &str,
    max_lines: usize,
    full_output: bool,
    color: Color,
    width: usize,
    is_streaming: bool,
) {
    if content.trim().is_empty() {
        return;
    }
    let content_width = width.saturating_sub(3).max(1);
    let total_logical = content.lines().count();
    let wrapped = wrap_content(content, content_width);
    let total_wrapped = wrapped.len();

    let limit = if full_output {
        total_wrapped
    } else {
        max_lines
    };
    let truncated = !full_output && total_wrapped > max_lines;
    let shown = trim_empty_edges(&wrapped[..limit.min(total_wrapped)], |wl| {
        wl.text.is_empty()
    });

    for wl in shown {
        let is_first_logical = wl.logical_idx == 0 && wl.is_first_chunk;
        // We say the last logical line is visible only when all wrapped chunks
        // through the very end are displayed (no truncation).
        let is_last_logical = !truncated && wl.logical_idx + 1 == total_logical && wl.is_last_chunk;

        let marker = if is_last_logical && is_streaming {
            '┆'
        } else if is_first_logical && is_last_logical {
            '·'
        } else if is_last_logical {
            '╰'
        } else if is_first_logical && wl.is_first_chunk {
            '╭'
        } else {
            '│'
        };

        out.push(tool_result_line_subdued(marker, &wl.text, color));
    }

    if truncated {
        out.push(placeholder_result_line(
            format!("… {total_logical} total lines"),
            color,
        ));
    }
}

/// Render tail-truncated body: show truncation marker then last `max_lines` wrapped lines.
///
/// The limit is enforced on wrapped (visual) lines, not logical lines, so very
/// long logical lines that wrap to many visual lines are still bounded.
///
/// When a truncation marker precedes, the first visible content line uses `│`
/// (the true start is hidden).  Otherwise it uses `╭`.  The last content line
/// always uses `╰` (confirmed) or `┆` (streaming) — the end is always visible.
/// A single-line body uses `·` (self-contained, no continuation implied).
/// Wrapped chunks of the same logical line continue with `│`.
///
/// To avoid wrapping the entire content when only the last few visual lines are
/// needed, we only process the tail portion of logical lines (up to
/// `max_lines * 16` logical lines from the end, a generous over-estimate).
fn render_tail_truncated_body(
    out: &mut Vec<Line<'static>>,
    content: &str,
    max_lines: usize,
    full_output: bool,
    color: Color,
    width: usize,
    is_streaming: bool,
) {
    if content.trim().is_empty() {
        return;
    }
    let content_width = width.saturating_sub(3).max(1);
    let total_logical = content.lines().count();

    // Only process the tail portion: enough logical lines to fill the visual
    // budget even with very long lines.  If full_output is set we still wrap
    // everything — that path is for user-requested untruncated display.
    let tail_logical = if full_output {
        total_logical
    } else {
        (max_lines * 16).min(total_logical)
    };
    let logical_offset = total_logical - tail_logical;

    let content_to_wrap: String = if logical_offset > 0 {
        content
            .lines()
            .skip(logical_offset)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        content.to_string()
    };

    let wrapped_full = wrap_content(&content_to_wrap, content_width);
    // Adjust logical_idx so it is relative to the full original content.
    let wrapped: Vec<WrappedLine> = wrapped_full
        .into_iter()
        .map(|mut wl| {
            wl.logical_idx += logical_offset;
            wl
        })
        .collect();

    let total_wrapped = wrapped.len();

    let truncated = !full_output && (total_wrapped > max_lines || logical_offset > 0);
    if truncated && (logical_offset > 0 || total_wrapped > max_lines) {
        out.push(placeholder_result_line(
            format!("… {total_logical} total lines"),
            color,
        ));
    }
    let start = if full_output || total_wrapped <= max_lines {
        0
    } else {
        total_wrapped - max_lines
    };
    let shown = trim_empty_edges(&wrapped[start..], |wl| wl.text.is_empty());

    for wl in shown {
        // `is_first_logical` is true only when the first wrapped chunk of the
        // very first logical line is visible AND no truncation hides any
        // earlier content.
        let is_first_logical = !truncated && wl.logical_idx == 0 && wl.is_first_chunk;
        // Tail-truncated always shows through the end, so the last logical
        // line's last chunk marks the true end.
        let is_last_logical = wl.logical_idx + 1 == total_logical && wl.is_last_chunk;

        let marker = if is_last_logical && is_streaming {
            '┆'
        } else if is_first_logical && is_last_logical {
            '·'
        } else if is_last_logical {
            '╰'
        } else if is_first_logical && wl.is_first_chunk {
            '╭'
        } else {
            '│'
        };

        out.push(tool_result_line(marker, &wl.text, color));
    }
}

/// Render a compact diff body for edit_file.
///
/// The per-side line limit is enforced on wrapped (visual) lines, not logical
/// lines, so very long logical lines that wrap to many visual lines are still
/// bounded.
#[allow(clippy::too_many_arguments)]
fn render_diff_body(
    out: &mut Vec<Line<'static>>,
    old_text: &str,
    new_text: &str,
    max_lines_per_side: usize,
    full_output: bool,
    width: usize,
    theme: &Theme,
    streaming: bool,
) {
    let removed_color = theme.log.diff.removed.fg.unwrap_or(Color::LightRed);
    let added_color = theme.log.diff.added.fg.unwrap_or(Color::LightGreen);
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    /// True when `a` and `b` match.  During streaming a partial line that
    /// is a prefix of the other side is treated as matching so the diff
    /// doesn't flicker while content is still arriving.  Once the result is
    /// finalized we only accept exact matches — otherwise a new line whose
    /// text starts with the old line's content is silently swallowed.
    fn lines_match(a: &str, b: &str, streaming: bool) -> bool {
        a == b || (streaming && (a.starts_with(b) || b.starts_with(a)))
    }

    // Compute common head length.
    let common_head = old_lines
        .iter()
        .zip(new_lines.iter())
        .take_while(|(a, b)| lines_match(a, b, streaming))
        .count();

    // Compute common tail length (must not overlap with head).
    let old_tail_max = old_lines.len().saturating_sub(common_head);
    let new_tail_max = new_lines.len().saturating_sub(common_head);
    let common_tail = old_lines[old_lines.len().saturating_sub(old_tail_max)..]
        .iter()
        .rev()
        .zip(
            new_lines[new_lines.len().saturating_sub(new_tail_max)..]
                .iter()
                .rev(),
        )
        .take_while(|(a, b)| lines_match(a, b, streaming))
        .count();

    let old_diff = &old_lines[common_head..old_lines.len() - common_tail];
    let new_diff = &new_lines[common_head..new_lines.len() - common_tail];

    let old_total = old_diff.len();
    let new_total = new_diff.len();

    let is_pure_addition = old_total == 0;
    let is_pure_removal = new_total == 0;

    let content_width = width.saturating_sub(3).max(1);

    // Helper: push a combined "total + common" filler when both apply.
    let push_total_common =
        |out: &mut Vec<Line<'static>>, total: usize, common: usize, color: Color| {
            if total > 0 && common > 0 {
                out.push(placeholder_result_line(
                    format!("… {total} total lines + {common} common lines"),
                    color,
                ));
            } else if total > 0 {
                out.push(placeholder_result_line(
                    format!("… {total} total lines"),
                    color,
                ));
            } else if common > 0 {
                out.push(placeholder_result_line(
                    format!("… {common} common lines"),
                    color,
                ));
            }
        };

    // Helper: render a slice of logical lines as wrapped lines, limiting at
    // the wrapped level. Pushes rendered lines to `out`.
    let render_diff_block = |out: &mut Vec<Line<'static>>, diff_lines: &[&str], color: Color| {
        for line in diff_lines {
            let normalized = normalize_terminal_segment(line, 3);
            let chunks = wrap_str(&normalized, content_width);
            for chunk in chunks {
                out.push(tool_result_line('│', chunk, color));
            }
        }
    };

    // Helper: render a slice of logical lines with a wrapped-line limit.
    // Stops once `max_wrapped` wrapped lines have been emitted.
    let render_diff_block_limited =
        |out: &mut Vec<Line<'static>>, diff_lines: &[&str], max_wrapped: usize, color: Color| {
            let mut emitted = 0usize;
            for line in diff_lines {
                if emitted >= max_wrapped {
                    break;
                }
                let normalized = normalize_terminal_segment(line, 3);
                let chunks = wrap_str(&normalized, content_width);
                for chunk in chunks {
                    if emitted >= max_wrapped {
                        break;
                    }
                    out.push(tool_result_line('│', chunk, color));
                    emitted += 1;
                }
            }
        };

    // ── Removed lines ────────────────────────────────────────────────────
    if old_total > 0 {
        if common_head > 0 && !is_pure_removal {
            out.push(placeholder_result_line(
                format!("… {common_head} common lines"),
                removed_color,
            ));
        }
        if full_output {
            render_diff_block(out, old_diff, removed_color);
        } else {
            render_diff_block_limited(out, old_diff, max_lines_per_side, removed_color);
        }
        let truncated = !full_output && old_total > max_lines_per_side;
        let total_filler = if truncated { old_total } else { 0 };
        let common_filler = if common_tail > 0 && !is_pure_removal {
            common_tail
        } else {
            0
        };
        push_total_common(out, total_filler, common_filler, removed_color);
    }

    // ── Added lines ──────────────────────────────────────────────────────
    if new_total > 0 {
        if common_head > 0 && !is_pure_addition {
            out.push(placeholder_result_line(
                format!("… {common_head} common lines"),
                added_color,
            ));
        }
        if full_output {
            render_diff_block(out, new_diff, added_color);
        } else {
            render_diff_block_limited(out, new_diff, max_lines_per_side, added_color);
        }
        let truncated = !full_output && new_total > max_lines_per_side;
        let total_filler = if truncated { new_total } else { 0 };
        let common_filler = if common_tail > 0 && !is_pure_addition {
            common_tail
        } else {
            0
        };
        push_total_common(out, total_filler, common_filler, added_color);
    }
}

/// Build a body content line with a block-drawing margin marker at column 1.
///
/// Layout: `·` at column 0, marker at column 1, `·` at column 2, content from
/// column 3 onward.  Both the marker prefix and the content share `color`.
fn tool_result_line(marker: char, content: impl Into<String>, color: Color) -> Line<'static> {
    let style = Style::default().fg(color);
    Line::from(vec![
        Span::styled(format!(" {} ", marker), style),
        Span::styled(content.into(), style),
    ])
}

fn tool_result_line_subdued(
    marker: char,
    content: impl Into<String>,
    color: Color,
) -> Line<'static> {
    let content_style = Style::default().fg(color);
    // Keep ordinary output rails visible without competing with the output.
    let marker_style = content_style.add_modifier(Modifier::DIM);
    Line::from(vec![
        Span::styled(format!(" {} ", marker), marker_style),
        Span::styled(content.into(), content_style),
    ])
}

/// Build a truncation/context placeholder line with a `┆` margin marker at
/// column 1.  The marker is rendered in `color`; the text is rendered in
/// `color` + dim + italic.
fn placeholder_result_line(text: impl Into<String>, color: Color) -> Line<'static> {
    let marker_style = Style::default().fg(color);
    let text_style = Style::default()
        .fg(color)
        .add_modifier(Modifier::ITALIC | Modifier::DIM);
    Line::from(vec![
        Span::styled(" ┆ ", marker_style),
        Span::styled(text.into(), text_style),
    ])
}

// ── ask_user block helpers ────────────────────────────────────────────────────

/// Context block: green background, readable text, with an emoji prefix
/// (e.g. "📋 ").  No DIM — the green background alone distinguishes it from
/// surrounding content.
fn append_ask_user_context_block(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    bg: Color,
    theme: &Theme,
    emoji: &str,
) {
    let bg_style = Style::default().bg(bg);
    let padding_style = Style::default().bg(bg);
    let md_lines = crate::markdown::render_with_theme(content, width, emoji, &theme.markdown);
    for line in md_lines {
        let styled: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|s| Span::styled(s.content, bg_style.patch(s.style)))
            .collect();
        let text_width: usize = styled.iter().map(|s| s.content.width()).sum();
        let padding = width.saturating_sub(text_width);
        let mut spans = styled;
        if padding > 0 {
            spans.push(Span::styled(" ".repeat(padding), padding_style));
        }
        out.push(Line::from(spans));
    }
}

/// Response block: rendered like a normal user message but with the ask_user background color.
fn append_ask_user_response(out: &mut Vec<Line<'static>>, content: &str, width: usize, bg: Color) {
    let bg_style = Style::default().bg(bg);
    let sanitized = sanitize_for_display(content);
    let segments: Vec<&str> = sanitized.split('\n').collect();
    let visible = visible_segments(&segments);

    out.push(halfblock_line(width, '▄', bg));

    for seg_idx in visible {
        let segment = segments[seg_idx];
        let normalized = normalize_terminal_segment(segment, 0);
        let chunks = wrap_str(&normalized, width);
        for chunk in chunks {
            let text_cols = chunk.as_str().width();
            let padding = width.saturating_sub(text_cols);
            let padded = format!("{}{}", chunk, " ".repeat(padding));
            out.push(Line::from(Span::styled(padded, bg_style)));
        }
    }

    out.push(halfblock_line(width, '▀', bg));
}

// ── Shared rendering primitives ───────────────────────────────────────────────

/// Return the indices of segments to keep: strip leading/trailing empty
/// lines while preserving interior empty lines. An empty input returns an
/// empty vector so that callers can iterate directly without a sentinel.
fn visible_segments(segments: &[&str]) -> Vec<usize> {
    segments
        .iter()
        .enumerate()
        .filter(|(idx, seg)| {
            if !seg.is_empty() {
                return true;
            }
            let has_nonempty_before = segments[..*idx].iter().any(|s| !s.is_empty());
            let has_nonempty_after = segments[idx + 1..].iter().any(|s| !s.is_empty());
            has_nonempty_before && has_nonempty_after
        })
        .map(|(idx, _)| idx)
        .collect()
}

fn trim_assistant_block_edges(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(start) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return String::new();
    };
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or(start);

    let mut out = String::new();
    for (idx, line) in lines[start..=end].iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let is_first = idx == 0;
        let is_last = start + idx == end;
        let rendered = if is_first && is_last {
            line.trim()
        } else if is_first {
            line.trim_start()
        } else if is_last {
            line.trim_end()
        } else {
            line
        };
        out.push_str(rendered);
    }

    out
}

fn halfblock_line(width: usize, ch: char, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        ch.to_string().repeat(width),
        Style::default().fg(color),
    ))
}

fn append_message_colored(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    color: Color,
    dim: bool,
    streaming: bool,
) {
    let mut style = Style::default().fg(color);
    if dim {
        style = style.add_modifier(Modifier::ITALIC | Modifier::DIM);
    }
    let segments: Vec<&str> = content.split('\n').collect();
    let visible = visible_segments(&segments);
    let content_width = width.saturating_sub(3).max(1);
    let last_visible_idx = visible.len() - 1;

    // The last line of a truncated block is a "… N total lines" placeholder
    // which should always use ┆, not ╰ — truncation implies more content exists.
    let last_is_truncation_marker = visible.last().is_some_and(|&idx| {
        let text = segments[idx];
        text.starts_with("… ") && text.ends_with(" total lines")
    });
    let ending = if streaming || last_is_truncation_marker {
        " ┆ "
    } else {
        " ╰ "
    };

    for (vi, &seg_idx) in visible.iter().enumerate() {
        let normalized = normalize_terminal_segment(segments[seg_idx], 0);

        if vi == 0 {
            // First line: icon at cols 0-1, space at col 2, text at col 3+.
            let (icon, text) = tool_presentation::split_icon_from_label(&normalized);
            let prefix = format!("{icon} ");
            let chunks = wrap_str(text, content_width);
            let last_chunk = chunks.len() - 1;
            for (ci, chunk) in chunks.iter().enumerate() {
                if ci == 0 {
                    out.push(Line::from(vec![
                        Span::styled(prefix.clone(), style),
                        Span::styled(chunk.clone(), style),
                    ]));
                } else {
                    let marker = if ci == last_chunk && vi == last_visible_idx {
                        ending
                    } else {
                        " │ "
                    };
                    out.push(Line::from(vec![
                        Span::styled(marker, style),
                        Span::styled(chunk.clone(), style),
                    ]));
                }
            }
        } else {
            // Subsequent logical lines (multiline labels).
            let chunks = wrap_str(&normalized, content_width);
            let last_chunk = chunks.len() - 1;
            for (ci, chunk) in chunks.iter().enumerate() {
                let marker = if ci == last_chunk && vi == last_visible_idx {
                    ending
                } else {
                    " │ "
                };
                out.push(Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled(chunk.clone(), style),
                ]));
            }
        }
    }
}

/// Like `append_message_colored` with dim=true but renders an icon prefix without
/// italic/dim so the emoji stays visually clean while the placeholder text
/// is still marked as provisional.  Content aligned to column 3.
fn append_message_colored_dim_with_icon(
    out: &mut Vec<Line<'static>>,
    icon: &str,
    text: &str,
    width: usize,
    color: Color,
) {
    let icon_style = Style::default().fg(color);
    let text_style = Style::default()
        .fg(color)
        .add_modifier(Modifier::ITALIC | Modifier::DIM);
    let prefix = format!("{icon} ");
    out.push(Line::from(vec![
        Span::styled(prefix, icon_style),
        Span::styled(text.to_string(), text_style),
    ]));
    let _ = width;
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn append_tool_result_block(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    color: Color,
) {
    let marker_style = Style::default().fg(color);
    let text_style = Style::default().fg(color);

    if content.is_empty() {
        let no_output_style = Style::default()
            .fg(Color::Rgb(100, 100, 120))
            .add_modifier(Modifier::ITALIC);
        out.push(Line::from(vec![
            Span::styled(" │ ", marker_style),
            Span::styled("(no output)", no_output_style),
        ]));
        return;
    }

    if width == 0 {
        out.push(Line::from(vec![Span::styled(
            " │ ".to_string(),
            marker_style,
        )]));
        return;
    }

    let content_width = width.saturating_sub(3).max(1);
    let segments: Vec<&str> = content.split('\n').collect();
    for seg_idx in visible_segments(&segments) {
        let segment = segments[seg_idx];
        let normalized = normalize_terminal_segment(segment, 3);
        let chunks = wrap_str(&normalized, content_width);
        for chunk in chunks {
            out.push(Line::from(vec![
                Span::styled(" │ ", marker_style),
                Span::styled(chunk, text_style),
            ]));
        }
    }
}

fn append_markdown_answer(
    out: &mut Vec<Line<'static>>,
    icon: &str,
    md_lines: Vec<Line<'static>>,
    streaming: bool,
) {
    if md_lines.is_empty() {
        if streaming {
            out.push(Line::from(Span::styled(
                "▋",
                Style::default().fg(Color::Yellow),
            )));
        }
        return;
    }

    let prefix = format!("{icon} ");
    let last_idx = md_lines.len() - 1;

    for (i, mut line) in md_lines.into_iter().enumerate() {
        if i == 0 {
            // First line: prepend emoji icon prefix.
            line.spans.insert(0, Span::raw(prefix.clone()));
        } else {
            // Continuation line: prepend margin marker.
            let marker = if i == last_idx && streaming {
                " ┆ "
            } else if i == last_idx {
                " ╰ "
            } else {
                " │ "
            };
            line.spans.insert(0, Span::raw(marker));
        }

        if streaming && i == last_idx {
            line.spans
                .push(Span::styled("▋", Style::default().fg(Color::Yellow)));
        }

        out.push(line);
    }
}

fn append_message_markdown(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    bg: Color,
    markdown_theme: &crate::theme::MarkdownTheme,
) {
    // Preserve user newlines: convert isolated \n to markdown hard breaks (  \n)
    // while leaving \n\n (paragraph breaks) intact.
    // Uses \x00 as a temporary placeholder for \n\n.
    let processed = content
        .replace("\n\n", "\x00")
        .replace('\n', "  \n")
        .replace('\x00', "\n\n");
    let md_lines = crate::markdown::render_with_theme(&processed, width, "", markdown_theme);
    if md_lines.is_empty() {
        return;
    }

    out.push(halfblock_line(width, '▄', bg));

    for line in md_lines {
        let text_width: usize = line.spans.iter().map(|s| s.content.width()).sum();
        let padding = width.saturating_sub(text_width);
        let mut spans: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|s| Span::styled(s.content, s.style.bg(bg)))
            .collect();
        if padding > 0 {
            spans.push(Span::styled(" ".repeat(padding), Style::default().bg(bg)));
        }
        out.push(Line::from(spans));
    }

    out.push(halfblock_line(width, '▀', bg));
}

pub(super) fn sanitize_for_display(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for line in text.split('\n') {
        s.push_str(line.trim_end());
        s.push('\n');
    }
    if s.ends_with('\n') {
        s.pop();
    }

    let s = s.trim_matches('\n');
    let mut result = String::with_capacity(s.len());
    let mut newline_run = 0usize;
    for ch in s.chars() {
        if ch == '\n' {
            newline_run += 1;
        } else {
            for _ in 0..newline_run.min(2) {
                result.push('\n');
            }
            newline_run = 0;
            result.push(ch);
        }
    }
    for _ in 0..newline_run.min(2) {
        result.push('\n');
    }
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        LogBlockCache, LogLayout, ToolBodyConfig, build_log_layout,
        build_log_layout_with_expansion, dim_lines, message_render_fingerprint,
        trim_assistant_block_edges,
    };
    use crate::{
        config::DisplayConfig,
        llm::{AssistantPhase, DisplayRange, Message, Role},
        theme::Theme,
    };
    use ratatui::{
        style::Color,
        text::{Line, Span},
    };

    #[test]
    fn dim_lines_dims_fg_proportionally() {
        use ratatui::style::Style;
        let lines = vec![Line::from(vec![Span::styled(
            "hello",
            Style::default().fg(Color::Rgb(200, 100, 50)),
        )])];
        let dimmed = dim_lines(lines);
        // 200 * 40/100 = 80, 100 * 40/100 = 40, 50 * 40/100 = 20
        assert_eq!(dimmed[0].spans[0].style.fg, Some(Color::Rgb(80, 40, 20)));
    }

    #[test]
    fn dim_lines_dims_bg_proportionally() {
        use ratatui::style::Style;
        // Default user bg = Rgb(50, 50, 64)
        let bg = Color::Rgb(50, 50, 64);
        let lines = vec![Line::from(vec![Span::styled(
            "hello",
            Style::default().bg(bg),
        )])];
        let dimmed = dim_lines(lines);
        // 50*40/100=20, 50*40/100=20, 64*40/100=25
        assert_eq!(dimmed[0].spans[0].style.bg, Some(Color::Rgb(20, 20, 25)));
    }

    #[test]
    fn dim_lines_bar_fg_and_text_bg_match() {
        use ratatui::style::Style;
        // Bar line: fg matches user bg, no bg
        let user_bg = Color::Rgb(50, 50, 64);
        let bar_line = Line::from(vec![Span::styled("▄▄▄", Style::default().fg(user_bg))]);
        // Text line: bg matches user bg, no fg
        let text_line = Line::from(vec![Span::styled("hello", Style::default().bg(user_bg))]);
        let dimmed = dim_lines(vec![bar_line, text_line]);
        let bar_fg = dimmed[0].spans[0].style.fg.unwrap();
        let text_bg = dimmed[1].spans[0].style.bg.unwrap();
        assert_eq!(
            bar_fg, text_bg,
            "bar fg and text bg must match after dimming"
        );
    }

    #[test]
    fn dim_lines_dims_plain_spans_with_fallback_grey() {
        let lines = vec![Line::from(vec![Span::raw("hello")])];
        let dimmed = dim_lines(lines);
        assert_eq!(
            dimmed[0].spans[0].style.fg,
            Some(Color::Rgb(110, 110, 120)),
            "plain spans must get fallback muted grey"
        );
    }

    #[test]
    fn dim_lines_preserves_span_content() {
        let lines = vec![Line::from(vec![Span::raw("hello")])];
        let dimmed = dim_lines(lines);
        assert_eq!(dimmed[0].spans[0].content, "hello");
    }

    fn cfg() -> ToolBodyConfig {
        ToolBodyConfig::default()
    }

    #[test]
    fn message_render_fingerprint_is_field_sensitive() {
        let base = Message::assistant("hello");
        let fp = message_render_fingerprint(&base);
        assert_ne!(fp, 0);

        let mut m = base.clone();
        m.content = "world".into();
        assert_ne!(message_render_fingerprint(&m), fp, "content");

        let mut m = base.clone();
        m.thinking = Some("thinking".into());
        assert_ne!(message_render_fingerprint(&m), fp, "thinking");

        let mut m = base.clone();
        m.tool_name = Some("bash".into());
        assert_ne!(message_render_fingerprint(&m), fp, "tool_name");

        let mut m = base.clone();
        m.tool_args = Some(serde_json::json!({"command": "ls"}));
        assert_ne!(message_render_fingerprint(&m), fp, "tool_args");

        let mut m = base.clone();
        m.tool_running_output = Some("output".into());
        assert_ne!(message_render_fingerprint(&m), fp, "tool_running_output");

        let mut m = base.clone();
        m.is_error = true;
        assert_ne!(message_render_fingerprint(&m), fp, "is_error");

        let mut m = base.clone();
        m.display_range = Some(DisplayRange {
            first_line: 1,
            last_line: 10,
            total_lines: 100,
        });
        assert_ne!(message_render_fingerprint(&m), fp, "display_range");

        assert_ne!(
            message_render_fingerprint(&Message::user("hello")),
            fp,
            "role"
        );
    }

    #[test]
    fn cache_reuses_unchanged_blocks_during_streaming() {
        let user = Message::user("first question");
        let assistant = Message::assistant("partial");
        let theme = crate::theme::Theme::default();
        let display = crate::config::DisplayConfig::default();
        let mut cache = LogBlockCache::default();

        let layout1 = build_log_layout_with_expansion(
            &[],
            &[user.clone(), assistant],
            0,
            true,
            80,
            &cfg(),
            &theme,
            &display,
            &std::collections::HashSet::new(),
            &mut cache,
        );
        let entries_after_first = cache.len();
        assert_eq!(entries_after_first, 2, "user + assistant cached");

        // Streaming growth: only the tail assistant changes, so only it should
        // re-render; the user block must be a cache hit.
        let layout2 = build_log_layout_with_expansion(
            &[],
            &[user, Message::assistant("partial answer grows longer")],
            0,
            true,
            80,
            &cfg(),
            &theme,
            &display,
            &std::collections::HashSet::new(),
            &mut cache,
        );
        assert_eq!(
            cache.len(),
            entries_after_first,
            "cache stays bounded (streaming tail overwrites its own entry)"
        );

        let text = |layout: &LogLayout, i: usize| -> Vec<String> {
            layout.blocks[i]
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        };
        assert_eq!(text(&layout1, 0), text(&layout2, 0), "user block unchanged");
        assert_ne!(
            text(&layout1, 1),
            text(&layout2, 1),
            "assistant content changed"
        );
    }

    #[test]
    fn non_tail_tool_running_output_update_reuses_other_blocks() {
        let user = Message::user("run this");
        let mut tool = Message::tool_call("c1", "bash", serde_json::json!({"command": "seq 5"}));
        tool.tool_running_output = Some("1\n2".to_string());
        let assistant = Message::assistant("done eventually");
        let theme = crate::theme::Theme::default();
        let display = crate::config::DisplayConfig::default();
        let mut cache = LogBlockCache::default();

        let layout1 = build_log_layout_with_expansion(
            &[],
            &[user.clone(), tool, assistant.clone()],
            0,
            false,
            80,
            &cfg(),
            &theme,
            &display,
            &std::collections::HashSet::new(),
            &mut cache,
        );
        let entries_after_first = cache.len();

        // Update the non-tail tool's live output; user and assistant blocks
        // must be reused, only the tool re-renders.
        let mut tool2 = Message::tool_call("c1", "bash", serde_json::json!({"command": "seq 5"}));
        tool2.tool_running_output = Some("1\n2\n3".to_string());
        let layout2 = build_log_layout_with_expansion(
            &[],
            &[user, tool2, assistant],
            0,
            false,
            80,
            &cfg(),
            &theme,
            &display,
            &std::collections::HashSet::new(),
            &mut cache,
        );
        assert_eq!(
            cache.len(),
            entries_after_first,
            "cache stays bounded (changed tool overwrites its own entry)"
        );
        assert_eq!(
            layout1.blocks.len(),
            layout2.blocks.len(),
            "block count stable"
        );
    }

    #[test]
    fn trim_assistant_block_edges_hides_outer_whitespace() {
        let rendered = trim_assistant_block_edges("\n  hello\n\n");
        assert_eq!(rendered, "hello");
    }

    #[test]
    fn trim_assistant_block_edges_preserves_interior_whitespace() {
        let rendered = trim_assistant_block_edges("\n\nfirst\n\n\nlast\n\n");
        assert_eq!(rendered, "first\n\n\nlast");
    }

    #[test]
    fn tool_intent_and_result_share_one_visual_block() {
        let messages = vec![
            Message::tool_call("call-1", "bash", serde_json::json!({"command": "sleep 5"})),
            Message::tool_result("call-1", "", false),
        ];
        let layout = build_log_layout(
            &messages,
            true,
            80,
            &ToolBodyConfig::default(),
            &Theme::default(),
            &DisplayConfig::default(),
        );
        assert_eq!(layout.blocks.len(), 1);
        assert_eq!(layout.blocks[0].identity, "message:0:tool");
    }

    #[test]
    fn expanded_tool_block_shows_full_body() {
        let call = Message::tool_call("c1", "bash", serde_json::json!({"command": "seq 20"}));
        let content = (1..=20)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let mut expanded = std::collections::HashSet::new();
        expanded.insert("message:0:tool".to_string());
        let layout = build_log_layout_with_expansion(
            &[],
            &[call, result],
            0,
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
            &expanded,
            &mut LogBlockCache::default(),
        );
        let text = layout
            .flatten()
            .0
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(
            !text.iter().any(|t| t.contains("total lines")),
            "expanded tool block must not show a truncation marker"
        );
    }

    #[test]
    fn build_log_layout_hides_whitespace_only_streaming_assistant() {
        let mut msg = Message::assistant("\n   \n".to_string());
        msg.assistant_phase = Some(AssistantPhase::Provisional);
        let lines = build_log_layout(
            &[msg],
            true,
            80,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        assert!(lines.is_empty());
    }

    #[test]
    fn expanded_thinking_shows_earlier_lines() {
        let mut msg = Message::assistant("answer".to_string());
        msg.thinking = Some(
            (1..=8)
                .map(|i| format!("thought{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let mut expanded = std::collections::HashSet::new();
        expanded.insert("message:0:thinking".to_string());
        let layout = build_log_layout_with_expansion(
            &[],
            &[msg],
            0,
            false,
            80,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
            &expanded,
            &mut LogBlockCache::default(),
        );
        let text = layout.blocks[0]
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("thought1"));
    }

    // ── read_file ─────────────────────────────────────────────────────────────

    #[test]
    fn read_file_result_head_truncated_to_8_lines() {
        let call = { Message::tool_call("c1", "read_file", serde_json::json!({"path": "foo.rs"})) };
        let content = (1..=20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        // 8 content lines + 1 marker = 9 body lines, plus 1 intent line = 10 total
        assert_eq!(lines.len(), 10);
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.last().unwrap().contains("20 total lines"));
    }

    #[test]
    fn read_file_result_no_truncation_marker_when_within_limit() {
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "foo.rs"}));
        let content = "line1\nline2\nline3";
        let result = Message::tool_result("c1", content, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(!text.iter().any(|t| t.contains("total lines")));
    }

    #[test]
    fn read_file_range_suffix_shown_when_display_range_present() {
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "foo.rs"}));
        let mut result = Message::tool_result("c1", "content", false);
        result.display_range = Some(DisplayRange {
            first_line: 1,
            last_line: 5,
            total_lines: 100,
        });
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let intent = lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(
            intent.contains("[1-5/100]"),
            "expected range suffix, got: {intent}"
        );
    }

    // ── find_files ────────────────────────────────────────────────────────────

    #[test]
    fn find_files_result_head_truncated() {
        let call = Message::tool_call("c1", "find_files", serde_json::json!({"pattern": "*.rs"}));
        let content = (1..=12)
            .map(|i| format!("src/file{i}.rs"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.last().unwrap().contains("12 total lines"));
    }

    // ── edit_file diff ────────────────────────────────────────────────────────

    #[test]
    fn edit_file_renders_diff_body() {
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": "old line", "new_text": "new line"}),
        );
        let result = Message::tool_result("c1", "Successfully edited foo.rs", false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.iter().any(|t| t.contains("old line")),
            "expected old line"
        );
        assert!(
            text.iter().any(|t| t.contains("new line")),
            "expected new line"
        );
    }

    #[test]
    fn edit_file_diff_truncated_per_side() {
        let old = (1..=6)
            .map(|i| format!("old{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new = (1..=6)
            .map(|i| format!("new{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": old, "new_text": new}),
        );
        let result = Message::tool_result("c1", "ok", false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        // Two truncation markers: one per side
        let marker_count = text.iter().filter(|t| t.contains("total lines")).count();
        assert_eq!(marker_count, 2);
    }

    #[test]
    fn edit_file_pure_addition_no_common_lines_placeholders() {
        // Pure addition: old_text=prefix, new_text=prefix+new_line.
        // Common-lines placeholders must NOT appear — only the green added line.
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": "prefix\n", "new_text": "prefix\nnew line\n"}),
        );
        let result = Message::tool_result("c1", "ok", false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            !text.iter().any(|t| t.contains("common lines")),
            "pure addition should NOT show common-lines placeholders; got: {text:?}"
        );
        assert!(
            text.iter().any(|t| t.contains("new line")),
            "should show added line"
        );
    }

    #[test]
    fn edit_file_pure_removal_no_common_lines_placeholders() {
        // Pure removal: old_text=prefix+old_line, new_text=prefix.
        // Common-lines placeholders must NOT appear — only the red removed line.
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": "prefix\nold line\n", "new_text": "prefix\n"}),
        );
        let result = Message::tool_result("c1", "ok", false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            !text.iter().any(|t| t.contains("common lines")),
            "pure removal should NOT show common-lines placeholders; got: {text:?}"
        );
        assert!(
            text.iter().any(|t| t.contains("old line")),
            "should show removed line"
        );
    }

    #[test]
    fn edit_file_error_shows_plain_content() {
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "foo.rs", "old_text": "x", "new_text": "y"}),
        );
        let result = Message::tool_result("c1", "old_text not found", true);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.iter().any(|t| t.contains("old_text not found")));
        assert!(!text.iter().any(|t| t.starts_with("- ")));
    }

    // ── bash tail truncation ──────────────────────────────────────────────────

    #[test]
    fn bash_result_tail_truncated() {
        let call = Message::tool_call("c1", "bash", serde_json::json!({"command": "seq 20"}));
        let content = (1..=20)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        // Marker should be first body line (tail-truncated)
        let body: Vec<&String> = text.iter().skip(1).collect();
        assert!(
            body[0].contains("20 total lines"),
            "expected marker first, got: {}",
            body[0]
        );
        assert!(
            body.last().unwrap().contains("20"),
            "expected last line to be 20"
        );
    }

    // ── python result tail truncation ──────────────────────────────────────────

    #[test]
    fn python_result_tail_truncated() {
        let call = Message::tool_call(
            "c1",
            "run_python",
            serde_json::json!({"script": "for i in range(1, 21): print(i)"}),
        );
        let content = (1..=20)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        // Body starts after the headline line.
        let body: Vec<&String> = text.iter().skip(1).collect();
        assert!(
            body[0].contains("20 total lines"),
            "expected tail-truncated marker first, got: {}",
            body[0]
        );
        assert!(
            body.last().unwrap().contains("20"),
            "expected last visible line to be 20, got: {}",
            body.last().unwrap()
        );
    }

    // ── full_output toggle ────────────────────────────────────────────────────

    #[test]
    fn full_output_disables_truncation() {
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "foo.rs"}));
        let content = (1..=20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = Message::tool_result("c1", &content, false);
        let full_cfg = ToolBodyConfig {
            full_output: true,
            ..ToolBodyConfig::default()
        };
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &full_cfg,
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(!text.iter().any(|t| t.contains("total lines")));
        // 20 content lines + 1 intent = 21
        assert_eq!(lines.len(), 21);
    }

    // ── ask_user ──────────────────────────────────────────────────────────────

    #[test]
    fn ask_user_renders_while_pending() {
        let call = Message::tool_call(
            "c1",
            "ask_user",
            serde_json::json!({"question": "What do you want?"}),
        );
        // Question always renders in the log body.
        let lines = build_log_layout(
            &[call],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.iter().any(|t| t.contains("What do you want?")),
            "question should be visible in the log"
        );
    }

    #[test]
    fn ask_user_renders_after_answer() {
        let call = Message::tool_call(
            "c1",
            "ask_user",
            serde_json::json!({"question": "What do you want?"}),
        );
        let result = Message::tool_result("c1", "Option A", false);
        // Committed turn: question should appear in the log.
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.iter().any(|t| t.contains("What do you want?")),
            "question should appear in committed log"
        );
        assert!(
            text.iter().any(|t| t.contains("Option A")),
            "response not rendered"
        );
    }

    #[test]
    fn ask_user_renders_question_from_partial_snapshot_during_streaming() {
        // During streaming, tool_args is empty but tool_partial_snapshot has
        // the question. The question must render in the log.
        let mut call = Message {
            role: Role::ToolCall,
            tool_call_id: Some("c1".to_string()),
            tool_name: Some("ask_user".to_string()),
            tool_args: Some(serde_json::json!({})), // empty — still streaming
            tool_partial_snapshot: Some(serde_json::json!({
                "question": "What do you think?"
            })),
            tool_streaming_field: Some("question".to_string()),
            ..Message::default()
        };
        call.role = Role::ToolCall;
        let lines = build_log_layout(
            &[call],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.iter().any(|t| t.contains("What do you think?")),
            "question from partial_snapshot should be visible in the log:\n{}",
            text.join("\n")
        );
    }

    #[test]
    fn ask_user_renders_context_from_partial_snapshot_during_streaming() {
        // During streaming, tool_args is empty but tool_partial_snapshot has
        // the context. The context must render in the log.
        let mut call = Message {
            role: Role::ToolCall,
            tool_call_id: Some("c1".to_string()),
            tool_name: Some("ask_user".to_string()),
            tool_args: Some(serde_json::json!({})), // empty — still streaming
            tool_partial_snapshot: Some(serde_json::json!({
                "question": "Proceed?",
                "context": "Summary: we found the bug."
            })),
            tool_streaming_field: Some("question".to_string()),
            ..Message::default()
        };
        call.role = Role::ToolCall;
        let lines = build_log_layout(
            &[call],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.iter()
                .any(|t| t.contains("Summary: we found the bug.")),
            "context from partial_snapshot should be visible in the log:\n{}",
            text.join("\n")
        );
        assert!(
            text.iter().any(|t| t.contains("Proceed?")),
            "question should also be visible"
        );
    }

    // ── Regression: finalized write_file headline shows path, not placeholder ─

    #[test]
    fn write_file_finalized_headline_shows_path() {
        // When a write_file tool call has complete args (ToolCallStart
        // arrived, partial_args cleared), the headline must show the path,
        // not the italic "📄 writing…" placeholder.
        let call = Message::tool_call(
            "c1",
            "write_file",
            serde_json::json!({"path": "/tmp/out.rs", "content": "fn main() {}"}),
        );
        let result = Message::tool_result("c1", "Written 1 lines to /tmp/out.rs", false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        // The first line is the headline.
        let headline: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            headline.contains("/tmp/out.rs"),
            "finalized write_file headline must show path, got: {headline}"
        );
        assert!(
            !headline.contains("writing…"),
            "finalized headline must not be a placeholder, got: {headline}"
        );
    }

    // ── Wrapped-line truncation (regression: very long logical lines) ────────

    /// Helper: collect all text from rendered lines as a single string for inspection.
    fn lines_text_joined(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn head_truncation_limits_wrapped_lines_not_logical() {
        // Two logical lines, each wrapping to ~five visual lines at width 20.
        // max_lines = 8 visual lines → only 8 wrapped lines shown, truncation
        // marker present.
        let long_line = "x".repeat(100); // ~5 wrapped lines at width 20
        let content = format!("{}\n{}", long_line, long_line);
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", &content, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            20, // narrow terminal → forces wrapping
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        // Lines: 1 headline + up to 8 body lines + 1 truncation marker = max 10
        let body_text = lines_text_joined(&lines);
        assert!(
            body_text.contains("2 total lines"),
            "expected truncation marker, got:\n{body_text}"
        );
        // Count body lines (exclude headline — first line contains the tool icon).
        let body_start = 1; // skip headline
        let body_end = lines.len();
        let body_count = body_end - body_start;
        assert!(
            body_count <= 9, // 8 wrapped lines + 1 marker
            "too many body lines ({body_count}), expected ≤ 9:\n{body_text}"
        );
    }

    #[test]
    fn tail_truncation_limits_wrapped_lines_not_logical() {
        // Two logical lines, each wrapping to ~five visual lines at width 20.
        let long_line = "x".repeat(100);
        let content = format!("{}\n{}", long_line, long_line);
        let call = Message::tool_call("c1", "bash", serde_json::json!({"command": "echo"}));
        let result = Message::tool_result("c1", &content, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            20,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let body_text = lines_text_joined(&lines);
        assert!(
            body_text.contains("2 total lines"),
            "expected truncation marker, got:\n{body_text}"
        );
    }

    #[test]
    fn head_truncation_no_marker_when_wrapped_lines_fit() {
        // Short content: all wrapped lines fit within the limit.
        let content = "short line";
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", content, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let body_text = lines_text_joined(&lines);
        assert!(
            !body_text.contains("total lines"),
            "unexpected truncation marker for short content:\n{body_text}"
        );
    }

    #[test]
    fn tail_truncation_no_marker_when_wrapped_lines_fit() {
        let content = "short output";
        let call = Message::tool_call("c1", "bash", serde_json::json!({"command": "echo"}));
        let result = Message::tool_result("c1", content, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            120,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let body_text = lines_text_joined(&lines);
        assert!(
            !body_text.contains("total lines"),
            "unexpected truncation marker:\n{body_text}"
        );
    }

    #[test]
    fn head_truncation_single_long_logical_line_is_bounded() {
        // One very long logical line → should be capped at max_lines wrapped chunks.
        let long_line = "x".repeat(500); // ~seven wrapped lines at width 80
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", long_line, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            80,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let body_text = lines_text_joined(&lines);
        // Should have truncation marker since head_lines=8 but the line wraps
        // to ~7 chunks (< 8 at width 80), so actually no truncation for 500
        // chars at width 80.  Let's verify: at width=80, content_width=77,
        // 500/77 ≈ 7 chunks → fits within 8.
        // Use narrower width to force truncation.
        assert!(
            !body_text.contains("total lines"),
            "500 chars at width 80 should fit in 8 wrapped lines:\n{body_text}"
        );
    }

    #[test]
    fn head_truncation_single_very_long_line_is_truncated() {
        // One very long logical line at narrow width → many wrapped chunks → must truncate.
        let long_line = "x".repeat(500);
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", long_line, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            20, // narrow → ~26 wrapped chunks (500/17 ≈ 30)
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let body_text = lines_text_joined(&lines);
        assert!(
            body_text.contains("1 total lines"),
            "expected truncation marker for single long line at narrow width:\n{body_text}"
        );
    }

    #[test]
    fn tail_truncation_single_long_line_is_bounded() {
        let long_line = "x".repeat(500);
        let call = Message::tool_call("c1", "bash", serde_json::json!({"command": "echo"}));
        let result = Message::tool_result("c1", long_line, false);
        let lines = build_log_layout(
            &[call, result],
            false,
            20,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let body_text = lines_text_joined(&lines);
        assert!(
            body_text.contains("1 total lines"),
            "expected truncation marker for long line in tail mode:\n{body_text}"
        );
    }

    #[test]
    fn diff_body_removed_block_limited_by_wrapped_lines() {
        // old_text has one very long line that wraps many times at width 20.
        // diff_lines default = 4; should limit to 4 wrapped visual lines.
        // There's only 1 logical line, so no logical truncation marker is
        // expected — but the visual display is still bounded.
        let old_long = "r".repeat(200);
        let new = "a";
        let call = Message::tool_call(
            "c1",
            "edit_file",
            serde_json::json!({"path": "f", "old_text": old_long, "new_text": new}),
        );
        let result = Message::tool_result("c1", "ok", false);
        let lines = build_log_layout(
            &[call, result],
            false,
            20,
            &cfg(),
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let body_text = lines_text_joined(&lines);
        // Count how many body lines contain the removed marker pattern " │ r".
        // Should be exactly 4 (diff_lines limit), not ~12 (the full wrapped count).
        let removed_line_count = body_text.lines().filter(|l| l.contains("│ rrr")).count();
        assert_eq!(
            removed_line_count, 4,
            "expected exactly 4 wrapped lines of 'r's (diff_lines limit), got {removed_line_count}:\n{body_text}"
        );
        // The new_text 'a' should also appear.
        assert!(
            body_text.contains("│ a"),
            "expected new_text 'a' in diff output:\n{body_text}"
        );
    }

    #[test]
    fn full_output_disables_wrapped_truncation() {
        let long_line = "x".repeat(500);
        let call = Message::tool_call("c1", "read_file", serde_json::json!({"path": "f"}));
        let result = Message::tool_result("c1", long_line, false);
        let cfg_full = ToolBodyConfig {
            full_output: true,
            ..ToolBodyConfig::default()
        };
        let lines = build_log_layout(
            &[call, result],
            false,
            20,
            &cfg_full,
            &crate::theme::Theme::default(),
            &crate::config::DisplayConfig::default(),
        )
        .flatten()
        .0;
        let body_text = lines_text_joined(&lines);
        assert!(
            !body_text.contains("total lines"),
            "full_output must not truncate:\n{body_text}"
        );
    }

    // ── Streaming diff stability regression ───────────────────────────────────

    /// Simulate the write-edit streaming chunk-by-chunk (as
    /// `streaming_tool_call` does: 4-8 chars alternating).  At each chunk,
    /// complete the partial JSON with `jawohl`, extract `old_text` / `new_text`,
    /// render the diff body, and verify two invariants:
    ///
    /// 1. The first diff body line must be either `context line 1` (the first
    ///    shared-context line) or a `… N common lines` placeholder — **never**
    ///    a mid-context line without the annotation.
    ///
    /// 2. The diff body changes shape at least once during streaming (the flicker).
    #[test]
    fn streaming_edit_diff_flickers_when_old_text_arrives_before_new_text() {
        // Real write-edit content: 6 lines of shared context above, one
        // differing target line, 6 lines of shared context below.
        let shared_head = "\
context line 1: The quick brown fox\n\
context line 2: jumps over the lazy dog\n\
context line 3: Pack my box with five\n\
context line 4: dozen liquor jugs\n\
context line 5: How vexingly quick\n\
context line 6: daft zebras jump\n\
";
        let shared_tail = "\
context line 7: The five boxing wizards\n\
context line 8: jump quickly\n\
context line 9: Sphinx of black quartz\n\
context line 10: judge my vow\n\
context line 11: Waltz nymph for quick\n\
context line 12: jigs vex bud\n\
";
        let full_old =
            format!("{shared_head}TARGET LINE: original content to be replaced\n{shared_tail}");
        let full_new =
            format!("{shared_head}TARGET LINE: replaced content — edit succeeded!\n{shared_tail}");

        // Build the exact JSON that write_edit_edit_stream emits, then chunk it
        // the same way streaming_tool_call does (4-8 chars alternating).
        let full_json = serde_json::to_string(&serde_json::json!({
            "path": "/tmp/test.txt",
            "old_text": full_old,
            "new_text": full_new,
        }))
        .unwrap();
        let chunks: Vec<String> = {
            let bytes = full_json.as_bytes();
            let mut pos = 0;
            let mut chunk_size = 4usize;
            let mut v = Vec::new();
            while pos < bytes.len() {
                let end = (pos + chunk_size).min(bytes.len());
                let end = (pos..=end)
                    .rev()
                    .find(|&i| full_json.is_char_boundary(i))
                    .unwrap_or(end);
                v.push(full_json[pos..end].to_string());
                pos = end;
                chunk_size = if chunk_size == 4 { 8 } else { 4 };
            }
            v
        };

        // Helper: render the diff body given old_text / new_text strings.
        let render_body = |old: &str, new: &str| -> String {
            let mut msg = Message {
                role: Role::ToolCall,
                tool_call_id: Some("c1".to_string()),
                tool_name: Some("edit_file".to_string()),
                tool_args: None,
                tool_partial_snapshot: Some(serde_json::json!({
                    "path": "/tmp/test.txt",
                    "old_text": old,
                    "new_text": new,
                })),
                ..Message::default()
            };
            msg.role = Role::ToolCall;
            let lines = build_log_layout(
                std::slice::from_ref(&msg),
                false,
                120,
                &cfg(),
                &crate::theme::Theme::default(),
                &crate::config::DisplayConfig::default(),
            )
            .flatten()
            .0;
            if lines.len() <= 1 {
                return String::new();
            }
            lines[1..]
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut prev_body: Option<String> = None;
        let mut flickered = false;
        let mut partial = String::new();

        for (_i, chunk) in chunks.iter().enumerate() {
            partial.push_str(chunk);

            // Mimic on_tool_call_args_delta: complete + parse partial JSON.
            let (old_str, new_str) = if let Ok(completed) = jawohl::complete_json(&partial)
                && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&completed)
            {
                (
                    parsed
                        .get("old_text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    parsed
                        .get("new_text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            } else {
                continue; // JSON not yet completable
            };

            if old_str.is_empty() && new_str.is_empty() {
                continue; // no fields available yet
            }

            let body = render_body(&old_str, &new_str);
            if body.is_empty() {
                continue;
            }

            // ── Invariant: once old_text contains at least one full line, the
            //    first diff body line must contain either "context line 1"
            //    (the first shared-context line) or "common lines"
            //    (a "… N common lines" placeholder).
            //
            //    Exception: when the diff looks like a pure addition or pure
            //    removal (one side has zero diff lines), the common-lines
            //    placeholder is intentionally suppressed so only the changed
            //    lines are shown.  In that streaming-intermediate state the
            //    first line may legitimately be a later context line. ──
            if old_str.contains('\n') {
                let first_line = body.lines().next().unwrap_or("");
                let is_pure = !body.contains("common lines");
                assert!(
                    first_line.contains("context line 1")
                        || first_line.contains("common lines")
                        || is_pure,
                    "chunk {_i}: first diff body line must contain 'context line 1' or 'common lines', \
                     or the diff must be a pure addition/removal (no common-lines placeholders)\n\
                     old_text  len={}  new_text len={}\n\
                     first_line: {first_line}\n\
                     body:\n{body}",
                    old_str.len(),
                    new_str.len(),
                );
            }

            // Track whether the body shape actually changes (the flicker).
            if let Some(ref prev) = prev_body
                && prev != &body
            {
                flickered = true;
            }
            prev_body = Some(body);
        }

        assert!(
            flickered,
            "diff body never changed during streaming — flicker not reproduced"
        );

        // Sanity: final diff contains the expected target lines.
        let final_body = prev_body.unwrap();
        assert!(
            final_body.contains("original content to be replaced"),
            "final diff must contain old target line:\n{final_body}"
        );
        assert!(
            final_body.contains("replaced content — edit succeeded!"),
            "final diff must contain new target line:\n{final_body}"
        );
    }
}
