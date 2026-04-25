# Plan: Extract shared streaming loop for LLM providers

Date: 2026-04-25
Status: **Complete (2026-04-25)**

## Context

Four provider implementations (`openai.rs`, `anthropic.rs`, `gemini.rs`,
`ollama.rs`) each independently implement the same streaming request/response
loop structure:

1. Clone fields into the async closure.
2. Build a request and send it via `send_streaming_request`.
3. Iterate `response.bytes_stream()`.
4. Feed bytes into `SseLineDecoder` (or a manual newline buffer for Ollama NDJSON).
5. For each decoded line: deserialize, map to `LlmEvent`s, yield them.
6. Handle byte-stream errors uniformly.
7. Yield `LlmEvent::Done` at end.

Steps 1–4 and 6–7 are identical or near-identical across all four providers.
Only step 5 (line parsing / event mapping) differs per protocol.

Issue #45 (complete) unified message *serialization* into `provider_format.rs`.
This plan unifies the streaming *loop* itself.

The duplication creates real maintenance cost: the `[TAU_DEBUG]` log prefix is
already inconsistent across providers, byte-stream error handling is copy-pasted
with minor variations, and Gemini's retry loop sits outside the shared pattern
entirely — meaning retry logic would need to be duplicated if other providers
needed it.

---

## Scope

### In scope

#### Introduce `stream_lines` in `src/llm/common.rs`

A generic streaming driver that owns the byte-loop and delegates line parsing
to a caller-supplied closure:

```rust
/// Drive an SSE (or NDJSON) streaming response, calling `parse_line` for each
/// decoded data line.  Yields `LlmEvent`s produced by `parse_line`.
/// Yields `LlmEvent::Done` after the stream completes normally.
/// Yields `LlmEvent::Error` on network or parse failure.
pub fn stream_sse_lines<F>(
    provider_name: &'static str,
    response: reqwest::Response,
    mut parse_line: F,
) -> LlmStream
where
    F: FnMut(&str, &mut Vec<LlmEvent>) -> StreamControl + Send + 'static,
{
    // ... byte loop, SseLineDecoder, error handling, Done ...
}

pub enum StreamControl {
    Continue,
    Done,
}
```

An NDJSON variant for Ollama:

```rust
pub fn stream_ndjson_lines<F>(
    provider_name: &'static str,
    response: reqwest::Response,
    parse_line: F,
) -> LlmStream
where
    F: FnMut(&str, &mut Vec<LlmEvent>) -> StreamControl + Send + 'static,
```

#### Refactor each provider to use the shared driver

Each provider's `stream_inner` (or equivalent) becomes:

1. Build and send the request (unchanged).
2. Call `stream_sse_lines(provider, response, |line, events| { ... })`.
3. The closure contains only the protocol-specific parse logic — currently
   the body of the `while let Some(line) = sse.next_data_line()` block.

The byte loop, `SseLineDecoder`, byte-stream error handling, and `Done` emission
all disappear from each provider file.

**OpenAI / Codex / Copilot:** Use `stream_sse_lines`. The `[DONE]` sentinel
handling moves into `stream_sse_lines` (it's identical for all OpenAI-format
providers) or into the parse closure.

**Anthropic:** Use `stream_sse_lines`. The parse closure becomes the
`match ev { AnthropicEvent::... }` dispatch (after the typed-serde plan is
applied).

**Gemini:** Use `stream_sse_lines`. The Gemini retry loop (which currently
wraps the entire request + stream at the outer level) is kept as-is or
extracted to a `retry_streaming_request` helper — it composes with
`stream_sse_lines` because the retry loop produces a `reqwest::Response` before
the stream driver is called.

**Ollama:** Use `stream_ndjson_lines` (newline-delimited, not SSE). The
existing `parse_ndjson_line` free function becomes the closure body directly.

#### Standardize debug logging

The `[TAU_DEBUG]` line log is currently emitted inconsistently:
- OpenAI: `← chunk {line_num}: {line}`
- Anthropic: `← anthropic chunk {line_num}: {data}`
- Ollama: `← ollama chunk {line_num}: {line}`
- Gemini: no per-line logging

Move the per-line debug log into `stream_sse_lines` / `stream_ndjson_lines`
with a consistent format: `[TAU_DEBUG] ← {provider} chunk {n}: {line}`.

### Out of scope

- Changing any wire-protocol behavior.
- Gemini's retry logic beyond making it compose correctly with the shared driver.
- The request-building side (headers, auth, body serialization).
- Typed serde deserialization (separate plan: `2026-04-25-typed-llm-response-deserialization.md`).

---

## Affected files

- `src/llm/common.rs` (new `stream_sse_lines`, `stream_ndjson_lines`)
- `src/llm/openai.rs`
- `src/llm/anthropic.rs`
- `src/llm/gemini.rs`
- `src/llm/ollama.rs`
- `src/llm/codex.rs`
- `src/llm/copilot.rs` (if it has its own loop; otherwise no change)

---

## Assumptions

- The `SseLineDecoder` in `common.rs` is reusable as-is inside the shared driver.
- Gemini's outer retry loop can remain in `gemini.rs` and produce a
  `reqwest::Response` that is then passed to `stream_sse_lines`.
- The `[DONE]` sentinel is universal across all SSE providers; it can be
  handled inside `stream_sse_lines` before calling `parse_line`, returning
  `StreamControl::Done`.
- The `FnMut` closure captures per-stream mutable state (tool-call accumulators,
  `emitted_tool_intent`, etc.) from the enclosing scope — this is the natural
  Rust pattern.

---

## Risks

- **Closure capture complexity:** Tool-call accumulation state (e.g.
  `HashMap<u32, PartialToolCall>` in OpenAI, `HashMap<u64, ToolBlock>` in
  Anthropic) is mutable and referenced across multiple lines. This state must
  be captured by the closure — straightforward in Rust but requires the closure
  to be `FnMut`, not `Fn`.
- **Gemini retry loop interaction:** Gemini retries the full request on 429/5xx.
  The retry loop currently wraps both the request and stream phases. Separating
  "request → Response" from "Response → stream" is correct but requires care
  that the retry loop ends before `stream_sse_lines` is called.
- **Order of plans:** This plan is independent of the typed-serde plan but
  composes well with it. Applying typed serde first makes each provider's parse
  closure smaller and cleaner. Either order works; applying typed-serde first
  is recommended.

---

## Verification

- `just preflight` passes.
- All existing provider-specific unit tests pass unchanged.
- Each provider file loses its byte-loop boilerplate (net line reduction ≥ 30
  lines per provider).
- `common.rs` grows by ≤ 80 lines (the two drivers).
- Debug log format is consistent across all providers.
- Manual smoke test: streaming text + tool calls work for OpenAI, Anthropic,
  Gemini, and Ollama.

---

## Ordered steps

1. Add `StreamControl` enum and `stream_sse_lines` to `common.rs` (no callers yet).
2. Add `stream_ndjson_lines` to `common.rs`.
3. Refactor `openai.rs` to use `stream_sse_lines`. Preflight.
4. Refactor `codex.rs`. Preflight.
5. Refactor `anthropic.rs`. Preflight.
6. Refactor `gemini.rs` (retry loop stays; only stream phase moves). Preflight.
7. Refactor `ollama.rs` to use `stream_ndjson_lines`. Preflight.
8. Check `copilot.rs` — refactor if it has its own loop.
9. Verify debug log format consistency.
10. Final preflight.
