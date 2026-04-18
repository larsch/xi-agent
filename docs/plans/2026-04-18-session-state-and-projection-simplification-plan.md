# Session state and projection simplification plan

Date: 2026-04-18

## Direction

Refactor session/history handling so the interaction model between the agent loop, durable session log, UI rendering, and model input is explicit, incremental, and easy to reason about.

The design goal is to make derived state intrinsic to the model rather than maintained through ad hoc cache/rebuild behavior. Normal operation should not require full projection rebuilds, and the UI should no longer rely on mutating committed display state and later reconciling it.

## Scope

In scope:
- introduce a single session-state abstraction that owns committed session history and committed derived read models
- define and implement a strict separation between committed session state and transient live turn/UI state
- make display and LLM projections incremental during normal operation
- keep pure projection functions where they remain useful as reference implementations or rebuild paths
- ensure committed conversation state enters through session events rather than direct UI message mutation
- reserve full rebuilds for session load and compaction-like semantic resets
- preserve existing behavior for resume, submit, export, retry, error, abort, and compaction flows while simplifying ownership
- update the app/agent loop/UI integration to follow the new ownership model
- add or update tests to lock in invariants around append behavior, projection updates, and duplicate prevention

Out of scope:
- changing session storage format unless required by the new model
- changing message text ownership strategy (`String` vs shared ownership)
- broader UI redesign unrelated to session/history ownership
- export format redesign beyond adapting it to the new state model

## Problem statement

The current session/history flow mixes multiple concerns:
- durable event history
- LLM-visible history
- UI-visible committed history
- transient in-flight turn state

That overlap has led to repeated regressions:
- duplicate UI appends
- projection rebuilds used to recover correctness
- “cache” behavior that is hard to reason about
- uncertainty about which structure is authoritative during a turn

The refactor should remove that ambiguity by making the authoritative data flow one-way and explicit.

## Target model

### Authoritative data flow

Normal operation should follow this model:

1. Agent/app produces domain events for committed conversation changes.
2. `SessionState` ingests those events.
3. `SessionState` updates committed read models incrementally.
4. UI renders committed history plus a separate live overlay.
5. Model input is built from the committed LLM read model.

### Ownership boundaries

#### `SessionState`
Owns committed session state:
- durable event log
- committed display read model
- committed LLM read model

Responsibilities:
- load from durable history
- ingest committed events incrementally
- expose committed display messages
- expose committed LLM messages
- rebuild only on load or semantic reset conditions such as compaction

Non-responsibilities:
- direct mutation of committed display messages from `App`
- storing transient streaming/UI scratch state

#### `LiveTurnState`
A named struct (`LiveTurnState`) owns in-flight, non-committed state for exactly one active turn:
- `assistant_content: String` — streaming assistant text accumulated so far
- `assistant_thinking: Option<String>` — streaming thinking content
- `assistant_phase: AssistantPhase` — current phase marker
- `tool_entries: Vec<LiveToolEntry>` — in-progress tool call/result pairs not yet committed
- `notices: Vec<String>` — UI-only notices (errors, export confirmations, session warnings) that are not part of committed conversation history and are not forwarded to the LLM

`LiveToolEntry` holds the tool call id, name, args, and optionally the result once received.

Responsibilities:
- accumulate streaming tokens without touching committed history
- provide the `AssistantMessage` event payload when the turn is finalised (assembled directly from its own fields, not read back from display state)
- be composable with committed display state at render time via a render helper
- be cleared entirely at turn boundaries (not rebuilt from committed state)
- hold UI-only notices for the lifetime of the session (notices are not cleared at turn boundaries; they persist until the next `new_conversation`)

#### `App`
Coordinates state transitions but does not own projection logic directly.

Responsibilities:
- route committed events into `SessionState`
- route transient streaming state into live turn/UI state
- compose UI rendering from committed display state plus live overlay
- request model input from committed LLM state

Non-responsibilities:
- directly appending committed conversation messages to a projection-owned `Vec<Message>`
- rebuilding committed projections to repair transient UI mutations

## Required invariants

