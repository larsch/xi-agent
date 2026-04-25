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
  markdown.rs          — markdown → ratatui Lines renderer (paragraphs, headings, code, tables, lists)
  commands/
    mod.rs             — slash-command registry (COMMANDS, SlashCommand, CommandAction, parse)
  completion.rs        — CompletionItem and completions_for (completion popup logic)
  completion_state.rs  — CompletionState sub-struct: popup items, selection index, model-fetch status
  selection_state.rs   — SelectionState, SelectionKind, MAX_SELECTION_VISIBLE
  login_state.rs       — LoginState, LoginActionKind: auth panel state, login action menu enum, and all login-flow methods (start_login, cancel_login, enter_login_selection_mode, enter_login_action_menu, apply_login_action, apply_login_event, clipboard_set)
  ask_user_state.rs    — AskUserState, PendingAsk: pending agent ask-user request state
  agent_runtime.rs     — AgentRuntime: agent task handle, event channels, steering queue, cancellation
  config.rs            — config.toml loading (XDG + HOME fallback)
  provider.rs          — provider routing, thinking support, context-window fallback table
  provider_instance.rs — BackendPreset/ProviderInstance types and preset metadata catalog
  event_log.rs          — append-only durable session event log (JSONL, legacy message migration)
  projection.rs         — pure and incremental projections from SessionEvent history to display/LLM messages
  session_event.rs      — durable committed conversation/domain event types
  session_state.rs      — committed session owner: EventLog + display/LLM read models
  live_turn.rs          — transient in-flight assistant/tool/notices state for one active turn
  session.rs            — persisted chat session storage/index
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
      terminal.rs      — apply_terminal_render: emulate terminal cursor behavior for \r
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
    test_provider.rs   — TestProvider (scripted sequences for UI exercise/testing)
```

## Data Flow

```
User keystroke → App::submit
  └─ ensure SessionState exists
     └─ append committed UserMessage event via SessionState ingestion
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

App::apply_event drains tx on each draw tick
  ├─ committed events → SessionState ingestion
  │   └─ updates committed display + committed LLM read models
  └─ transient streaming/tool/notices → LiveTurnState
      └─ ui::draw renders committed SessionState display + LiveTurnState overlay

LLM input construction
  └─ App::prepare_llm_messages
      └─ system prompt + SessionState committed LLM projection only
         (LiveTurnState content is excluded)
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

// session_state.rs — committed session owner
pub struct SessionState {
    event_log: EventLog,
    display: DisplayProjection,
    llm: LlmProjection,
}

// live_turn.rs — transient in-flight turn state
pub struct LiveTurnState {
    pub assistant_content: String,
    pub assistant_thinking: Option<String>,
    pub assistant_phase: AssistantPhase,
    pub tool_entries: Vec<LiveToolEntry>,
    pub notices: Vec<Message>,
}

// llm/error.rs — typed provider failure
pub enum ProviderErrorKind { Unauthorized, Forbidden, RateLimited, ServerError, Network, Other }
pub struct ProviderError { pub kind: ProviderErrorKind, pub status_code: Option<u16>,
                           pub source: String, pub message: String }

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
    Error(ProviderError),  // typed low-level error; app/main format user-facing text
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
in `llm/*.rs`. Message serialization is centralized in
`llm/provider_format.rs` (`to_openai_wire`, `to_anthropic_wire`,
`to_gemini_wire`, `to_codex_wire`, `to_ollama_wire`); individual provider
modules delegate to the appropriate function rather than maintaining their
own inline conversion logic. `agent/mod.rs`, `app.rs`, and `ui.rs` are
provider-agnostic. New backends implement the trait and are registered in
`provider.rs`.

**`AgentLoopConfig` hooks** — `before_tool_call` and `after_tool_call` are
optional function pointers passed in at construction time. This keeps the
agent loop itself free of UI concerns; a future tool-confirmation UI will
wire a user-approval step through `before_tool_call` without touching the
loop logic.

