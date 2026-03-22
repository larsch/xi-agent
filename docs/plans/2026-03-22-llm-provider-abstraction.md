# Plan: Reduce LLM Provider Abstraction Leakage (TAU-REVIEW §4)

**Date:** 2026-03-22  
**Status:** Planned  
**Review reference:** TAU-REVIEW.md §4 — "Leaky LLM Provider Abstraction"

---

## Chosen direction

Extract the duplicated cross-cutting logic that every provider reimplements into a shared
`src/llm/common.rs` helper module.  Keep each provider's unique message-format conversion
and API-specific parsing intact; only the *mechanical boilerplate* that is character-for-
character identical (or near-identical) across providers moves to the shared module.

A full trait-based `StreamingResponse` redesign (as sketched in the review) is out of scope
for this plan: the providers share scaffolding, not a common parse shape, so a trait would
add abstraction without reducing code.

---

## Scope

### In scope

| Area | Duplication today | Action |
|---|---|---|
| SSE line-extraction loop | Copied verbatim in `openai.rs`, `anthropic.rs`, `gemini.rs`, `codex.rs` | Extract `SseLineDecoder` into `common.rs` |
| `infer_initiator()` | Identical function in `openai.rs`, `codex.rs`, `anthropic.rs` | Move to `common.rs`, re-export |
| `normalize_tool_name()` | Near-identical in `openai.rs` and `anthropic.rs` (same emoji→name table) | Move to `common.rs`, re-export |
| HTTP connect + error pattern | Same `match req.send()` + status check block in all 5 providers | Extract `send_streaming_request()` helper |

### Out of scope

