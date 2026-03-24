# TAU Code Review

**Date:** 2026-03-22 (last refreshed: 2026-03-24)  
**Scope:** Architecture, structure, abstractions, complexity, correctness, maintainability  
**Build Status:** ✅ Compiles clean, 0 clippy warnings, 229 tests passing

---

## CRITICAL ISSUES

None identified. The codebase compiles cleanly with no warnings or test failures.

---

## STRUCTURAL CONCERNS

### 2. God Object: App State Explosion

**Severity:** High  
**File:** `src/app.rs` (55KB)

The `App` struct now has **~95 fields** (was ~70 at review time; grown further with auth
preflight and thinking fields added post-review). Current state:
- Message history (1 field)
- Textarea input state (1 field)
- Scroll/display state (5 fields)
- Completion popup state (3 fields)
- Model fetching state (4 fields)
- Selection menu state (7 fields)
- Info bar state (2 fields)
- Login overlay state (10 fields)
- Session persistence state (4 fields)
- ask_user popup state (2 fields)
- **5 async channel receivers + 5 senders**
- Steering message queue
- Active task handle

**Problem:** This is unmaintainable. New features require adding new fields and managing new state transitions. It's impossible to understand what's actually needed at any given time.

**Impact:**
- State management is implicit and fragile
- Adding a feature = adding another field + scatter across multiple handlers
- No clear lifecycle for different UI modes
- Difficult to unit test individual behaviors

**Recommendation:** Decompose into feature-specific state structs:
```rust
struct CompletionPopup { items: Vec<..>, selected: usize, ... }
struct SelectionMenu { title: &'static str, items: Vec<..>, ... }
struct LoginOverlay { provider: String, url: Option<String>, ... }
struct AskUserOverlay { options: Vec<..>, reply_tx: oneshot::Sender<..>, ... }

pub struct App {
    messages: Vec<Message>,
    textarea: TextArea<'static>,
    display: DisplayState,
    
    // Owned only by relevant sub-state:
    completion: Option<CompletionPopup>,
    selection: Option<SelectionMenu>,
    login: Option<LoginOverlay>,
    ask_user: Option<AskUserOverlay>,
    
    // Async bridge
    event_rx: UnboundedReceiver<AppEvent>,
    ...
}
```

---

### 3. Monolithic Event Loop

**Severity:** High  
**File:** `src/main.rs`, lines ~340–667

The `run()` function contains a **327-line tokio::select!** block handling:
- Terminal key events + mouse
- LLM streaming events
- Model list fetches
- Login flow events
- ask_user requests

**Problem:** Impossible to reason about. State transitions depend on which channel event arrived. No clear handler separation or testability.

**Current pattern:**
```rust
tokio::select! {
    Some(Ok(ev)) = crossterm_events.next() => {
        // 200 lines of key handling, selection mode, textarea, shortcuts
    }
    Some(ev) = app.event_rx.recv() => {
        app.apply_event(ev); // Delegates to App
    }
    Some(models) = app.models_rx.recv() => {
        app.apply_model_list(models);
    }
    Some(ev) = app.login_rx.recv() => {
        app.apply_login_event(ev);
    }
    Some(req) = app.ask_rx.recv() => {
        app.receive_ask_request(req);
    }
}
```

**Issues:**
- No separation of concerns (input handling, async event dispatch)
- Hard to add a new event type (requires modifying the select block)
- Impossible to test event handlers in isolation
- Control flow is hidden by tokio::select! semantics

**Recommendation:** Create a unified `AppEvent` enum and a single `apply_event(&mut self, event: AppEvent)` dispatcher:
```rust
pub enum AppEvent {
    KeyPress(KeyEvent),
    Mouse(MouseEvent),
    LlmStreamEvent(AgentEvent),
    ModelListFetched(Result<Vec<String>, String>),
    LoginEvent(LoginEvent),
    AskRequest(AskRequest),
    // ... etc
}

// In run():
tokio::select! {
    Some(ev) = event_stream.next() => {
        app.apply_event(AppEvent::KeyPress(ev));
    }
    Some(ev) = app.event_rx.recv() => {
        app.apply_event(AppEvent::LlmStreamEvent(ev));
    }
    // ... similar for each source
}
```

This makes the event loop testable and extensible.

---

### 4. Leaky LLM Provider Abstraction

