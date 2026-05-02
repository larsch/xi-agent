# Plan: Tool output rendering fixes

Date: 2026-05-02

## Context

Gap analysis against `UI-LAYOUT-SPEC.md` identified 11 gaps between the spec
and the current implementation. This plan addresses all of them. It also
prepares the rendering layer for future configurability of truncation lengths
and toggling of full (untruncated) display.

---

## Scope

### In scope

1. Introduce a `ToolBodyConfig` struct that centralises all UI truncation
   parameters, replacing the scattered magic constants.
2. Replace the 200-char generic tool result preview in `ui/log.rs` with
   per-tool line-based rendering with correct head/tail direction and
   `... (N lines total)` truncation markers.
3. Implement `edit_file` compact diff body (up to 4 lines each side, `-`/`+`
   prefix, red/green, per-side `... (N lines total)`).
4. Fix `ask_user` rendering: suppress output-area rendering while pending;
   commit context + question + response as a unified block with correct
   background colors after the answer.
5. Fix `ask_user` icon assignment: `❓` on the question, not the context.
6. Fix `read_file` streaming range suffix: show `[first-last]` during
   streaming, `[first-last/total]` after result.
7. Fix `find_files` intent: render `in <path>` suffix when path is present.
8. Add `... (N lines total)` truncation marker globally, replacing bare `…`.
9. Make `exec` explicit in `tool_emoji`.
10. Fix user prompt background color (`#32323c` → `#323240`).
11. Fix `bash` streaming truncation marker direction (minor).
12. Add a `full_output` toggle to `LogViewState` so the cache is invalidated
    and all tool bodies render untruncated when active. Wire it to a keybind
    (TBD, suggested `F` or `Ctrl+F` when not in input focus).

### Out of scope

- Streaming `write_file`/`edit_file` tail-advancing window — the spec describes
  the desired behaviour but the current implementation does not stream the
  body at all during LLM argument generation. Fixing that is a separate task.
- Indicator area spec (marked TBD).
- Config-file-level truncation customisation (future; the struct introduced
  here makes it straightforward to add later).

---

## Affected files

- `src/ui/log.rs` — primary rendering changes
- `src/tool_presentation.rs` — `find_files` intent, `exec` emoji, truncation helpers
- `src/log_view_state.rs` — add `full_output` toggle
- `src/ui.rs` — pass `ToolBodyConfig` into `build_log_lines`, wire keybind
- `src/config.rs` — no changes yet (deferred)

---

## Step-by-step

### Step 1 — Introduce `ToolBodyConfig`

Add to `src/ui/log.rs` (or a new `src/ui/tool_body.rs`):

```rust
/// Display configuration for tool body rendering.
///
/// All line-count limits apply to the visible window; when a body exceeds
/// the limit the overflow is replaced by a `... (N lines total)` marker.
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
    /// Max lines shown for shell command intent (bash/cmd/powershell).
    pub intent_shell_lines: usize,
}

impl Default for ToolBodyConfig {
    fn default() -> Self {
        Self {
            full_output: false,
            head_lines: 8,
            tail_lines: 8,
            diff_lines: 4,
            intent_shell_lines: 5,
        }
    }
}
```

Change `build_log_lines` signature to accept `&ToolBodyConfig`:

```rust
pub fn build_log_lines(
    messages: &[Message],
    streaming: bool,
    width: usize,
    cfg: &ToolBodyConfig,
) -> Vec<Line<'static>>
```

All existing call sites pass `&ToolBodyConfig::default()` initially.

---

### Step 2 — Per-tool line-based tool result rendering

Replace the 200-char block in `Role::ToolResult` in `ui/log.rs` with a
dispatch on the preceding tool name:

```rust
fn render_tool_result(
    msgs: &[Message],
    idx: usize,
    cfg: &ToolBodyConfig,
    width: usize,
    out: &mut Vec<Line<'static>>,
)
```

Per-tool behaviour:

| Tool | Direction | Lines | Notes |
|------|-----------|-------|-------|
| `read_file` | head | `cfg.head_lines` | plain content |
| `write_file` | head | `cfg.head_lines` | shows written content from `tool_args["content"]` |
| `find_files` | head | `cfg.head_lines` | one path per line |
| `bash`/`cmd`/`powershell` | tail | `cfg.tail_lines` | plain content |
| `exec` | tail | `cfg.tail_lines` | plain content |
| `edit_file` | special | `cfg.diff_lines` × 2 | see step 3 |
| `ask_user` | — | — | handled separately |
| `local_shell` | tail | `cfg.tail_lines` | existing colour treatment |
| custom/unknown | tail | `cfg.tail_lines` | plain content |

