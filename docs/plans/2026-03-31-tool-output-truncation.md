# Tool Output Truncation

## Problem

Shell tool calls (`bash`, `cmd`, `powershell`) and other tools can produce
arbitrarily large output. Two consumers need to handle this:

1. **The model** — receives tool results as message content. Context windows are
   finite; enormous outputs consume expensive tokens and can crowd out history.
2. **The UI** — renders tool results in the chat log. Only a short preview is
   ever useful; rendering many thousands of lines is wasteful and slow.

These two consumers have different requirements and should be handled
independently. Metadata that the UI needs (line ranges, truncation flags) must
flow through the data pipeline as **structured fields**, not be embedded in the
content string and re-parsed on the other side.

---

## Goals

- Protect the model's context window from runaway tool output.
- Keep the UI responsive regardless of output size.
- Never silently discard output — always tell the model (and the user) when
  truncation has occurred, what was kept, and where the full output lives.
- Give the model enough information in prose to request more output when needed.
- **Never parse self-generated text** to recover structured data — use typed
  fields end-to-end instead.
- Keep the implementation simple and well-tested.

---

## Provider Compatibility Note

An alternative to prose notices would be to use provider-native structured tool
result APIs — for example, Anthropic supports an array of typed content blocks
inside `tool_result`. In principle, truncation metadata could arrive as a
second distinct block rather than being embedded in the output text.

This was considered and rejected for the following reasons:

- **Only Anthropic supports content-block arrays.** OpenAI, Gemini, and Ollama
  all reduce tool results to a single flat string. A structured approach would
  require per-provider divergence that cannot be abstracted at the `Message`
  level.
- **The block types are still untyped text.** Even Anthropic's content blocks
  are `{"type": "text", "text": "…"}`. There is no provider API for a typed
  `truncation_notice` block; the metadata would still be prose inside a block.
- **The self-parsing problem is already solved the right way.** The prose notice
  is genuinely only for the model to read. Structured metadata for the UI flows
  through `DisplayRange` on `Message`, completely bypassing `content`. No code
  parses the prose notice back.
- **Prose is more actionable for the model.** "Showing lines 1001–3000 of 4500.
  Full output saved to /tmp/…" is directly usable by the model without any
  special formatting convention.

The "no self-parsing" principle applies to our own code reading back what it
wrote — not to human-readable text sent to the model.

---

## Core Principle: No Self-Parsing

A tempting shortcut is to embed structured information as formatted text inside
`content` (e.g. a `[lines X-Y of Z]` header) and then parse it back out in the
UI. This creates fragile coupling between the writer and reader, makes the
content harder to read, and means the format must be kept in sync with the
parser's expectations.

### Current violations to fix

**`read_file`** — embeds `[lines X-Y of Z]\n` as a literal prefix inside
`content`. `build_log_lines` in `ui.rs` calls `split_read_file_header()` to
parse it back out: once to strip it from the preview, and again to extract the
range numbers for the `[10-20/300]` annotation on the tool-call row.

**`bash` truncation notice** — the agent loop currently appends
`[Showing lines N-M of TOTAL. Full output … in PATH]` to the content string.
The notice is useful prose for the model, but the UI should not need to parse
it to learn the line numbers — those must arrive as structured fields.

### The correct approach

Structured metadata travels as typed fields through the pipeline:

```
TruncationResult  →  ToolResult  →  AgentEvent::ToolCallEnd  →  Message  →  build_log_lines
```

The UI reads `msg.display_range` directly. The model receives human-readable
prose in `content` that conveys the same information. The two are generated
from the same source data but are never parsed back from each other.

---

## Truncation Strategy

### Model-side (tool result content)

Shell tools (`bash`, `cmd`, `powershell`) apply **tail truncation** — the last
N lines are kept, because errors and final results appear at the end.

The `read_file` tool applies **head truncation** with an explicit
`offset`/`limit` pagination API so the model can page through large files.

Limits (tunable constants, not hard-coded magic numbers):

| Limit | Default | Rationale |
|---|---|---|
| Max lines | 2 000 | Covers typical build/test output |
| Max bytes | 50 KiB | Keeps context token cost bounded |
| Single-line cap | 240 chars | Prevents minified JS / binary blobs |

Whichever limit is hit first wins. The single-line cap applies only when the
entire output is a single line (e.g. a minified file or base64 blob); it does
not apply to individual lines within multi-line output.

When truncation occurs the model receives a **prose notice** appended to the
content, separated by a blank line:

```
Output truncated. Showing lines 1001–3000 of 4500. Full output of `make test`
saved to /tmp/tau-tool-output-abc123/call_xyz.stdout
```

This notice is written for the model to read, not for code to parse. The
structured data (start line, end line, total lines, file paths) is carried
separately in `ToolResult` and `Message` fields (see below).

The full raw stdout and stderr are written to temp files under a per-session
directory (cleaned up on exit):
- `$XDG_RUNTIME_DIR/tau/tool-output/<session-id>/<call-id>.stdout`  (Linux)
- `$TMPDIR/tau-tool-output-<session-id>/<call-id>.stdout`            (macOS / Windows)
- Stderr gets a `.stderr` sibling file; empty streams are omitted.

The model is therefore able to:
- Read the tail immediately (errors are usually there).
- Understand the full shape of the output from the prose notice.
- Request a different window by calling `bash` again with `head`/`sed`/`tail`
  or by using `read_file` with `offset`/`limit` on the saved file path.

The `find_files` tool uses a simpler count-cap (default 1 000 results) with an
inline prose notice; no temp file is written because the output is already
structured and compact.

### UI-side (chat log preview)

The chat log renders a short **preview** of each tool result — enough to
confirm at a glance what happened. The preview limit (200 display characters +
`…`) is a display-only concern and is never sent to the model.

