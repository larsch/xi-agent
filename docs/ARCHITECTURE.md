# Architecture

## Purpose

`tau` is a terminal AI agent harness. It provides a streaming TUI for
conversational interaction with LLMs and runs the full agentic loop: user
message → model response → tool call → tool result → model continues,
until the model returns a final answer without tool calls.

## Module Map

```
src/
  main.rs              — tokio entry point, CLI parsing, outer provider loop
  app.rs               — App state, event handling, submission, scroll
  ui.rs                — all ratatui rendering, pre-wrapping, scroll logic
  commands.rs          — slash-command registry and completion items
  config.rs            — config.toml loading (XDG + HOME fallback)
  provider.rs          — ProviderKind enum, build_provider(), context-window fallback table
  session.rs           — persisted chat session storage/index
  thinking.rs          — ThinkingLevel enum, parse/serialize, per-model resolution
  tool_presentation.rs — tool call/result rendering helpers for the TUI
  auth/                — provider auth store + login/refresh flows + token-state preflight
  agent/
    mod.rs             — run_agent_loop: the multi-turn agentic loop
    types.rs           — Tool trait, ToolRegistry, AgentEvent, AgentLoopConfig
    system_prompt.rs   — build_system_prompt: dynamic system prompt
    file_tracker.rs    — FileTracker: mtime+hash snapshot, external-change detection, diff generation
    tools/
      mod.rs           — register_builtin_tools() (built-ins + custom tools)
      bash.rs          — BashTool  (💻 run shell command)
      read.rs          — ReadFileTool (👀 read file with offset/limit)
      write.rs         — WriteTool (✏️ write/overwrite file)
      edit.rs          — EditTool  (📝 replace exact text in file)
      find.rs          — FindTool  (🔍 search by name glob or content pattern)
      custom.rs        — CustomTool, load_custom_tools, custom_tool_dirs
  llm/
    mod.rs             — LlmProvider trait, Message/Role/LlmEvent/ToolDefinition types
    error.rs           — ProviderError, ProviderErrorKind (typed HTTP/network failures)
    common.rs          — shared HTTP helpers: send_streaming_request, status→ProviderError mapping
    openai.rs          — OpenAiProvider (OpenAI Chat Completions, tool-calling)
    copilot.rs         — CopilotProvider (route by model: vendor from /models cache, name heuristics fallback)
    codex.rs           — CodexProvider (chatgpt.com/backend-api responses)
    anthropic.rs       — AnthropicProvider (Messages API transport)
    gemini.rs          — GeminiProvider (Google Cloud Code Assist streaming)
    ollama.rs          — OllamaProvider (streaming NDJSON via /api/chat)
```

## Data Flow

```
User keystroke → App::submit
  └─ spawns tokio task: run_agent_loop(messages, config, provider, tx, steering_rx)
       └─ drain steering_rx → insert queued user messages before each turn
          for each turn:
            check FileTracker for externally modified files
              └─ if any: inject ⚠️ user message with unified diff (or warn-only if large)
                         send AgentEvent::ExternalFileChange
            provider.stream_chat_with_tools(messages, tool_defs)
              └─ yields LlmEvent::{Token{..}, ThinkingToken, Usage,
                                   ToolIntentStart, ToolCall, Done, Error}
            if ToolCall → tool.execute(args) → ToolResult
              └─ drain steering_rx after each tool → skip remaining tools if non-empty
            loop until no tool calls
            sends AgentEvent::{TextToken{..}, ThinkingToken, Usage,
                               ToolIntentStart, SteeringConsumed,
                               ToolCallStart, ToolCallEnd,
                               ExternalFileChange,
                               TurnEnd, Done, Error} on tx

User keystroke (while streaming) → App::enqueue_steering_from_input
  └─ pushes text onto queued_steering (for 🕹️ UI) + sends on steering_tx

  App::apply_event drains tx on each draw tick → updates messages vec
  ui::draw renders messages vec + queued_steering to terminal
```

## Key Types

### `llm/mod.rs`