Truncation marker format: `... (N lines total)` on its own line, using the
tool result's existing color. Marker position:
- Head-truncated: marker is last line (after shown content)
- Tail-truncated: marker is first line (before shown content)

---

### Step 3 — `edit_file` compact diff body

When the preceding tool is `edit_file`, render the result as a compact diff
rather than raw content.

The `edit_file` tool args contain `old_text` and `new_text`. These are
available on the `ToolCall` message at `tool_args`. Read them from
`messages[idx - 1].tool_args`.

Render as:

```
- <old line 1>
- <old line 2>
...
... (N lines total)
+ <new line 1>
+ <new line 2>
...
... (N lines total)
```

- Old lines: red foreground, `-` prefix
- New lines: green foreground, `+` prefix
- Each side independently truncated to `cfg.diff_lines` lines
- Each side gets its own `... (N lines total)` marker when truncated
- If the tool result itself is an error (e.g. "old_text not found"), render
  the error message as plain content instead of a diff

---

### Step 4 — Fix `ask_user` rendering

**Output area — `Role::ToolCall` for `ask_user`:**

Do not render anything in the output area for `ask_user` while the tool result
is not yet present. Specifically: skip rendering the `ask_user` ToolCall row if
the next message is not a `ToolResult` for the same call.

When the result is present (i.e. `messages[idx + 1]` is the matching
`ToolResult`), render the full exchange immediately in the ToolCall handler
(and skip it again in the ToolResult handler):

1. Context (if present): background `#1b471f`, dimmed text, no icon
2. Question: background `#1b471f`, normal text, `❓` icon  
3. Response: background `#1b471f`, italicized text, no icon

**Icon fix:** currently context gets `❓` and question gets no icon. Swap: icon
goes on the question line only.

**`Role::ToolResult` for `ask_user`:** skip rendering (already handled above).

---

### Step 5 — `read_file` streaming range suffix

**Problem:** `[first-last]` requires first/last line numbers before the result
arrives. These are only available on `display_range` (result side).

**Solution:** during streaming the live tool entry has no result yet so no
range suffix is shown — render as `👀 <path>` only. This is already the current
behaviour. No new data source is needed for the streaming phase.

The spec says `[first-last]` during streaming — revisit this. Since `first` and
`last` are not knowable until the result arrives, the streaming form is simply
`👀 <path>`. The intent line then updates to `👀 <path> [first-last/total]`
when the result is available. This is one of the permitted stability exceptions.

Update spec accordingly (minor spec correction, not an implementation gap).

**In `ui/log.rs`:** the current range suffix logic reads `display_range` from
the next message. This is correct for the committed case. No change needed
here — the gap was in the spec, not the implementation.

---

### Step 6 — `find_files` intent `in <path>` suffix

In `tool_presentation.rs`, update `tool_detail` for `find_files`:

```rust
if name == "find_files" {
    let pattern = args.get("pattern").and_then(|v| v.as_str());
    let path = args.get("path").and_then(|v| v.as_str());
    return match (pattern, path) {
        (Some(p), Some(d)) => format!("{} in {}", compact(p), d),
        (Some(p), None) => compact(p),
        (None, Some(d)) => format!("in {}", d),
        (None, None) => String::new(),
    };
}
```

Also update `tool_invocation_label_partial` to handle the partial case for
`find_files` (currently falls through to generic field extraction which only
shows pattern, not path).

---

### Step 7 — Truncation marker format

Replace bare `…` truncation markers in `tool_presentation.rs`
(`head_truncate`, `tail_truncate`, `multiline_shell_command`) with
`... (N lines total)` format, consistent with the global convention.

The line count `N` is the total original line count. Where total is not known
(e.g. partial args during streaming), use `…` as a fallback until total is
available.

Update `multiline_shell_command` to accept total line count and emit
`... (N lines total)` as the last line when truncated.

---

### Step 8 — `exec` in `tool_emoji`, user bg color, minor fixes