The tool-call row annotation (e.g. `[10-20/300]` for a `read_file` call) is
derived from **`msg.display_range`**, a structured field on `Message`, not from
parsing the content string.

---

## Data Structures

### `TruncationResult` (existing, `truncate.rs`)

| Field | Type | Meaning |
|---|---|---|
| `content` | `String` | Truncated text (body only, without any prose notice) |
| `truncated` | `bool` | Whether any truncation was applied |
| `total_lines` | `usize` | Line count of the full original output |
| `total_bytes` | `usize` | Byte size of the full original output |
| `output_lines` | `usize` | Number of complete lines in `content` |
| `first_kept_line` | `usize` | 1-indexed first line included in `content` |

### `ToolResult` (existing, `types.rs`)

| Field | Meaning |
|---|---|
| `content` | Text the model receives (truncated body + prose notice when applicable) |
| `is_error` | Whether the tool failed |
| `is_truncated` | Whether `content` was truncated |
| `truncation` | `Option<TruncationResult>` — structured metadata when truncated |
| `raw_stdout` | Full pre-truncation stdout (set when `saves_output()` is true) |
| `raw_stderr` | Full pre-truncation stderr (set when `saves_output()` is true) |

### `DisplayRange` (new, `llm/mod.rs`)

```rust
pub struct DisplayRange {
    pub first_line: usize,   // 1-indexed
    pub last_line: usize,    // 1-indexed, inclusive
    pub total_lines: usize,
}
```

### `Message` (updated, `llm/mod.rs`)

A new optional field is added to `Message`:

```rust
#[serde(default)]
pub display_range: Option<DisplayRange>,
```

- Populated by the agent loop when converting a `ToolResult` with truncation
  metadata into a `Message::tool_result`.
- Serialised/deserialised so it survives session persistence.
- Deserialises as `None` for old sessions — graceful degradation, the
  annotation simply won't appear for those messages.

---

## Pipeline

```
Tool executes
  → TruncationResult  (structured metadata + truncated body)
  → ToolResult        (body + prose notice in content, metadata in fields)
  → agent loop writes streams to ToolOutputLog, builds prose notice,
    constructs DisplayRange from TruncationResult
  → AgentEvent::ToolCallEnd { result }
  → App::handle_event: Message::tool_result_with_range(…, display_range)
  → build_log_lines reads msg.display_range for the annotation
  → build_log_lines reads msg.content directly for the preview (no header to strip)
```

---

## Migration: removing `split_read_file_header`

Currently `read_file` prepends `[lines X-Y of Z]\n` as a literal text header
in `content`, and `ui.rs` calls `split_read_file_header()` to parse it back
out. The steps to remove this:

1. Add `DisplayRange` struct and `display_range: Option<DisplayRange>` field to
   `Message`; add `Message::tool_result_with_range` constructor.
2. Update `read_file`: stop prepending the text header; instead return a
   `ToolResult` that carries a populated `truncation` field (reusing
   `TruncationResult`) when a partial window was read.
3. In the agent loop / `App::handle_event`: when a `ToolResult` has truncation
   metadata, construct a `DisplayRange` from it and call
   `Message::tool_result_with_range`.
4. In `build_log_lines`: replace the `split_read_file_header` call with a
   direct read of `msg.display_range` for the tool-call row annotation; read
   `msg.content` directly for the preview (no stripping needed).
5. Delete `split_read_file_header` and its tests.
6. Update the `read_file` description string to drop the mention of the header.

Session compatibility: old sessions with the `[lines …]` header still in
`content` will render it as part of the preview text. That is acceptable — it
is better than a parse failure, and the annotation just won't appear.

---

## Temp File Lifecycle

- One `ToolOutputLog` is created per agent session; its directory is removed on
  `Drop`.
- File names are `<sanitised-call-id>.stdout` / `.stderr`.
- Call IDs may contain `/`, `.`, or other unsafe characters; these are
  replaced with `_` in file names.
- The directory is placed under `$XDG_RUNTIME_DIR` on Linux (in-memory tmpfs,
  cleared on logout) or `$TMPDIR` elsewhere.
- On crash the directory is left on disk but is small (bounded by the
  per-session output limit) and will be cleaned up by the OS temp-dir sweep.

---

## Scope and Non-Goals

**In scope:**
- `bash`, `cmd`, `powershell` tail truncation with temp file offload.
- `read_file` head truncation with `offset`/`limit` pagination.
- `find_files` count cap with inline prose notice.
- `ToolOutputLog` per-session temp directory with `Drop` cleanup.
- `DisplayRange` + `Message::display_range` for structured UI annotation.
- Removal of `split_read_file_header` and the text-header convention.

**Out of scope (potential future work):**
- Streaming tool output to the model incrementally.
- Configurable per-tool truncation limits via `tau.toml`.
- Automatic retry / re-read when the model's response references a line range
  beyond what was shown.
- Binary output handling (base64 transcoding, MIME detection).
- Compression of the saved temp files.

---

## Testing Approach

Each layer is tested independently:

| Layer | What is tested |
|---|---|
| `truncate.rs` | Tail/head correctness, line counting, byte limits, single-line cap, trailing-newline edge cases |
| `bash.rs` | Full round-trip: stdout/stderr capture, exit code appending, truncation flag, tail preservation |
| `read.rs` | Offset/limit slicing, `TruncationResult` fields populated correctly, no text header in content |
| `tool_output_log.rs` | File naming, sanitisation, empty-stream skipping, `Drop` cleanup |
| `agent/mod.rs` | Prose notice construction, `DisplayRange` population from `TruncationResult` |
| `ui.rs` | Preview truncation, annotation read from `msg.display_range` (not from content parsing); `split_read_file_header` deleted |
