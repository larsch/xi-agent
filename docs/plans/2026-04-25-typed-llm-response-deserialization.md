# Plan: Typed serde deserialization for Anthropic and Gemini response parsing

Date: 2026-04-25

## Context

The LLM provider response handlers fall into two distinct patterns:

**Already typed (good):** `openai.rs`, `ollama.rs`, `codex.rs`, `copilot.rs` —
these deserialize streaming chunks into `#[derive(Deserialize)]` structs
(`ChatChunk`, `ChunkMessage`, etc.) and access fields directly.

**Stringly typed (bad):** `anthropic.rs` and `gemini.rs` — these deserialize
each line into `serde_json::Value` and then navigate with `.get("key")`,
`.as_str()`, `.as_u64()`, chained option combinators, and index-operator
access like `ev["type"].as_str()`. Approximately 50+ navigation chains across
the two files.

Consequences:
- Field names are strings: typos are silent runtime bugs, not compile errors.
- Missing-field handling is ad hoc (`unwrap_or("")`, `unwrap_or(0)`): different
  defaults applied inconsistently across similar fields.
- The code is verbose and hard to follow: `chunk.get("response").and_then(|r|
  r.get("candidates")).and_then(|c| c.as_array()).and_then(|a| a.first())` vs
  `chunk.response?.candidates?.first()`.
- Unit-testing parse logic requires constructing `serde_json::json!({...})`
  blobs instead of typed struct literals.
- When the API adds a new field (e.g. Anthropic's extended thinking), adding
  handling requires inserting more `.get()` chains rather than extending a
  struct.

---

## Scope

### In scope

#### Anthropic (`src/llm/anthropic.rs`)

The Anthropic SSE stream emits a sequence of typed events. Define:

```rust
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicEvent {
    MessageStart      { message: MessageStartPayload },
    ContentBlockStart { index: u64, content_block: ContentBlock },
    ContentBlockDelta { index: u64, delta: ContentDelta },
    ContentBlockStop  { index: u64 },
    MessageDelta      { delta: MessageDeltaPayload, usage: Option<MessageDeltaUsage> },
    MessageStop,
    Error             { error: AnthropicApiError },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct MessageStartPayload {
    usage: Option<MessageUsage>,
}

#[derive(Deserialize)]
struct MessageUsage {
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    ToolUse { id: String, name: String },
    Text,
    Thinking,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentDelta {
    TextDelta    { text: String },
    ThinkingDelta { thinking: String },
    InputJsonDelta { partial_json: String },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct MessageDeltaPayload {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct MessageDeltaUsage {
    output_tokens: Option<usize>,
}

#[derive(Deserialize)]
struct AnthropicApiError {
    message: String,
}
```

Replace the `let ev: serde_json::Value = serde_json::from_str(&data)` +
`match ev["type"].as_str()` dispatch with `let ev: AnthropicEvent =
serde_json::from_str(&data)` + `match ev { ... }`. All `.get(…).as_str()`
chains become struct field accesses.

The `extract_usage_stats` free function is replaced by direct field access on
the typed structs.

#### Gemini (`src/llm/gemini.rs`)

The Gemini streaming response wraps candidates in a nested structure. Define:

```rust
#[derive(Deserialize)]
struct GeminiStreamChunk {
    response: Option<GeminiResponse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: Option<GeminiContent>,
}

#[derive(Deserialize)]
struct GeminiContent {
    parts: Option<Vec<GeminiPart>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum GeminiPart {
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
    Text {
        text: String,
        #[serde(default)]
        thought: bool,
    },
}

#[derive(Deserialize)]
struct GeminiFunctionCall {
    name: String,
    id: Option<String>,
    args: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsage {
    prompt_token_count: Option<usize>,
    candidates_token_count: Option<usize>,
    total_token_count: Option<usize>,
}
```

Replace the `chunk.get("response").and_then(...)...` navigation with direct
struct field access. The `parse_usage` free function becomes a trivial
`From<GeminiUsage>` conversion or inline mapping.

### Out of scope

- Changing any wire behavior — this is purely a parse-layer refactor.
- OpenAI / Ollama / Codex / Copilot — already typed; no change needed.
- Request serialization (`to_*_wire` functions in `provider_format.rs`) — 
  separate concern, already tracked in issue #45 (complete).

---

## Affected files

- `src/llm/anthropic.rs`
- `src/llm/gemini.rs`

---

## Assumptions

- `serde`'s `#[serde(tag = "type")]` handles Anthropic's tagged union correctly
  for all current event types.
- `#[serde(other)]` on the `Unknown` variant silently drops unrecognized event
  types — same as the current `_ => {}` arm.
- `#[serde(untagged)]` on `GeminiPart` correctly disambiguates `functionCall`
  from `text` parts since they have mutually exclusive fields.
- `#[serde(rename_all = "camelCase")]` covers all Gemini camelCase field names.
- The `id` field on Gemini function calls is optional (currently generated via
  timestamp fallback); this remains valid after typing.

---

## Risks

- **Anthropic `message_start` usage path:** Input tokens arrive in
  `message_start`, output tokens in `message_delta`. The current code carries
  `input_tokens_from_start` as a local variable and merges it into `message_delta`
  usage. This logic stays exactly as-is; only the parse path changes.
- **Unknown event types:** New Anthropic/Gemini event types added by the API
  will be silently ignored via `#[serde(other)]` / `Unknown` variant — identical
  to the current `_ => {}` behavior.
- **Gemini `#[serde(untagged)]` ambiguity:** If a part has both `text` and
  `functionCall` (not observed in practice), `untagged` picks the first
  matching variant. Add a unit test for this edge case.

---

## Verification

- `just preflight` passes.
- All existing Anthropic/Gemini-related unit tests pass unchanged.
- Add unit tests for:
  - Anthropic: `message_start` → `content_block_start` (text) → `content_block_delta` → `message_stop` round-trip
  - Anthropic: `content_block_start` (tool_use) → `input_json_delta` → `content_block_stop` round-trip
  - Anthropic: `error` event parsing
  - Gemini: text part parsing (plain + thought=true)
  - Gemini: `functionCall` part parsing with and without `id`
  - Gemini: `usageMetadata` parsing
- Manual smoke test: send a tool-using message via Anthropic and Gemini; verify
  correct tool call dispatch and usage stats display.

---

## Ordered steps

1. Define all Anthropic typed structs/enums at the top of `anthropic.rs`.
2. Replace `let ev: serde_json::Value` + navigation chains with typed dispatch.
3. Remove `extract_usage_stats` free function; inline field access.
4. Add Anthropic unit tests.
5. Preflight.
6. Define all Gemini typed structs/enums at the top of `gemini.rs`.
7. Replace `chunk.get(...)` navigation chains with typed field access.
8. Remove `parse_usage` free function (or replace with trivial mapping).
9. Add Gemini unit tests.
10. Final preflight.