**Severity:** Medium-High  
**Files:** `src/llm/openai.rs`, `src/llm/codex.rs`, `src/llm/gemini.rs`, etc.

**Partial improvement in `416c270`:** HTTP status → typed error mapping is now centralized
in `llm/common.rs` (`ProviderError`/`ProviderErrorKind`), eliminating the per-provider 401
string heuristics described below. The remaining duplication is SSE stream parsing and
provider-specific message serialization.

Each provider independently implements:
- Message serialization to provider-specific format
- Streaming response parsing
- Token accumulation and event emission
- Tool call extraction
- Usage stats normalization

**Pattern duplication:**
- OpenAI/Codex: Both parse SSE streams; similar structure but ~200 lines each
- Message conversion: Each provider converts `Message` → provider format manually
- Tool call handling: Each provider extracts tool calls from different response structures

**Problem:** Changes to core behavior (e.g., phase tracking, thinking token handling) must be replicated across 5 providers. Risk of inconsistency.

**Example:** Thinking token handling:
```rust
// openai.rs
if ev.type_ == "content_block_delta" && ev.delta.type_ == "thinking_delta" {
    yield LlmEvent::ThinkingToken(ev.delta.thinking);
}

// gemini.rs
if part.thinking_text.is_some() {
    yield LlmEvent::ThinkingToken(part.thinking_text.unwrap());
}

// codex.rs
if response.contains("thinking") { /* different logic */ }
```

**Recommendation:** Extract common streaming logic into a trait or helper module:
```rust
pub trait StreamingResponse {
    type Item: IntoIterator<Item = LlmEvent>;
    fn parse_chunk(&self, chunk: &[u8]) -> Self::Item;
}
```

---

### 5. ~~Implicit Tool Trait Coupling to serde_json~~ ✅ RESOLVED

**Resolved in:** `87423f5 Refactor tool args`

**Original severity:** Medium  
**Files:** `src/agent/types.rs`, `src/agent/tools/`

A shared `parse_args<T: DeserializeOwned>()` helper was introduced in `src/agent/tools/mod.rs`. Every built-in tool now defines a `#[derive(serde::Deserialize)]` args struct and calls `parse_args()` instead of manually extracting fields from `serde_json::Value`. Type mismatches and missing required fields are caught by `serde_json::from_value` and surfaced as a `ToolResult::err("Invalid arguments: …")`, eliminating the repetitive per-field guard pattern. Tests covering wrong-type and extra-field cases were added alongside the refactor.

---

## OVERENGINEERING & UNNECESSARY COMPLEXITY

### 6. Steering Message Queue Semantics

**Severity:** Medium  
**Files:** `src/app.rs`, `src/agent/mod.rs`

Steering (user messages typed while agent is running) is modeled as:
- A `Vec<String>` queue in `App`
- A `UnboundedSender<String>` passed to agent loop
- In agent loop, a flag `stop_after_turn_for_steering`

**Problem:** Implicit contract. The flag exists to handle a specific edge case:

From `src/agent/mod.rs`:
```rust
let mut stop_after_turn_for_steering = false;
// ... tool execution ...
for (...) {
    // ... if streaming is active and steering queued, stop after turn
    if steering_queued && stop_after_turn_for_steering {
        break;  // Exit tool loop early
    }
}
```

This is a hack. Steering and tool execution have no clean interaction model.

**Recommendation:** Clarify semantics explicitly:
- Steering should either queue normally OR interrupt the current tool batch
- Don't mix both; pick one and document it
- Consider steering as a first-class `SteeringMode` instead of a string queue

---

### 7. ~~Provider Routing Logic Duplication~~ ✅ RESOLVED

**Resolved in:** `37c335d feat(copilot): drive model routing and context window from /models API`

**Original severity:** Medium  
**File:** `src/provider.rs`, `src/llm/copilot.rs`

A process-global metadata cache (`OnceLock<RwLock<HashMap<String, CopilotModelMeta>>>`) was introduced in `src/llm/copilot.rs`, populated as a side-effect of `list_models()`. Each entry stores the vendor string and optional context-window size exactly as returned by the `/models` API endpoint.