```rust
pub enum AssistantPhase { Unknown, Provisional, Final }

pub struct UsageStats {
    pub input_tokens: Option<usize>,
    pub output_tokens: Option<usize>,
    pub total_tokens: Option<usize>,
}

pub struct Message {
    pub role: Role,                      // System | User | Assistant | ToolCall | ToolResult
    pub content: String,
    pub thinking: Option<String>,        // chain-of-thought block
    pub assistant_phase: Option<AssistantPhase>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<serde_json::Value>,
    pub is_error: bool,
}

// llm/error.rs — typed provider failure
pub enum ProviderErrorKind { Unauthorized, Forbidden, RateLimited, ServerError, Network, Other }
pub struct ProviderError { pub kind: ProviderErrorKind, pub status_code: Option<u16>,
                           pub provider: String, pub message: String }

pub enum LlmEvent {
    Token { text: String, phase: AssistantPhase },
    ThinkingToken(String),
    Usage(UsageStats),
    ToolIntentStart,
    ToolCall { id: String, name: String, args: serde_json::Value },
    Done,
    Error(ProviderError),   // typed; 401→Unauthorized, 403→Forbidden, 429→RateLimited
}

pub trait LlmProvider: Send + Sync {
    fn stream_chat(&self, messages: Vec<Message>) -> LlmStream;
    fn stream_chat_with_tools(&self, messages: Vec<Message>, tools: Vec<ToolDefinition>) -> LlmStream;
    fn list_models(&self) -> ModelListFuture;  // Result<Vec<String>, ProviderError>; default: []
}
```

### `agent/types.rs`

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value; // JSON Schema object
    fn execute(&self, args: serde_json::Value) -> Pin<Box<dyn Future<Output = ToolResult> + Send + '_>>;
}

pub enum AgentEvent {
    TextToken { text: String, phase: AssistantPhase },
    ThinkingToken(String),
    Usage(UsageStats),
    ToolIntentStart,
    SteeringConsumed { text: String },
    ToolCallStart { id, name, args },
    ToolCallEnd   { id, name, result: ToolResult },
    TurnEnd,
    Done,
    Error(ProviderError),  // typed; display via Display impl for user-facing messages
}

