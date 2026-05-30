# Plan: Decompose the `App` God Object

## Problem

`struct App` owns every piece of application state (UI, providers, login,
session, agent runtime, ask-user flow, step-back, shell, etc.) and its
`impl` is split across four files solely to manage line count. All fields
are `pub(crate)`, so every module can reach in and mutate state directly.
This blocks subsystem testing and makes invariants impossible to enforce.

## Approach

Incremental extraction — one cohesive domain at a time.  Each step is
independently shippable.

### Step 1 — `LoginFlowState` (already partially done via `LoginState`)

Audit what `App` delegates to `login: LoginState` vs. what it retains
directly. Move any residual login-flow fields into `LoginState`, making
it a complete owner of that domain.

### Step 2 — `ProviderSetupState`

The provider setup wizard (add/edit/remove provider, multi-step input)
lives on `ProviderManager`.  Extract it further into
`src/provider_setup_state.rs` so `ProviderManager` only holds stable
configuration, not transient wizard state.

### Step 3 — `StepBackState`

```rust
pub struct StepBackState {
    pub cursor: Option<usize>,
    pub saved_input: Option<String>,
}
```
Two fields on `App` → one sub-struct. Small but clarifies the invariant
(both fields are always set/cleared together).

### Step 4 — `AgentTurnState`

Group streaming-related fields:
```rust
pub struct AgentTurnState {
    pub status: Option<StreamingStatus>,
    pub throbber_tick: u8,
    pub last_output_at: Option<std::time::Instant>,
}
```
`App::streaming()`, `tick()`, `throbber_visible()` become methods on this struct.

### Step 5 — Review `pub(crate)` field exposure

After each extraction, tighten visibility: sub-structs should expose
behaviour (methods) rather than fields where possible.

## Affected files

- `src/app.rs` — field removals, delegation
- `src/login_state.rs` — additions
- `src/provider_manager.rs` — wizard state extraction
- `src/provider_setup_state.rs` (new)
- `src/step_back_state.rs` (new)
- All `impl App` blocks that touch moved fields

## Success criteria

- `App` struct fields reduce by ≥30% from current count.
- Each extracted struct has its own `impl` with methods covering its invariants.
- No new `pub(crate)` fields added.
- All tests pass; `cargo clippy` clean at each step.

## Risk

High overall, but each step is low-risk in isolation.  Do not attempt
multiple steps in one commit.  The most dangerous step is Step 2
(ProviderManager wizard state) because it is touched by many `app_interaction.rs`
methods.  Tackle it last.