- Unifying message-history serialisation (each provider's format is genuinely different).
- Unifying usage-stats parsing (field names differ significantly per API).
- Trait-based chunk parsing (would require restructuring `async_stream::stream!` bodies).
- `ThinkingLevel` mapping (tracked separately in TAU-REVIEW §8).

---

## Affected files

| File | Change |
|---|---|
| `src/llm/common.rs` | **New** — shared helpers |
| `src/llm/mod.rs` | Add `pub mod common;` |
| `src/llm/openai.rs` | Remove duplicated helpers; use `common::` |
| `src/llm/anthropic.rs` | Remove duplicated helpers; use `common::` |
| `src/llm/codex.rs` | Remove duplicated helpers; use `common::` |
| `src/llm/gemini.rs` | Remove duplicated helpers; use `common::` |
| `src/llm/ollama.rs` | Use `send_streaming_request()` helper; no SSE loop |

---

## Implementation steps

### Step 1 — Create `src/llm/common.rs`

**1a. `infer_initiator`**

```rust
/// Returns `"user"` when the last message is from a user (or the history is
/// empty), and `"agent"` otherwise.  Used by providers that support an
/// X-Initiator hint header.
pub fn infer_initiator(messages: &[Message]) -> &'static str {
    match messages.last().map(|m| &m.role) {
        Some(Role::User) | None => "user",
        _ => "agent",
    }
}
```

**1b. `normalize_tool_name`**

```rust
/// Map emoji shorthand tool names to their canonical ASCII names.
/// Passthrough for names that are already ASCII.
pub fn normalize_tool_name(name: &str) -> &str {
    match name {
        "👀"  => "read_file",
        "✍️"  => "write_file",
        "📝"  => "edit_file",
        "💻"  => "bash",
        "🔍"  => "find_files",
        other => other,
    }
}
```

**1c. `SseLineDecoder`**

A struct that wraps a `String` buffer and implements a `push_bytes` + `next_line`
iterator-style interface so each provider's event loop becomes:

```rust
let mut sse = SseLineDecoder::new();
while let Some(chunk) = byte_stream.next().await {
    sse.push_bytes(&chunk?);
    while let Some(line) = sse.next_data_line() {
        // `line` is already stripped of "data: " prefix
        if line == "[DONE]" { ... }
        let ev: serde_json::Value = serde_json::from_str(line)?;
        // ... provider-specific parsing
    }
}
```

`next_data_line` returns `None` when no complete line is buffered yet, skips blank
lines and SSE comment lines (`:`-prefixed), strips the `data:` prefix, and
returns `Some("[DONE]")` unchanged so callers can detect stream termination.

**1d. `send_streaming_request` helper**

```rust
/// Send `req` and return its bytes stream, or yield an LlmEvent::Error and
/// return `None` when the request fails or the server responds with a non-2xx
/// status.  `provider_name` is used in log and error messages only.
pub async fn send_streaming_request(
    req: reqwest::RequestBuilder,
    provider_name: &str,
) -> Result<impl Stream<Item = Result<Bytes, reqwest::Error>>, String>
```

Callers use it like:
```rust
let byte_stream = match send_streaming_request(req, "openai").await {
    Ok(s) => s,
    Err(e) => { yield LlmEvent::Error(e); return; }
};
```

This removes the repeated "check status, read body, truncate preview, log warn,
yield error" block from every provider.

---

### Step 2 — Register the new module

In `src/llm/mod.rs`:
```rust
pub mod common;
```

---

### Step 3 — Update each provider

For each of `openai.rs`, `anthropic.rs`, `codex.rs`, `gemini.rs`, `ollama.rs`:

1. `use super::common::{infer_initiator, normalize_tool_name, SseLineDecoder, send_streaming_request};`
   (only the symbols that provider actually uses).
2. Delete the local duplicate implementations.
3. Replace the SSE `buf`-accumulation loop with `SseLineDecoder`.
4. Replace the HTTP connect + error-check block with `send_streaming_request`.

`ollama.rs` uses NDJSON (not SSE), so it skips `SseLineDecoder` but gains
`send_streaming_request`.  Its `parse_ndjson_line` helper remains local.

---

### Step 4 — Fix `openai.rs` `normalize_tool_name` return type

The current `openai.rs` version returns `String`; the `anthropic.rs` and new
`common` version returns `&str`.  Align the call sites during the migration:

```rust
// Before:
name: normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default()),
// After (common version returns &str, so .to_string() is explicit):
name: normalize_tool_name(tc.tool_name.as_deref().unwrap_or_default()).to_string(),
```

---

### Step 5 — Tests

Add unit tests in `src/llm/common.rs`:

- `infer_initiator`: empty history → `"user"`, last=User → `"user"`, last=Assistant → `"agent"`.
- `normalize_tool_name`: all five emoji aliases + unknown passthrough.
- `SseLineDecoder`:
  - Skips blank lines.
  - Skips `:` comment lines.
  - Strips `data: ` prefix.
  - Returns `[DONE]` unchanged.
  - Handles partial lines split across two `push_bytes` calls.
  - Handles multiple complete lines in one `push_bytes` call.

Existing per-provider tests remain; they exercise the full round-trip through the
provider logic and continue to pass without modification.

---

## Assumptions

- The `infer_initiator` semantics are identical across all three providers that use it
  (confirmed by reading the code — all three are character-for-character identical).
- `normalize_tool_name` in `openai.rs` (returns `String`) and in `anthropic.rs`
  (returns `&str`) handle the same five mappings; the slight type difference is
  absorbed in step 4.
- Ollama's NDJSON loop is sufficiently different from SSE that it does not benefit
  from `SseLineDecoder`; a separate `NdjsonLineDecoder` is not worth creating now.
- `send_streaming_request` need not be `async_stream`-compatible — it can be a plain
  `async fn` returning `Result<impl Stream, String>`, called at the top of the
  `async_stream::stream!` body with an `await`.

## Risks

| Risk | Mitigation |
|---|---|
| `SseLineDecoder` changes observable parsing behaviour (e.g. trailing `\r` in `\r\n` lines) | Add `\r\n` line-ending test case; trim consistently |
| `send_streaming_request` hides a log line that a provider logs at a different level | Keep per-provider `log::debug!` calls for the outbound request; only the error path moves to common |
| Merge conflicts if other provider work lands concurrently | Plan touches only `src/llm/`; low coupling to the rest of the codebase |

---

## Verification

1. `cargo build` — zero warnings, zero errors.
2. `cargo clippy -- -D warnings` — clean.
3. `cargo test` — all 146 existing tests pass; new `common` tests pass.
4. Manually count `infer_initiator` definitions: `grep -r "fn infer_initiator" src/` → exactly 1 (in `common.rs`).
5. Manually count `normalize_tool_name` definitions: `grep -r "fn normalize_tool_name" src/` → exactly 1.
6. Spot-check that `SseLineDecoder` handles the `\r\n` SSE variant correctly (some proxies inject carriage returns).

---

## Success criteria

- All five providers compile cleanly using the shared helpers.
- `infer_initiator` and `normalize_tool_name` exist in exactly one place.
- The SSE line-extraction buffer loop is not duplicated across providers.
- No provider gains or loses behaviour — only the scaffolding moves.
- Test count increases (new `common` tests), no existing tests are removed.
