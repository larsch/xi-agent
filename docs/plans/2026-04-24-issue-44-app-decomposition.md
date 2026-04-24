# Plan: App decomposition — incremental sub-struct extraction (Issue #44)

**Date:** 2026-04-24  
**Issue:** https://gitea.belunktum.dk/larsch/tau/issues/44  
**Objective:** Improve encapsulation, testability, and derisk new features by
decomposing `App` into coherent sub-structs. Each step is independently
shippable, leaves tests green, and makes no behavior changes.

---

## Scope

Five steps covering the highest-value, lowest-risk seams. Each step is one
commit. Steps are ordered from least to most coupling.

Out of scope for this plan (deferred to follow-on work):
- Unifying all mode flags into a single `UiMode` state machine
- Extracting `ProviderManager` (auth flows, token refresh, provider list)
- Reducing `pub` field visibility on `App` (natural follow-on once sub-structs
  are stable)

---

## Steps

### Step 1 — Extract `CompletionState` to `src/completion_state.rs` ✅ DONE

**Fields moved from `App`:**
- `completions: Vec<CompletionItem>`
- `completion_selected: usize`
- `available_models: Option<Vec<String>>`
- `models_loading: bool`
- `model_fetch_error: Option<String>`

**Methods moved from `App`:**
- `update_completions`
- `should_fetch_models`
- `start_model_fetch`
- `apply_model_list`
- `completion_select_next`
- `completion_select_prev`
- `apply_completion`

`App` holds `pub completion: CompletionState` (fields `pub(crate)`) and delegates. External callers
(`main.rs`, `ui/`) access via `app.completion.*` or the existing delegating
methods.

> **Implementation note:** `update_completions`, `should_fetch_models`,
> `start_model_fetch`, `apply_model_list`, and `apply_completion` were not
> moved to `CompletionState` because they require `App`-owned fields
> (`textarea`, `thinking_supported`, `loaded_skills`, `provider_instances`,
> `selection`, auth helpers). They remain on `App` accessing completion fields
> via `self.completion.*`.

**Why first:** Cleanest seam. No tokio handles, no async, no cross-domain
side-effects. Easy to unit-test in isolation.

---

### Step 2 — Move `SelectionState` to `src/selection_state.rs`

`SelectionState` is already a named struct in `app.rs`. Move it (and its
`impl`, `SelectionKind`, and related private helpers) to its own module.

`App` holds `pub selection: SelectionState`. All method calls on `selection`
remain unchanged — only the definition moves.

**Why:** Reduces `app.rs` by ~150 lines. Enables independent testing of the
selection state machine.

---

### Step 3 — Move `LoginState` to `src/login_state.rs`

`LoginState` is already a named struct in `app.rs`. Move it (and `impl
LoginState`, `LoginActionKind`, `AuthFlow` re-export) to its own module.

`App` holds `pub login: LoginState`. Delegation unchanged.

**Why:** ~100 lines out of `app.rs`. Login flow is a clear domain with its own
lifecycle and clipboard ownership — good isolation target.

---

### Step 4 — Move `AskUserState` to `src/ask_user_state.rs`

`AskUserState` + `PendingAsk` are already local structs. Move them to a new
module along with all ask-user methods currently on `App`:
- `has_pending_ask`
- `pending_ask_allows_freeform`
- `ask_user_selection_no_freeform`
- `receive_ask_request`
- `enter_ask_freeform_mode`
- `begin_ask_freeform_typing`
- `cancel_ask_freeform_typing`
- `submit_pending_ask_answer`
- `select_pending_ask_option`
- `cancel_pending_ask`
- `finish_pending_ask` (private)

`App` holds `pub(crate) ask_user: AskUserState` and thin delegating methods
where callers expect them on `App`.

**Why:** Self-contained lifecycle (receive → answer/cancel). The methods never
touch session state or agent runtime. Ideal isolation for unit tests.

---

### Step 5 — Move `AgentRuntime` to `src/agent_runtime.rs`

`AgentRuntime` is already a named struct. Move it and expose a cleaner API:
- `new() -> Self`
- `app_event_tx(&self) -> AppEventTx`
- `recv_app_event(&mut self) -> Option<AppEvent>`
- `queued_steering(&self) -> &[String]`
- `start_task(...)` / `abort_task(...)` (wrapping current inline logic in `App`)
- `is_running(&self) -> bool`

`App` holds `runtime: AgentRuntime` (private). The coordination methods
(`start_agent_task`, `abort_agent_loop`) remain on `App` but delegate to
`runtime` for handle/channel management.

**Why:** Isolates all `JoinHandle` + cancellation channel ownership. Makes
agent lifecycle testable without constructing a full `App`. Moderate risk
because `start_agent_task` also touches session and live_turn — the split is
at the handle-management boundary only.

---

## Verification approach

After each step:
1. `just preflight` must pass (fmt + clippy + tests + check)
2. Manual smoke test: launch tau, send a message, verify streaming works
3. No behavior change — this is pure structural refactoring

---

## Success criteria

- `app.rs` reduced from ~4,676 lines toward ~3,000 lines
- Each extracted module has its own `#[cfg(test)]` block covering its state
  machine
- `App` fields for each extracted domain are private or `pub(crate)` where
  external access is not needed
- `just preflight` passes after every commit

---

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Wide `pub` field access in `main.rs`/`ui/` breaks after move | Audit call sites before each step; add delegating accessors as needed |
| Test helpers (`make_app`) become hard to construct | Keep `make_app` in `app.rs`; it can construct sub-structs directly |
| Step 5 `AgentRuntime` coordination logic is tangled | Only move handle/channel fields; leave orchestration on `App` for now |
