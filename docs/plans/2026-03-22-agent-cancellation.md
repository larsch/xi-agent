# Agent loop cancellation robustness

**Date:** 2026-03-22  
**Status:** Planned  
**Priority:** Medium  
**Risk:** Phase A — Low (one-liner per tool file); Phase B — Medium (function signature change)  
**Source:** TAU-REVIEW.md §9 — Fragile Agent Loop Task Management

---

## Problems

### Problem A — Child processes are not killed on abort

When `abort_agent_loop()` calls `handle.abort()`, Tokio cancels the agent loop
task at its next `.await` point.  If the loop is currently inside
`bash.execute()` → `Command::output().await`, Tokio drops the `Command` future.
Because `.kill_on_drop(true)` is not set, the child `sh`/`cmd.exe`/`powershell.exe`
process **continues running** in the background after the agent loop has been
marked as stopped.

Consequences:
- Long-running commands (e.g. `find / …`, `cargo build`) survive after the user
  presses Esc.
- The process may continue modifying files or consuming resources the user
  intended to stop.

### Problem B — No deterministic, testable cancellation signal between turns

The current approach relies entirely on `handle.abort()` and the implicit
cancellation of Tokio futures. There is no way to:
- Test in a unit test that the loop stops at a predictable boundary.
- Have the loop perform cleanup or emit a proper cancellation event.
- Distinguish "stopped because done" from "stopped because cancelled".

## Goals

### Phase A (immediate, low risk)
1. Add `.kill_on_drop(true)` to the `Command` builder in `bash.rs`, `cmd.rs`,
   and `powershell.rs` so that aborting the Tokio task also terminates the child
   process.

### Phase B (medium risk)
2. Add a `tokio::sync::watch::Receiver<bool>` cancellation parameter to
   `run_agent_loop`, checked at the start of every loop iteration (between turns)
   and after each tool call in a batch.
3. Update `App::start_agent_task` to create and store the watch sender.
4. Update `App::abort_agent_loop` to signal cancellation via the watch channel
   (in addition to keeping `handle.abort()` as a hard fallback).
5. Make the cancellation path unit-testable without timing dependencies.

## Non-goals

- Cancelling during an in-flight LLM stream token (the existing `handle.abort()`
  handles that at the stream iteration's `.await` point; clean stream cancellation
  is a separate problem).
- Adding a new `AgentEvent::Cancelled` variant (can be added later if needed;
  Phase B uses `AgentEvent::Done` as the terminal event on cancellation to keep
  the change self-contained).

---

## Phase A: kill_on_drop

### Change in `src/agent/tools/bash.rs`

```rust
// Before:
let output = match tokio::process::Command::new("sh")
    .arg("-c")
    .arg(&command)
    .output()
    .await
{ ... }

// After:
let output = match tokio::process::Command::new("sh")
    .arg("-c")
    .arg(&command)
    .kill_on_drop(true)   // ← added
    .output()
    .await
{ ... }
```

Apply the same one-line addition in `src/agent/tools/cmd.rs` and
`src/agent/tools/powershell.rs`.

### Affected files (Phase A)

| File | Change |
|------|--------|
| `src/agent/tools/bash.rs` | `.kill_on_drop(true)` |
| `src/agent/tools/cmd.rs` | `.kill_on_drop(true)` |
| `src/agent/tools/powershell.rs` | `.kill_on_drop(true)` |

### Tests (Phase A)

`kill_on_drop` is a well-tested Tokio primitive, so no new unit test is strictly
required.  Add a doc-comment to each call site explaining why `kill_on_drop` is
needed:

```rust
// kill_on_drop(true): ensures the child process is terminated if the
// enclosing Tokio task is aborted (e.g. user presses Esc).
.kill_on_drop(true)
```

---

## Phase B: explicit cancellation token

### New signature for `run_agent_loop` in `src/agent/mod.rs`

```rust
pub async fn run_agent_loop(
    mut messages: Vec<Message>,
    config: AgentLoopConfig,
    provider: Arc<dyn LlmProvider>,
    tx: UnboundedSender<AgentEvent>,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,  // ← new
    mut steering_rx: UnboundedReceiver<String>,
) {
```