- `classify_copilot_route()` now checks cached vendor first: `"Anthropic"` → `AnthropicMessages`, with name-heuristics retained only as a cold-start fallback (before the cache is populated) or for OpenAI sub-routing where the API provides no disambiguating field.
- `context_window_for_model()` checks the cache first, so the info bar stays accurate for new Copilot models without requiring a code change.

New models that report a vendor in the API response are automatically routed correctly; the hardcoded name-heuristic surface is reduced to the remaining OpenAI sub-routing cases (`codex` / `gpt-5` → Responses API) where no API field currently disambiguates.

---

### 8. Thinking Level Mapping Duplication

**Severity:** Medium  
**File:** `src/provider.rs`, `src/thinking.rs`

Each provider maps `ThinkingLevel` differently:
```rust
// For Gemini:
let mapped_thinking = match thinking {
    ThinkingLevel::Off => None,
    ThinkingLevel::Minimal => Some(GeminiThinkingLevel::Minimal),
    ThinkingLevel::Low => Some(GeminiThinkingLevel::Low),
    // ...
};

// For Codex:
thinking.to_reasoning_effort().map(ToString::to_string)

// For Copilot (conditional):
if matches!(route, CopilotApiRoute::OpenAiResponses) {
    thinking.to_reasoning_effort()
} else {
    None  // Not supported
}
```

**Problem:** Maintenance burden. If thinking levels change or new providers are added, multiple implementations must be updated.

**Recommendation:** Create a trait-based mapping:
```rust
pub trait ThinkingMapper {
    fn map_thinking(&self, level: ThinkingLevel) -> Option<String>;
}

impl ThinkingMapper for GeminiProvider {
    fn map_thinking(&self, level: ThinkingLevel) -> Option<String> { ... }
}
```

---

## CORRECTNESS & ROBUSTNESS

### 9. Fragile Agent Loop Task Management

**Severity:** Medium  
**File:** `src/app.rs`

Agent loop lifetime is managed by task handle:
```rust
pub agent_task: Option<JoinHandle<()>>,

pub fn abort_agent_loop(&mut self) {
    if let Some(task) = self.agent_task.take() {
        task.abort();  // Drop handle to abort
    }
}
```

**Problem:** 
1. Dropping a task handle doesn't guarantee prompt cancellation of long-running tools
2. If the agent loop is in a long-running tool, it will continue past cancellation
3. No explicit cancellation token passed to agent loop

**Example failure scenario:**
1. User starts long bash command
2. User presses Esc to abort
3. `abort_agent_loop()` drops the handle
4. Agent loop is still in `bash.execute()`, waiting for the subprocess
5. The subprocess continues running until it exits naturally

Note: the channel-close scenario is actually safe — when the task handle is dropped,
the `UnboundedSender` in `App` is dropped too, causing `recv()` in the agent loop to
return `None` and exit cleanly. The real problem is that in-flight tool executions
(especially long-running bash commands) are not interrupted.

**Recommendation:** Pass an explicit cancellation token:
```rust
let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
self.agent_cancel_tx = Some(cancel_tx);

tokio::spawn(async move {
    agent::run_agent_loop(..., cancel_rx).await;
});

pub fn abort_agent_loop(&mut self) {
    if let Some(tx) = self.agent_cancel_tx.take() {
        let _ = tx.send(true);  // Explicit cancellation signal
    }
}
```

---

### 10. Config Loading Lacks Validation

**Severity:** Low-Medium  
**File:** `src/config.rs`

Config is loaded with defaults for missing fields:
```rust
#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct OpenAiConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
}
```

Provider build will fail at runtime if `api_key` is None:
```rust
let api_key = config.openai.api_key.clone().ok_or_else(|| {
    anyhow::anyhow!("Missing API key. Configure [openai].api_key in config.toml.")
})?;
```

**Problem:** Errors happen at the moment you try to switch to that provider, not at startup.

**Recommendation:** Validate config on load:
```rust
impl TauConfig {
    pub fn load() -> anyhow::Result<Self> {
        let cfg = Self::load_raw()?;
        cfg.validate()?;  // Fail fast
        Ok(cfg)
    }
    
    fn validate(&self) -> anyhow::Result<()> {
        if let Some(ref provider) = self.provider {
            match provider.as_str() {
                "openai" if self.openai.api_key.is_none() => {
                    return Err(anyhow!("openai selected but api_key not configured"));
                }
                // ...
            }
        }
        Ok(())
    }
}
```

