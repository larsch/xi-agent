# Plan: App behaviour extraction — completing the sub-struct refactor

Date: 2026-04-25
Status: **Phase 1, 2, 3a/b/c/d complete (2026-04-25)**

## Context

Previous refactoring rounds (issue #44, 2026-04-24) extracted data into sub-structs
(`ProviderManager`, `SessionManager`, `AskUserState`, `SelectionState`, `LoginState`,
`AgentRuntime`, `CompletionState`) but explicitly deferred moving any *methods*.
Every sub-module file carries the same comment: "methods remain on `App` because they
require fields owned by `App` beyond this struct's scope."

That deferral is now permanent: `app.rs` is still ~4,486 lines with 196 methods on a
single `impl App`. The sub-structs are pure data holders with only `new()` and
`default()`.

The "needs App fields" justification is circular. The methods were left on `App`
because they call other methods on `App` (e.g. `exit_selection_mode`,
`reset_textarea`, `bump_log_revision`). But those cross-cutting leaf operations also
live on `App` only because everything historically lived there. None of them require
the full `App` to exist.

This plan addresses two complementary problems:

1. **Cross-cutting leaf operations** — `exit_selection_mode`, `reset_textarea`,
   `bump_log_revision`, etc. are defined on `App` but only touch one sub-struct or
   a trivial field. Move them to their sub-structs or make them free functions.

2. **`apply_agent_event` monolith** — a ~240-line `match` that handles live-turn
   mutation, session event creation, token counting, scroll behavior, and log
   revision bumping for every `AgentEvent` variant, all inline. Each arm touches a
   narrow set of fields and should be a free function or delegating method.

---

## Scope

### In scope

#### Phase 1 — Leaf operations onto sub-structs

Move or re-home these cross-cutting operations that currently block method migration:

- `SelectionState::reset(&mut self)` — inline equivalent of `exit_selection_mode`'s
  selection clearing half.
- `SelectionState::activate(&mut self, kind, title, items)` — the common
  "set items + set kind + set active + scroll to default" pattern repeated in
  every `enter_*_selection_mode` method.
- `SelectionState::apply_filter(&mut self)` — currently `apply_selection_filter` on
  `App`, only touches `self.selection`.
- `SelectionState::ensure_visible(&mut self)` — currently `ensure_selection_visible`
  on `App`, only touches `self.selection`.
- `CompletionState::update(textarea: &TextArea, ...)` — `update_completions` only
  reads `textarea` and writes `self.completion`. Can take `textarea` as a parameter.
- `bump_log_revision` reduced to `self.log_revision = self.log_revision.wrapping_add(1);
  self.cached_log_lines = None;` — keep on `App` but inline at call sites, or
  extract just those two fields into a `LogCache` struct with a `invalidate()`
  method.

After Phase 1, the justification "can't move because calls exit_selection_mode" no
longer applies to the majority of methods.

#### Phase 2 — Decompose `apply_agent_event`

Split the `apply_agent_event` match into one private method (or free function taking
explicit parameters) per `AgentEvent` variant:

- `handle_thinking_token(live_turn: &mut LiveTurnState, last_output_at: &mut ..., token: String)`
- `handle_text_token(live_turn, last_output_at, text, phase)`
- `handle_status_update(streaming_status: &mut ..., last_output_at, msg)`
- `handle_steering_consumed(runtime: &mut AgentRuntime, session: &mut SessionManager, text)`
- `handle_compaction_done(session, latest_usage, auto_scroll, ...fields...)`
- `handle_turn_end(session, streaming_status, ...)`
- `handle_done(session, streaming_status, ...)`
- `handle_provider_error(session, streaming_status, ...)`
- etc.

Each handler takes only the fields it actually needs by mutable reference. This is
possible in Rust by destructuring `self` before the match or by using `&mut` borrows
of individual fields:

```rust
let App { session, runtime, streaming_status, latest_usage, .. } = self;
handle_compaction_done(session, latest_usage, ...);
```

After Phase 2, the 240-line `apply_agent_event` becomes a thin dispatcher that
delegates to named, individually-testable functions.

#### Phase 3 — Migrate provider-setup and session methods to sub-structs

With Phase 1 unblocking cross-cutting dependencies, migrate the methods whose
comments say "remains on App":

**`ProviderManager`:**
- `enter_provider_name_input_mode`, `submit_provider_name_input`
- `enter_provider_endpoint_input_mode`, `submit_pending_provider_base_url`
- `enter_provider_api_key_input_mode`, `submit_pending_provider_api_key`
- `finish_pending_provider_setup`, `clear_pending_provider_setup`
- `enter_provider_backend_preset_selection_mode`, `set_pending_provider_backend_preset`
- `enter_provider_api_type_selection_mode`, `set_pending_provider_api_type`
- `pending_provider_instance`, `pending_provider_setup_is_edit`
- `enter_provider_removal_confirmation_mode`, `clear_pending_provider_removal`
- `record_model_changed`, `record_thinking_level_changed`

These need `textarea` only for pre-fill/reset — pass it as a `&mut TextArea` parameter.

**`AskUserState`:**
- `receive_ask_request`, `enter_ask_freeform_mode`, `begin_ask_freeform_typing`
- `cancel_ask_freeform_typing`, `submit_pending_ask_answer`
- `select_pending_ask_option`, `cancel_pending_ask`, `finish_pending_ask`

These need `selection` and `textarea` — pass as parameters.

**`SessionManager`:**
- `init_session_persistence`, `refresh_resume_availability`
- `ensure_session_id`, `ensure_event_log_for_submit`
- `persist_messages`, `append_user_message`
- `flush_turn_events`, `append_event_immediate`
- `new_conversation` (session parts only)

**`LoginState`:**
- `start_login`, `cancel_login`, `apply_login_event`
- `apply_login_action`, `enter_login_action_menu`

`App` retains thin delegating wrappers only where external callers (`main.rs`,
`ui/`) call through `app.*`.

### Out of scope

- Unifying `InputMode` + `SelectionKind` into a top-level `UiMode` state machine
  (deferred; own issue if warranted)
- Reducing `pub`/`pub(crate)` visibility (natural follow-on once methods are stable)
- Moving key-handler functions from `main.rs` (separate plan)
- LLM provider changes

---

## Affected files

- `src/app.rs` (primary — shrinks significantly)
- `src/provider_manager.rs`
- `src/session_manager.rs`
- `src/ask_user_state.rs`
- `src/selection_state.rs`
- `src/completion_state.rs`
- `src/login_state.rs`
- `src/agent_runtime.rs`
- `src/main.rs` (call-site updates)
- `src/ui/` submodules (call-site updates)

---

## Assumptions

- Rust's field-level borrow splitting is sufficient: `let App { selection, session, .. } = self`
  before calling free functions eliminates the "can't borrow two fields" issue.
- Existing tests in `app.rs` can be migrated to the sub-module's `#[cfg(test)]` block
  as methods move.
- No behavior changes — pure structural refactoring.

---

## Risks

- **Borrow conflicts in `apply_agent_event`:** Some arms read `self.session` and also
  call `self.bump_log_revision()`. After Phase 1 inlines `bump_log_revision`, these
  can be split.
- **`main.rs` call sites:** ~40 call sites in `main.rs` use `app.method()`. After
  methods move, these become `app.sub.method()` or free function calls. Compile
  errors will guide this — do incrementally.
- **Test construction:** `make_app()` in `app.rs` constructs sub-structs. Tests that
  currently call `app.enter_*` will become `app.provider.enter_*(...)` — mechanical
  but widespread.

---

## Verification

- `just preflight` passes after each phase.
- `app.rs` reduces to ≤ 2,500 lines after Phase 3.
- Each sub-module grows a `#[cfg(test)]` block testing its methods in isolation
  (no `App` construction needed).
- No behavior changes: existing integration tests in `app.rs` all pass.

---

## Ordered steps

1. **Phase 1a** — Add `SelectionState::activate`, `reset`, `apply_filter`,
   `ensure_visible`. Remove the `App`-level wrappers, update call sites.
2. **Phase 1b** — Add `CompletionState::update(textarea)`. Inline
   `bump_log_revision` to two lines (or extract `LogCache`).
3. **Preflight.**
4. **Phase 2** — Split `apply_agent_event` into per-variant free functions.
   `apply_agent_event` becomes a 20-line dispatcher.
5. **Preflight.**
6. **Phase 3a** — Migrate `ProviderManager` methods.
7. **Phase 3b** — Migrate `AskUserState` methods.
8. **Phase 3c** — Migrate `SessionManager` methods.
9. **Phase 3d** — Migrate `LoginState` methods.
10. **Final preflight** — verify line count and test coverage.