### Cancellation check inside the loop

Add a check at the **start of every iteration** (before streaming the next turn):

```rust
loop {
    // Respect explicit cancellation before starting a new LLM turn.
    if *cancel_rx.borrow() {
        let _ = tx.send(AgentEvent::Done);
        return;
    }

    // drain steering, stream LLM, execute tools …
}
```

Add a second check **after each tool call** in the batch, mirroring the
existing steering-drain check:

```rust
for (idx, (id, name, args)) in pending_tool_calls.iter().cloned().enumerate() {
    // … execute tool …

    // Stop remaining tool calls if explicitly cancelled.
    if *cancel_rx.borrow() {
        // Skip remaining tools (same pattern as steering interruption).
        for (skip_id, skip_name, skip_args) in
            pending_tool_calls.iter().skip(idx + 1).cloned()
        { /* emit skipped ToolCallStart + ToolCallEnd */ }
        let _ = tx.send(AgentEvent::TurnEnd);
        let _ = tx.send(AgentEvent::Done);
        return;
    }

    // existing steering drain check …
}
```

### `App` changes in `src/app.rs`

Add a field:
```rust
agent_cancel_tx: Option<tokio::sync::watch::Sender<bool>>,
```

In `start_agent_task`:
```rust
fn start_agent_task(&mut self, llm_messages: Vec<Message>, provider: &DynProvider) {
    // …
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    self.agent_cancel_tx = Some(cancel_tx);

    self.agent_task = Some(tokio::spawn(async move {
        run_agent_loop(llm_messages, config, provider, tx, cancel_rx, steering_rx).await;
    }));
}
```

In `abort_agent_loop`:
```rust
pub fn abort_agent_loop(&mut self) {
    // Signal the loop to stop at the next inter-turn check.
    if let Some(cancel_tx) = self.agent_cancel_tx.take() {
        let _ = cancel_tx.send(true);
    }
    // Hard abort as a fallback (stops any in-flight .await immediately).
    if let Some(handle) = self.agent_task.take() {
        handle.abort();
        self.streaming = false;
        self.steering_tx = None;
        self.queued_steering.clear();
        self.messages.push(Message::assistant("[agent loop aborted]"));
        self.persist_messages();
    }
}
```

Also initialise the new field in `App::new`:
```rust
agent_cancel_tx: None,
```

### Callers that need updating

| Location | Required change |
|----------|----------------|
| `src/app.rs` `start_agent_task` | Create watch channel, pass `cancel_rx` |
| `src/main.rs` `run_print_mode` | Create a never-cancelled channel: `let (_, cancel_rx) = watch::channel(false);` |
| `src/agent/tests.rs` `run_and_collect` helper | Same: `let (_, cancel_rx) = watch::channel(false);` |
| Any other direct calls to `run_agent_loop` | Same pattern |

### Affected files (Phase B)

| File | Change |
|------|--------|
| `src/agent/mod.rs` | New `cancel_rx` parameter; two cancellation checks inside loop |
| `src/agent/types.rs` | No change |
| `src/app.rs` | `agent_cancel_tx` field; updated `start_agent_task`; updated `abort_agent_loop` |
| `src/main.rs` | Pass never-cancelled watch receiver in `run_print_mode` |
| `src/agent/tests.rs` | Pass never-cancelled receiver in `run_and_collect` and other helpers |

---

## Tests (Phase B)

Add to `src/agent/tests.rs`.

### Helper update

```rust
async fn run_and_collect(provider: MockProvider) -> Vec<AgentEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (_steering_tx, steering_rx) = mpsc::unbounded_channel();
    let (_, cancel_rx) = tokio::sync::watch::channel(false);  // ← new
    let config = AgentLoopConfig { tools: HashMap::new(), before_tool_call: None, after_tool_call: None };
    let messages = vec![Message::user("hi")];
    run_agent_loop(messages, config, Arc::new(provider), tx, cancel_rx, steering_rx).await;
    // … collect …
}
```