---

### 11. Session Persistence Race Condition

**Severity:** Low  
**File:** `src/session.rs`, `src/app.rs`

Session messages are saved manually:
```rust
pub fn save_messages(
    &mut self,
    session_id: &str,
    cwd: &str,
    messages: &[Message],
) -> anyhow::Result<()> {
    // Updates both index.json and session file
}
```

But the index is in-memory. If multiple tau instances run in the same cwd, they could both update the same session file without coordination.

**Problem:** Data loss or inconsistency if two instances write concurrently.

**Recommendation:** Use advisory file locking or a lock file:
```rust
let lock_path = sessions_dir.join("index.lock");
let _lock = FileLock::lock(&lock_path)?;  // Blocks until exclusive
self.save_index()?;
```

---

### 12. Clipboard Not Cleaned Up on Error

**Severity:** Low  
**File:** `src/app.rs`

Clipboard is kept alive to prevent text loss on Linux:
```rust
pub clipboard: Option<arboard::Clipboard>,
```

But on Windows, it's not used. And if login fails halfway through, clipboard might be dropped prematurely:
```rust
pub fn cancel_login(&mut self) {
    self.login_active = false;
    self.clipboard = None;  // Dropped immediately, might lose text
}
```

**Problem:** On Linux, text is lost from clipboard if tau crashes.

**Recommendation:** Keep clipboard alive until after any paste operations, or use a proper clipboard manager abstraction.

---

## QUESTIONABLE DECISIONS

### 13. Multiple Async Channel Types Instead of Unified Event Loop

**Severity:** Medium  
**Files:** `src/app.rs`, `src/main.rs`

Five separate channels:
- `event_rx` / `event_tx` for LLM events
- `models_rx` / `models_tx` for model list fetches
- `login_rx` / `login_tx` for login events
- `ask_rx` / `ask_tx` for ask_user tool
- (Plus steering_tx)

Each is handled separately in the select! block.

**Problem:** 
1. Hard to reason about message ordering across channels
2. New event type = new channel + new select! arm
3. Potential deadlocks if channel buffers interact badly
4. No single source of truth for what events can occur

**Recommendation:** Unified `AppEvent` enum with single channel:
```rust
pub enum AppEvent {
    Input(InputEvent),
    LlmStream(AgentEvent),
    ModelsReady(Result<Vec<String>, String>),
    LoginUpdate(LoginEvent),
    AskUser(AskRequest),
}

// Single channel
event_rx: UnboundedReceiver<AppEvent>,
```

---

### 14. Hidden Coupling Between Tools and App

**Severity:** Low-Medium  
**Files:** `src/agent/tools/ask_user.rs`, `src/app.rs`

Ask user tool communicates directly with App via a channel:
```rust
// In tool registration:
let tools = register_builtin_tools(Some(app.ask_request_tx()));

// In ask_user tool:
pub struct AskUserTool {
    ask_tx: Option<AskRequestTx>,
}

impl Tool for AskUserTool {
    async fn execute(&self, args: Value) -> ToolResult {
        if let Some(ref tx) = self.ask_tx {
            let (reply_tx, reply_rx) = oneshot::channel();
            tx.send(AskRequest { ..., reply: reply_tx })?;
            // Wait for UI to respond
        }
    }
}
```

**Problem:** Ask user tool has a hidden dependency on the app's event loop. If you want to use the agent loop standalone (e.g., in tests or non-interactive mode), you must handle this channel dance.

**Current workaround in run_print_mode:**
```rust
let tools = register_builtin_tools(None);  // Pass None for ask_user
```

This silently disables the tool instead of providing a fallback.

**Recommendation:** Create a trait-based tool environment:
```rust
pub trait ToolEnv: Send + Sync {
    fn ask_user(&self, req: AskRequest) -> oneshot::Receiver<AskUserResponse>;
    // ... other context methods
}

impl Tool for AskUserTool {
    async fn execute(&self, args: Value, env: &dyn ToolEnv) -> ToolResult {
        // Can now always work; environment provides the interaction point
    }
}
```

---

## MAINTAINABILITY & MINOR ISSUES

### 15. UI Rendering is Monolithic

**Severity:** Medium  
**File:** `src/ui.rs` (1,780 lines)

