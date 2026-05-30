# Plan: `@filename` injection via synthetic tool events

**Date:** 2026-05-30

## Goal and scope

When a user submits a message containing one or more `@<path>` tokens, xi-agent
resolves each path, reads the file, and inserts a synthetic `ToolCall` +
`ToolResult` event pair immediately before the `UserMessage` event in the
session log. The LLM sees the file content without a round-trip tool call. The
`@path` tokens are kept verbatim in the user message text.

**In scope:**
- Text files → `ToolResult.content` string
- Image files → `ToolResult` with `image_data` on the in-memory `Message` (not
  persisted to the event log, same as real `read_file` image results)
- Errors for unresolvable paths → inline notice, submission not aborted
- Synthetic events hidden in the UI (`Message.hidden = true` on display
  projection), present in the LLM projection

**Out of scope:**
- `@filename` completion (separate task)
- Re-injection across compaction boundaries
- Persistence of image data across session reloads (consistent with existing
  tool-result image behaviour)

## Approach

### 1. `@`-token parser (`src/at_file.rs`, new module)

```
pub struct AtToken { pub path: String, pub raw: &str }
pub fn parse_at_tokens(input: &str) -> Vec<AtToken>
```

Parse `@<path>` tokens from user input. A token starts with `@` preceded by
start-of-string or whitespace, and ends at the next whitespace or end-of-string.
Quoted form `@"path with spaces"` is also supported.

### 2. File resolver (`src/at_file.rs`)

```
pub enum AtFileResult {
    Text { path: String, content: String },
    Image { path: String, base64: String, mime_type: String },
    Error { path: String, message: String },
}
pub async fn resolve_at_tokens(tokens: &[AtToken], cwd: &Path) -> Vec<AtFileResult>
```

Reads each file. Detects image MIME type by extension (png, jpg, jpeg, gif,
webp). Text files read as UTF-8. Returns `Error` variant for missing/unreadable
files.

### 3. Synthetic event injection (`src/app_submission.rs`)

In `submit()`, after trimming textarea content and before calling
`append_user_message`:

1. Call `parse_at_tokens(&trimmed)`.
2. If any tokens found, `resolve_at_tokens(...)` (async — `submit` becomes
   async or spawns a task; see risk below).
3. For each resolved file, push two events via a new helper
   `append_synthetic_file_events(path, result)`:
   - `SessionEvent::ToolCall { id: "attach_{n}", name: "read_file", args: {"path": "<path>"}, timestamp }`
   - `SessionEvent::ToolResult { id: "attach_{n}", name: "read_file", content: "<content or placeholder>", is_error: false, display_range: None, timestamp }`
4. For each error, push a notice (not a session event) visible in the UI.
5. Then call `append_user_message(trimmed)` as before.

Image data: stored in `App::pending_attachment_images: Vec<(String, ImageData)>`
keyed by call ID. The live-turn message builder picks these up when constructing
`Message` objects for synthetic tool results (same path as real image tool
results).

### 4. Display projection (`src/projection.rs`)

In `push_display_message`, detect synthetic tool events by ID prefix `"attach_"`:
set `msg.hidden = true` on both the `ToolCall` and `ToolResult` messages.

### 5. LLM projection (`src/projection.rs`)

`push_llm_message` — no change needed. Synthetic events look identical to real
`ToolCall`/`ToolResult` events and are already handled correctly.

### 6. Image data for synthetic results (`src/live_turn.rs` or `src/app_submission.rs`)

Because `SessionEvent::ToolResult` does not carry `image_data`, image attachments
must be injected into `Message.image_data` at projection time. The cleanest
approach: store `pending_attachment_images` on `App` and inject them in
`live_turn::to_messages()` when building messages for the current turn — the same
place real image tool results are handled.

After the turn completes and events are flushed, image data is dropped (same as
real tool results). On session reload the image is absent — acceptable per the
agreed design.

## Affected files

| File | Change |
|------|--------|
| `src/at_file.rs` | **New** — token parser + file resolver |
| `src/app_submission.rs` | `submit()` — inject synthetic events before user message |
| `src/app.rs` | Add `pending_attachment_images` field to `App` |
| `src/projection.rs` | Hide synthetic events in display projection |
| `src/live_turn.rs` | Inject image data for synthetic tool results |
| `src/main.rs` or `src/lib.rs` | Register new module |

## Code-level done conditions

- `at_file.rs` exists with `parse_at_tokens` and `resolve_at_tokens`; both are
  covered by unit tests.
- `submit()` calls the parser and injects synthetic events before the user
  message; covered by an integration test in `agent/tests.rs` or `app.rs`.
- Synthetic `ToolCall`/`ToolResult` events with ID prefix `"attach_"` are
  hidden (`hidden = true`) in the display projection; verified by a unit test in
  `projection.rs`.
- Synthetic events are present and visible in the LLM projection; verified by a
  unit test.
- Image attachments are sent as `image_data` on the `Message` for the current
  turn.
- Unresolvable paths produce a visible notice and do not abort submission;
  covered by a unit test.
- `cargo clippy` clean, no unused code.

## Risks and assumptions

| Risk | Mitigation |
|------|-----------|
| `submit()` is currently sync; file I/O is async | Resolve files in a spawned task before calling `launch_turn`, or use `std::fs` (blocking) since files are local. Use blocking `std::fs` for simplicity — file reads are small and local. |
| MIME detection by extension may miss files | Treat unknown extensions as text; document this. |
| ID prefix `"attach_"` could collide with LLM-generated IDs | LLM-generated IDs come from the provider (e.g. `call_abc123`); `"attach_"` prefix is safe as a sentinel. |
| Image data not persisted | Accepted by design — same behaviour as real `read_file` image results. |

## Verification

1. `cargo test --all-features` passes. ✅ 674 tests, 0 failed.
2. `cargo clippy --all-targets --all-features -- -D warnings` clean. ✅
3. `cargo fmt --all -- --check` clean. ✅
4. Manual smoke test: type `@src/main.rs` in textarea, submit — verify LLM
   receives file content without a real tool call round-trip.
5. Manual smoke test: `@path/to/image.png` — verify image sent as vision content.
6. Manual smoke test: `@nonexistent.rs` — verify notice shown, message submitted.
7. Grep: `push_display_message` hides both `ToolCall` and `ToolResult` for
   `attach_*` IDs. ✅ confirmed in `projection.rs`.
8. New projection tests added and passing:
   - `synthetic_attachment_events_are_hidden_in_display_projection`
   - `synthetic_attachment_events_are_present_in_llm_projection`