### New tests

```rust
/// A pre-set cancellation token causes the loop to exit before starting any turn.
#[tokio::test]
async fn agent_loop_exits_immediately_when_pre_cancelled() {
    let provider = MockProvider::new(vec![
        vec![
            LlmEvent::Token { text: "should never appear".into(), phase: AssistantPhase::Final },
            LlmEvent::Done,
        ],
    ]);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (_steering_tx, steering_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let _ = cancel_tx.send(true);  // pre-cancel before the loop even starts

    let config = AgentLoopConfig { tools: HashMap::new(), before_tool_call: None, after_tool_call: None };
    run_agent_loop(vec![Message::user("hi")], config, Arc::new(provider), tx, cancel_rx, steering_rx).await;

    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::TextToken { .. })),
        "expected no text tokens when pre-cancelled: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::Done)),
        "expected Done as terminal event: {events:?}"
    );
}

/// Cancellation after a tool call stops the loop before the next LLM turn.
#[tokio::test]
async fn agent_loop_stops_after_tool_call_when_cancelled() {
    // Turn 1: tool call (loop would normally continue to turn 2).
    // Turn 2: text (should never be reached after cancel).
    let provider = MockProvider::new(vec![
        vec![
            LlmEvent::ToolCall {
                id: "c1".to_string(),
                name: "slow_tool".to_string(),
                args: serde_json::json!({"value": "x"}),
            },
            LlmEvent::Done,
        ],
        vec![
            LlmEvent::Token { text: "second turn".to_string(), phase: AssistantPhase::Final },
            LlmEvent::Done,
        ],
    ]);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let (_steering_tx, steering_rx) = mpsc::unbounded_channel();

    let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
    tools.insert("slow_tool".to_string(), Arc::new(SlowTool));

    let config = AgentLoopConfig { tools, before_tool_call: None, after_tool_call: None };

    let handle = tokio::spawn(async move {
        run_agent_loop(vec![Message::user("hi")], config, Arc::new(provider), tx, cancel_rx, steering_rx).await;
    });

    // SlowTool sleeps 60ms; cancel after 10ms (while tool is still running).
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let _ = cancel_tx.send(true);

    handle.await.expect("agent loop join");

    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::TextToken { text, .. } if text == "second turn")),
        "second turn should not start after cancellation: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::Done)),
        "expected Done as terminal event: {events:?}"
    );
}
```

---

## Implementation order

1. **Phase A first** — zero signature changes; merge independently.
   1. Add `.kill_on_drop(true)` to the three shell tools.
   2. Run quality gates.

2. **Phase B second** — signature change; update all callers atomically.
   1. Add `cancel_rx` parameter to `run_agent_loop`.
   2. Add both cancellation checks inside the loop.
   3. Update `App::start_agent_task` and `abort_agent_loop`.
   4. Update `run_print_mode` and all test helpers.
   5. Add new tests.
   6. Run quality gates.

## Verification checklist (both phases)

1. `cargo fmt`
2. `cargo clippy --all-targets`
3. `cargo test` — all existing tests pass; new cancellation tests pass
4. Manual test: start a long `sleep 30` bash command, press Esc — verify the
   sleep process is gone from the process list (`ps aux | grep sleep`)
5. Confirm no `run_agent_loop` call site is missing the new parameter

## Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `kill_on_drop` on Windows behaves differently | Low | `TerminateProcess` is called; this is Tokio's documented Windows behaviour |
| New `cancel_rx` parameter is forgotten in a future caller | Low | Compiler error: `watch::Receiver<bool>` is not `Default`; will not compile without an argument |
| Pre-cancel test is flaky due to task scheduling | Very low | The pre-cancel check happens before the first `.await` inside the loop — it is always the first thing checked |
| Timing-dependent second test is flaky | Low | SlowTool sleeps 60ms; cancel at 10ms. Cancellation is sent during the tool's sleep and checked after it returns. Retry budget: the tool must complete before cancel is checked, which it will (it always returns after 60ms) |