pub struct AgentLoopConfig {
    pub tools: ToolRegistry,
    pub file_tracker: Arc<Mutex<FileTracker>>,
    pub before_tool_call: Option<Box<dyn Fn(&str, &Value) -> bool + Send + Sync>>,
    pub after_tool_call:  Option<Box<dyn Fn(&str, &ToolResult) -> Option<ToolResult> + Send + Sync>>,
}
```

## Key Design Decisions

**Pre-wrapping instead of ratatui Wrap** — ratatui's `Wrap` widget wraps at
render time and cannot easily pad individual lines to full width. By
pre-wrapping in `build_log_lines` we know the exact row count before
rendering, which makes scroll arithmetic exact and lets us apply per-row
background styles (e.g. the grey user-message highlight).

**Channel-based LLM events** — `tokio::mpsc` decouples the async HTTP
streaming task from the synchronous draw loop. The draw loop never awaits;
it drains the channel non-blockingly on each tick, keeping the TUI
responsive during long model responses.

**Steering queue during streaming** — while a loop is active, Enter enqueues
user steering text into a dedicated channel. The UI renders queued entries at
the bottom with `🕹️` until the loop consumes them. On consumption, a
`SteeringConsumed` event removes the pinned row and inserts the message into
normal transcript order before the next assistant turn.

**`LlmProvider` trait** — all provider-specific wire formats are contained
in `llm/*.rs`. `agent/mod.rs`, `app.rs`, and `ui.rs` are provider-agnostic.
New backends implement the trait and are registered in `provider.rs`.

**`AgentLoopConfig` hooks** — `before_tool_call` and `after_tool_call` are
optional function pointers passed in at construction time. This keeps the
agent loop itself free of UI concerns; a future tool-confirmation UI will
wire a user-approval step through `before_tool_call` without touching the
loop logic.

**Outer provider loop in `main.rs`** — `run()` returns a `RunResult` enum
(`Quit | ChangeModel | ChangeProvider`) rather than mutating global state.
The outer loop in `main` rebuilds the provider and re-enters `run()` on
every model/provider switch, so `App` and `ui` never depend on which
provider is active.

**Typed provider errors** — `LlmEvent::Error`, `AgentEvent::Error`, and
`ModelListFuture` carry `ProviderError` (with `ProviderErrorKind`) rather
than a raw string. HTTP status is mapped centrally in `llm/common.rs`:
401→`Unauthorized`, 403→`Forbidden`, 429→`RateLimited`, 5xx→`ServerError`,
network failures→`Network`. `App` matches on kind directly; no substring
heuristics. 403 explicitly does **not** trigger a token refresh.

**Proactive auth refresh** — before submitting a request or starting a model
fetch, `App::check_token_preflight` inspects the stored `expires_at` via
`auth::token_state`. If the token is `Expired` or `ExpiringSoon` (within
`AUTH_REFRESH_LEEWAY_SECS` = 120 s), it triggers a refresh and defers the
request. A reactive fallback on `Unauthorized` errors handles clock skew and
server-side revocation. The `auth_retry_budget` is not consumed by the
preflight, leaving one reactive retry available if the request still gets a
401 after refresh.

**Display-only sanitization** — message content is stored and sent to the
LLM verbatim. Trailing whitespace per line, leading/trailing newlines, and
excess blank-line collapsing are applied only at render time inside
`ui::sanitize_for_display`. This avoids any mutation of LLM context.

**Per-model thinking settings** — `ThinkingLevel` (Off/Minimal/Low/Medium/
High/XHigh) is resolved at request time from `config.thinking_by_model`
(per-model override) then `config.thinking` (global default). The `/thinking`
command cycles levels at runtime and updates the active model's entry.
Providers that support thinking (Copilot/OpenAI with `o*` models, Anthropic)
translate the level to their respective API parameter; others ignore it.

**Custom user tools** — at startup (and on `/reload`), `load_custom_tools`
scans three directories in order: `~/.tau/tools/`, `./.tau/tools/` (project-
local), and `ProjectDirs::config_dir()/tools/`. Each executable that responds
to `--describe` with a valid JSON descriptor (`name`, `description`,
`parameters_schema`) is registered as a `CustomTool`. At invocation, JSON
args are written to the process stdin; stdout is the result string; non-zero
exit becomes `ToolResult::err`. Built-in tool names take precedence — a
custom tool whose name collides with a built-in is silently dropped (logged
at debug). All three tool directories are shown in `tau --print-dirs`.

**External file change detection** — `FileTracker` (`agent/file_tracker.rs`)
records a snapshot (mtime + SHA-256 + content) for every file successfully
touched by `read_file`, `write_file`, or `edit_file`. At the start of each
LLM turn, `check_modified()` stats every tracked path; if mtime is unchanged
the file is skipped cheaply. On mtime change, the file is re-read and
rehashed; content-identical saves (no-op writes) are suppressed. Truly
changed files produce a `ChangedFile { path, old_content, new_content }`.
The agent loop composes a single ⚠️ user message: diffs with ≤
`DIFF_INLINE_MAX_LINES` (50) changed lines are inlined as unified diffs
(via `similar`); larger diffs get a warn-only note. The message is injected
into the conversation history and mirrored to the UI via
`AgentEvent::ExternalFileChange { paths, notification }`. Binary files
(non-UTF-8) are silently skipped. The tracker is held as
`Arc<Mutex<FileTracker>>` shared between `AgentLoopConfig` and the three
file tools.

## What Is Not Here Yet

- **OS keyring-backed secret storage** — auth is now tau-owned and supports
  in-app `/login`, but secrets still live in `auth.json` rather than the
  platform keyring. See [ROADMAP](ROADMAP.md).
- **Context window management** — no truncation or summarisation when
  conversation history exceeds the model's context window. See
  [plan](plans/2026-03-14-context-management.md).
- **Deeper test coverage** — auth store persistence is covered; tool
  implementations, agent loop, and provider wire format still lack tests. See
  [plan](plans/2026-03-14-tests.md) and [auth tests plan](plans/2026-03-15-auth-tests-plan.md).

See [ROADMAP](ROADMAP.md) for prioritised work items.