1. `SessionEvent` is the sole source of truth for committed conversation history.
2. Committed display and LLM read models are updated only through event ingestion.
3. Transient live-turn/UI state is stored separately from committed session state.
4. UI rendering is a composition of committed display history and live transient overlay.
5. Full rebuilds are not used in normal append/flush flow.
6. Full rebuilds are allowed only on load, compaction, or explicit repair paths. `DisplayProjection::rebuild` must not be called outside of `SessionState`; callers outside that module must go through `SessionState` ingestion methods.
7. A committed conversation item must not be appended once through the UI path and again through the event path.
8. The `AssistantMessage` event payload is assembled from `LiveTurnState` fields directly when the turn is finalised — it is never constructed by reading back from `display_projection` or any committed state.
9. UI-only notices (errors, export confirmations, warnings) are stored in `LiveTurnState::notices` and are never backed by a `SessionEvent`. They must never appear in the LLM projection.
10. Local shell execution output is UI-only and must be stored in `LiveTurnState`, not in the committed display projection. It must never appear in the LLM projection or the event log.
11. The legacy LLM fallback path that reads from `display_projection` filtered by `include_in_llm` must be removed. The no-event-log condition it guards must be made impossible before or during this refactor (by ensuring the event log is always initialised before any turn is submitted).

## Approach

1. Define the new ownership model in code around `SessionState` and a live-turn/transient state type.
2. Move committed append logic behind `SessionState` ingestion methods.
3. Remove mutable access patterns that let the app treat committed projections as scratch buffers.
4. Convert display and LLM projections into incremental committed read models.
5. Update UI rendering to compose committed display state with transient live state rather than mutating committed display messages during streaming.
6. Restrict rebuild behavior to load and compaction paths.
7. Add tests for the new invariants and regression cases.

## Ordered implementation steps

### Step 1: Establish state boundaries
- Delete the orphaned `src/session_state.rs` file (unreferenced prototype from a previous attempt).
- Introduce `SessionState` in a new `src/session_state.rs` as the owner of committed event history and committed read models, wired directly into `App` (replacing `App::event_log` and `App::display_projection`).
- Introduce `LiveTurnState` (in `src/live_turn.rs` or inline in `src/app.rs`) with the fields defined in the ownership model above: `assistant_content`, `assistant_thinking`, `assistant_phase`, `tool_entries: Vec<LiveToolEntry>`, and `notices: Vec<String>`.
- `LiveToolEntry` holds `id`, `name`, `args`, and `result: Option<ToolResult>`.
- Gate `DisplayProjection::rebuild` so it is only callable from within `session_state.rs` (make it `pub(crate)` scoped to the module, or move it behind a `SessionState` method and make the bare method private).
- Document the boundary in code comments near both types.

### Step 2: Make committed projection updates event-driven
- Implement committed display and LLM projection/read-model update APIs around single-event or append-only ingestion.
- Keep pure projection functions available as reference implementations for parity testing and rebuild paths.
- Ensure normal events update incrementally without rebuilding.
- Define explicit invalidation/rebuild handling for compaction or equivalent semantic reset events.

### Step 3: Remove direct committed display mutation from `App`
- Audit all places where `App` pushes directly into display message storage.
- Classify each write as either:
  - committed conversation state -> convert to event ingestion
  - transient UI/live state -> move into live-turn/UI state
- Remove APIs that expose mutable committed display vectors to unrelated code.

### Step 4: Rework streaming and tool rendering flow
- Stop writing streaming tokens into `display_projection` directly. Route `ThinkingToken`, `TextToken`, `ToolIntentStart`, `ToolCallStart`, and `ToolCallEnd` into `LiveTurnState` fields instead.
- Remove `ensure_assistant_message` (which pushes an empty assistant message into `display_projection`) — the live assistant content now lives in `LiveTurnState`.
- When rendering, compose: committed display messages + live assistant entry (from `LiveTurnState`) + live tool entries (from `LiveTurnState::tool_entries`) + notices (from `LiveTurnState::notices`).
- On turn completion (`TurnEnd`), assemble the `AssistantMessage` event directly from `LiveTurnState::assistant_content`, `assistant_thinking`, and `assistant_phase` — **do not read from `display_projection`**. This eliminates the `finalise_assistant_turn_event` coupling.
- Clear `assistant_content`, `assistant_thinking`, `assistant_phase`, and `tool_entries` from `LiveTurnState` after flushing. Preserve `notices`.
- Local shell execution (`submit_shell_command`) output (tool-call/result pairs with `include_in_llm = false`) moves into `LiveTurnState::tool_entries` or a dedicated `LiveTurnState::shell_entries` field, not into committed display state.

### Step 5: Rework model-input construction
- Build LLM input from `SessionState` committed LLM read model exclusively.
- Remove the legacy fallback branch in `prepare_llm_messages` that reads from `display_projection` filtered by `include_in_llm`. To make this safe, ensure `SessionState` (and therefore the event log) is always initialised before any turn can be submitted — enforce this at the type level or with an explicit guard that panics rather than silently degrading.
- Keep system prompt prepending outside the committed session history as today.
- Ensure `LiveTurnState` content (notices, shell entries, in-progress assistant text) cannot reach model-visible history.