- Add explicit `"exec" => "⚙️"` arm to `tool_emoji` match.
- Fix `USER_BG = Color::Rgb(50, 50, 64)` (hex `#323240`). Current value is
  `Rgb(50, 50, 60)` = `#32323c`.
- Review `bash` streaming truncation: `head_truncate` (used during streaming)
  shows tail lines with `…` prefix. This is intentionally a tail-advancing
  window. The marker direction can remain as-is; only update the marker text
  to `... (N lines total)` format per step 7.

---

### Step 9 — `full_output` toggle

Add to `LogViewState`:

```rust
/// When true, tool bodies are rendered without truncation.
pub full_output: bool,
```

In `build_log_lines_cached` in `ui.rs`, derive `ToolBodyConfig` from
`app.log_view.full_output`:

```rust
let cfg = ToolBodyConfig {
    full_output: app.log_view.full_output,
    ..ToolBodyConfig::default()
};
```

Add a keybind to toggle `log_view.full_output` and invalidate the cache.
Suggested key: `f` when not in text input focus (consistent with common
pager conventions). Exact key TBD — check for conflicts.

When `full_output` is active, a visible indicator should appear (e.g. in the
indicator area or status bar) so the user knows they are in expanded mode.

---

## Risks

- The `ask_user` rendering change alters observable output-area behaviour.
  Needs careful testing for the live (pending) case and the committed case.
- The `edit_file` diff reads `tool_args` from the ToolCall message. This is
  available in committed state but may not be present in all edge cases (e.g.
  sessions loaded from disk where args were stripped). Handle gracefully by
  falling back to plain content if args unavailable.
- `ToolBodyConfig` is introduced as an internal struct. It is not yet
  serialised or user-configurable. Keep it internal until the config story
  is decided.

---

## Step 10 — Extend the test provider

The test provider (`src/llm/test_provider.rs`) must be extended so every
rendering path can be exercised interactively without a real API connection.

Currently missing: `read_file`, `find_files`, `edit_file`.

Add the following commands:

| Command | What it exercises |
|---------|-------------------|
| `read` | `read_file` on a small built-in fixture (≤8 lines) — full body, no truncation, no range suffix |
| `read-long` | `read_file` on a fixture >8 lines — head-truncation, `... (N lines total)`, range suffix `[first-last/total]` |
| `find` | `find_files` on the temp directory — list body rendering, head-truncation when >8 results |
| `edit` | `edit_file` on the temp file written by `write` — compact diff body (normal case, both sides ≤4 lines) |
| `edit-long` | `edit_file` with old/new text exceeding 4 lines per side — per-side `... (N lines total)` truncation |

### Implementation notes

- `read` and `read-long` should use in-memory fixture strings as the file
  content, written to a temp file immediately before issuing the tool call, so
  no real file path is required from the user.
- `read-long` fixture should be at least 20 lines so truncation is clearly
  triggered.
- `edit` should target the same temp file path used by `write`
  (`tau-test-write.txt` in the system temp dir) so the two commands can be
  run in sequence.
- `edit-long` should use a fixture with ≥6 lines of old text and ≥6 lines of
  new text to clearly exercise per-side truncation.
- For `find`, issuing the tool call against the system temp directory is
  sufficient — the result will contain at least the files written by `write`.
- The HELP_TEXT constant and the markdown commands table should be updated to
  include all new commands.

---

## Verification

- `just preflight` passes.
- Manual test using the test provider (`--provider test`):
  - `read` — full body, no truncation
  - `read-long` — truncated body with `... (N lines total)`, range suffix
  - `find` — list body
  - `write` then `edit` — compact diff, both sides within limit
  - `edit-long` — compact diff with per-side truncation markers
  - `bash <long command>` — tail-truncated body with `... (N lines total)`
  - `exec ls /tmp` — exec body rendering
  - `ask`, `ask-type`, `ask-notype` — all three ask_user variants
  - Toggle `full_output` on each of the above — verify bodies expand
- Existing `ui.rs` and `tool_presentation.rs` tests continue to pass; add new
  tests for:
  - `render_tool_result` per-tool dispatch
  - `edit_file` diff rendering (normal, truncated, error fallback)
  - `find_files` intent with pattern+path, pattern only, path only
  - `... (N lines total)` marker format and position