**Outer provider loop in `main.rs`** — `run()` returns a `RunResult` enum
(`Quit | ChangeModel | ChangeProvider`) rather than mutating global state.
The outer loop in `main` rebuilds the active provider instance's transport and
re-enters `run()` on every model/provider-instance switch, so `App` and `ui`
never depend directly on backend transport details.

**Typed provider errors** — `LlmEvent::Error`, `AgentEvent::Error`, and
`ModelListFuture` carry `ProviderError` (with `ProviderErrorKind`) rather
than a raw string. HTTP status is mapped centrally in `llm/common.rs`:
401→`Unauthorized`, 403→`Forbidden`, 429→`RateLimited`, 5xx→`ServerError`,
network failures→`Network`. Lower layers preserve structured error facts
(`status_code`, low-level `source`, original `message`) without rewriting the
provider/body text. User-facing wording is composed later in `app.rs` and
`main.rs` using the active provider/backend label, so OpenAI-compatible
transports do not surface as `OpenAI` for backends such as Open WebUI. 403
explicitly does **not** trigger a token refresh.

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
command updates the active model's entry and is shown only when the active
provider/model pair reports mapped thinking support. Translation is centralized
in `thinking.rs`: OpenAI-Responses-style backends use a shared reasoning-effort
mapping, Gemini native uses a shared Gemini-specific mapping, and unsupported
routes ignore the setting. For Copilot, support is model-dependent because the
backend route is chosen per model (`gpt-5`/Codex-style models use Responses;
chat-completions and Anthropic-routed models do not currently map thinking).

**Custom user tools** — at startup (and on `/reload`), `load_custom_tools`
scans three directories in order: `~/.tau/tools/`, `./.tau/tools/` (project-
local), and `ProjectDirs::config_dir()/tools/`. Each executable that responds
to `--describe` with a valid JSON descriptor (`name`, `description`,
`parameters_schema`) is registered as a `CustomTool`. At invocation, JSON
args are written to the process stdin; stdout is the result string; non-zero
exit becomes `ToolResult::err`. Built-in tool names take precedence — a
custom tool whose name collides with a built-in is silently dropped (logged
at debug). All three tool directories are shown in `tau --print-dirs`.

**Bash tool terminal rendering** — `apply_terminal_render()` in
`agent/tools/terminal.rs` emulates terminal cursor behavior for carriage
returns (`\r`). When bash commands output progress bars or spinners that use
`\r` to overwrite the current line, the function simulates terminal rendering
to produce clean output: characters overwrite from the cursor position (which
is reset to 0 on `\r`), and only the final rendered state is passed to the
model. This avoids cluttering the LLM's context with intermediate progress
states while preserving multi-line output unchanged.

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

## Context Compaction

Tau now supports durable context compaction through the session event log.
When a completed turn crosses the active model's context threshold, or when a
provider returns a context-overflow-style error, the agent generates a
structured summary of older history and appends a
`SessionEvent::CompactionSummary` boundary. The LLM projection injects the
most recent summary as a synthetic user message and excludes older events,
while the display projection shows a visible `[compacted: Xk → Yk tokens]`
marker.

Compaction uses two token measures:
- provider-reported turn usage when available for trigger decisions
- tau-estimated current context size (`chars / 4`) for cut-point selection and
  before/after reporting

The compaction algorithm preserves recent history verbatim, respects
assistant tool-call/tool-result pairing, derives cumulative `<read-files>` and
`<modified-files>` sections from persisted file-tool events, and can be
triggered manually with `/compact [instructions]` to add user guidance to the
summary prompt.

## What Is Not Here Yet

- **OS keyring-backed secret storage** — auth is now tau-owned and supports
  in-app `/login`, but secrets still live in `auth.json` rather than the
  platform keyring. See [ROADMAP](ROADMAP.md).

See [ROADMAP](ROADMAP.md) for prioritised work items.