All rendering logic is in a single file with deeply nested conditionals:
```rust
pub fn draw(f: &mut Frame, app: &App) {
    // Layout computation
    // Conditional rendering for each mode:
    //   - Normal chat + input
    //   - Selection menu overlay
    //   - Login overlay
    //   - ask_user overlay
    //   - Info bar
    // All interleaved with styling
}
```

**Problems:**
1. Impossible to test individual UI components
2. Hard to reason about layout interactions
3. Adding a new overlay requires modifying this monster function
4. Reusing rendering logic (e.g., message list, scroll) is manual

**Recommendation:** Decompose into composable rendering functions:
```rust
fn draw(f: &mut Frame, app: &App) {
    let chunks = compute_layout(&f.area(), app.state());
    
    draw_message_log(f, chunks[0], &app.messages);
    draw_input(f, chunks[1], &app.textarea);
    draw_info_bar(f, chunks[2], app);
    
    match &app.overlay {
        None => {},
        Some(Overlay::Selection(s)) => draw_selection_overlay(f, chunks[3], s),
        Some(Overlay::Login(l)) => draw_login_overlay(f, chunks[3], l),
        Some(Overlay::AskUser(a)) => draw_ask_user_overlay(f, chunks[3], a),
    }
}
```

---

### 16. Error Handling Inconsistency

**Severity:** Low  
**Files:** Throughout

Some functions return `anyhow::Result`:
- `SessionStore::open()`
- `TauConfig::load()`
- `build_provider()`

Others return custom error types or panic:
- Tools return `ToolResult` (not Result)
- Some auth flows use custom LoginEvent variants

**Problem:** Inconsistent error handling makes the codebase harder to reason about.

**Recommendation:** Standardize on `anyhow::Result` for internal APIs, use custom error types only at API boundaries.

---

### 17. Naming Could Be Clearer

**Severity:** Low  
**Files:** Various

- `App::apply_event()` vs `App::receive_ask_request()` — unclear naming pattern
- `event_tx` / `event_rx` — which events? (should be `llm_event_tx`, `agent_event_tx`)
- `steering_tx` — counterintuitive that it's separate from the unified event channel
- `SelectionResult::AskFreeform` vs `SelectionResult::AskOption` — unclear distinction at a glance

**Recommendation:** Rename for clarity:
```rust
// Bad:
pub async fn apply_event(ev: AgentEvent) { }
pub fn receive_ask_request(req: AskRequest) { }

// Good:
pub async fn on_llm_stream_event(&mut self, ev: AgentEvent) { }
pub fn on_ask_user_request(&mut self, req: AskRequest) { }
```

---

## CONCRETE IMPROVEMENTS (Minimal Scope)

1. **Add validation to build_provider():** Fail fast if credentials/config missing
2. **Extract streaming common logic:** Create an `SseStreamParser` utility used by OpenAI + Codex
3. **Create unified `AppEvent` enum:** Consolidate 5 separate channel types
4. **Pass cancellation token to agent loop:** Use `tokio::sync::watch` instead of task.abort()
5. **Add FileLock to session persistence:** Prevent concurrent write corruption
6. **Document channel ordering guarantees:** Or switch to a unified event loop
7. ~~**Use typed args structs in tools:** Replace manual `args.get()` chains with `#[derive(Deserialize)]`~~ ✅ Done (`87423f5`)
8. ~~**Drive Copilot routing from API metadata:** Use vendor/context-window fields from `/models` response~~ ✅ Done (`37c335d`)

---

## SUMMARY

**Overall direction:** Sound core (multi-provider streaming agent loop works), but state management and architecture are beginning to show strain as complexity grows. The god object `App` and monolithic event loop are the biggest blockers for maintainability.

**Current state:** Early/mid-stage project that works and compiles cleanly with all tests passing. No critical bugs, but structural debt is accumulating and fragility is increasing as complexity grows.

**Immediate priorities:**
1. Decompose App state into feature-specific structs
2. Unify event loop around a single AppEvent enum
3. Add explicit agent loop cancellation token
4. Add validation to build_provider() for fail-fast config errors

**Medium-term:** Refactor UI rendering into composable functions, create a model registry to eliminate routing duplication, extract common provider streaming logic.

**Long-term:** If the tool set grows significantly, consider a plugin architecture or move to a more modular design.
