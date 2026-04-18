# Plan — Issue #26: Explicit steering interruption semantics in tool-batch loop

## Direction

Refactor the tool-batch interruption flow in `src/agent/mod.rs` so steering and cancellation are represented by explicit control-flow outcomes instead of the boolean `stop_after_turn_for_steering`. Preserve current user-visible behavior while making precedence and remaining-tool handling easier to read.

## Scope

### In

- Replace `stop_after_turn_for_steering` with a small explicit control-flow outcome for the current tool batch
- Extract shared handling for marking remaining tool calls as skipped/interrupted, recording both emitted events and conversation/session history
- Make interruption precedence obvious at the post-tool boundary
- Add focused tests for steering interruption and cancel-vs-steering interaction

### Out

- Broader agent-loop redesign
- UI changes
- Steering UX changes
- App-level event/channel refactors

## Ordered steps

1. **Refactor interruption control flow** (`src/agent/mod.rs`)
   - Introduce a local enum for post-tool outcomes
   - Use it to decide whether the loop should continue the batch, continue on the next assistant turn, or return

2. **Extract remaining-tool handling** (`src/agent/mod.rs`)
   - Add a helper to emit `ToolCallStart`/`ToolCallEnd` for skipped remaining calls
   - Ensure tool-call and tool-result messages plus `SessionEvent`s are recorded consistently for both steering and cancellation

3. **Make precedence explicit** (`src/agent/mod.rs`)
   - Check cancellation first at the interruption boundary
   - Check queued steering second
   - Keep the current semantics: in-flight tool completes before interruption takes effect

4. **Add focused tests** (`src/agent/tests.rs`)
   - Steering during a multi-tool batch skips the remaining calls
   - Skipped remaining tool calls are recorded with the queued-user-message reason
   - Cancellation and steering both pending at the same boundary prefer cancellation

5. **Verify**
   - Run targeted agent tests first
   - Then `cargo fmt --all`
   - Then stronger repo checks as appropriate (`cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-features`)

## Affected files

- `src/agent/mod.rs`
- `src/agent/tests.rs`
- `docs/plans/2026-04-18-issue-26-explicit-steering-interruption.md`

## Assumptions

- The intended current behavior is that cancellation wins over steering when both are pending immediately after a tool finishes
- No external API changes are needed; this is an internal refactor with behavior-preserving tests

## Risks

- Accidentally changing event order around `ToolCallEnd`, skipped calls, or `TurnEnd`
- Accidentally changing which tool results are recorded in `messages` and `session_events`
- Introducing helper abstractions that obscure rather than clarify the local flow

## Verification

- Targeted `cargo test` for the affected agent interruption tests
- `cargo fmt --all`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-features`

## Status

- [x] Step 1: Refactor interruption control flow
- [x] Step 2: Extract remaining-tool handling
- [x] Step 3: Make precedence explicit
- [x] Step 4: Add focused tests
- [x] Step 5: Verify

## Current completion point

Implemented the issue #26 refactor and verification:

- `src/agent/mod.rs`
  - added `ToolBatchInterruption` to make post-tool control flow explicit
  - extracted `record_tool_call_result` for consistent message/session-event recording
  - extracted `skip_remaining_tool_calls` for the shared remaining-tool handling used by cancellation and steering
  - added `resolve_tool_batch_interruption` so precedence is explicit at the interruption boundary
  - removed `stop_after_turn_for_steering`
- `src/agent/tests.rs`
  - kept the existing steering batch interruption test
  - added `cancellation_beats_steering_at_same_tool_boundary`
- `docs/plans/2026-04-18-issue-26-explicit-steering-interruption.md`
  - recorded the plan and implementation status

Verification completed:

- `cargo test steering_during_tool_batch_skips_remaining_tools -- --nocapture`
- `cargo test cancellation_beats_steering_at_same_tool_boundary -- --nocapture`
- `cargo fmt --all`
- `cargo test --all-features`
- `cargo clippy --all-targets --all-features -- -D warnings`