### Step 6: Adapt resume/export/integration paths
- Resume/load should initialize committed read models from durable history.
- Export should use committed display state or a durable-display projection path consistent with the new ownership model.
- Ensure retry, abort, and error flows map cleanly to either committed events or transient UI state.

### Step 7: Lock in regression coverage
- Add tests around:
  - incremental append behavior
  - duplicate-prevention between UI and committed history
  - `LiveTurnState` overlay not mutating committed state
  - committed event assembled from `LiveTurnState` fields, not from `display_projection`
  - compaction-triggered rebuild behavior
  - resume/load initialization
  - model input excluding `LiveTurnState` content (notices, shell entries, in-progress text)
  - UI-only notices surviving turn boundaries but not appearing in LLM projection or event log
  - local shell output not appearing in event log or LLM projection
  - absence of the legacy LLM fallback path (compile-time: the code path should not exist)

## Affected areas

Expected primary files:
- `src/app.rs`
- `src/projection.rs`
- `src/session_state.rs`
- `src/ui.rs`
- `src/main.rs`
- session/event related modules as needed

Expected test areas:
- projection tests
- session-state tests
- app integration/unit tests around event application and message flow
- UI-related tests if present for rendering composition

## Risks and assumptions

### Risks
- Streaming behavior currently relies on direct display mutation and may reveal hidden coupling when separated into `LiveTurnState`.
- Tool call/result rendering currently assumes committed and in-flight state share storage; moving tool entries into `LiveTurnState` requires the render path to compose from two sources.
- Error and abort flows may straddle the boundary between transient notices and committed conversation history.
- Compaction semantics may require careful treatment to keep display and LLM state consistent after reset.
- Partial migration could temporarily increase complexity if old and new models coexist too long.
- Local shell output has no event representation; moving it into `LiveTurnState` means it will not survive a `new_conversation` reset — this is acceptable but should be an explicit decision.
- UI-only notices (errors, export confirmations) currently persist across `new_conversation` because they live in `display_projection`; moving them to `LiveTurnState` and clearing on `new_conversation` is the correct behavior but is a visible behavior change.

### Assumptions
- The session event model is sufficient to represent all committed conversation history needed by the UI and LLM.
- UI-only notices can remain outside committed session history without breaking user expectations.
- Incremental display and LLM maintenance is feasible for normal event flow.
- Load and compaction are acceptable rebuild points.

## Success criteria

- `SessionState` is the sole owner of committed session history and committed derived read models.
- `App` no longer directly mutates committed display history for conversation events.
- `LiveTurnState` is a named struct with defined fields; streaming, tool, notice, and shell state all live there during a turn.
- The `AssistantMessage` event is assembled from `LiveTurnState` fields at turn finalisation — it is never read back from `display_projection`.
- UI rendering composes committed display history with the `LiveTurnState` overlay at render time.
- UI-only notices live in `LiveTurnState::notices` and are never backed by a `SessionEvent` and never visible in the LLM projection.
- Local shell output lives in `LiveTurnState` and never enters the event log or LLM projection.
- `DisplayProjection::rebuild` is not callable from outside `SessionState`.
- The legacy LLM fallback path (reading `display_projection` filtered by `include_in_llm`) is removed.
- Normal operation does not rebuild display or LLM projections.
- Load and compaction remain the only standard rebuild paths.
- Existing resume/export/submit/retry/error/abort behavior remains intact under the simplified ownership model.
- Duplicate UI append regressions are prevented by design rather than repaired by reconciliation.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` pass.

## Verification

Implementation verification should include:
- targeted unit tests for committed display and LLM incremental updates
- tests proving `LiveTurnState` overlay does not duplicate committed history
- tests that the `AssistantMessage` event is assembled from `LiveTurnState` fields, not from display state
- tests for UI-only notices: present in render output, absent from LLM projection and event log
- tests for local shell output: present in render output, absent from event log and LLM projection
- tests for compaction invalidation and rebuild behavior
- tests for retry/abort/error flows at the committed/transient boundary
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-features`

## Notes for execution

Keep the migration coherent. Avoid leaving behind mixed ownership where some committed conversation changes still mutate UI display storage directly while others go through session ingestion. If implementation uncovers event-model gaps, update the plan before continuing rather than adding more reconciliation behavior.
