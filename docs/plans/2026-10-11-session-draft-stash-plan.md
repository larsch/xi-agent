# Plan: per-session draft stash and unified queue navigation (issue #8)

## Goal

Implement the agreed interaction for one logical sequence containing pending steering, the composer boundary, and stashed drafts. Message behavior must depend on its position in that sequence, not on separate steering-vs-draft navigation rules. Drafts remain outside conversation history until submitted.

## Queue contract

- Logical layout is `[pending steering oldest ... newest] | composer | [stashed drafts newest ... oldest]`.
- Alt+Up on ordinary non-empty composer text durably inserts it at the front of the stash and clears the composer only after persistence succeeds.
- Alt+Up from the empty composer pops the latest pending steering message from the runner/UI queue into the composer for editing. Pending steering is withdrawn immediately, before any later consumption boundary.
- Alt+Up while editing a stash entry saves edits in place (or deletes a whitespace-only entry), then moves one stash item toward the composer. Alt+Up at the top of the stash reaches the composer boundary.
- Alt+Up while editing a popped steering message saves nonblank edits at the front of the stash (blank means delete), then pops/selects the next latest pending steering message; if none remain, it reaches the composer boundary.
- Alt+Down from the empty composer selects the front stash item. Alt+Down while editing a stash entry saves/deletes it and moves to the next older stash entry; at the end it is a no-op.
- Alt+Down while editing a popped steering message saves/deletes it at the front of the stash, then selects the next stashed entry behind it (the previously-front stash item). If none exists, return to the composer boundary.
- Alt+Down never re-enqueues/resubmits a pending steering message. Enter submits the edited message: while a loop runs it is enqueued as steering; when idle it is submitted normally. Submitting a selected stash entry removes it durably only if submission/steering enqueue succeeds and applies the agreed rotation to the remaining stash order.
- Idle navigation at the composer boundary falls through to existing conversation-history stepping; while streaming, it does not.
- If persistence fails, keep the current editor text and enough selection state to retry; do not silently clear or lose the message.
- Interruptions preserve all pending steering plus any popped-but-being-edited steering exactly once. Delayed `SteeringConsumed` events reconcile recovered messages without duplication.

## Implementation approach

1. Replace the parallel steering/draft cursor state and branch-specific recall helpers with a single selection source/cursor model (composer boundary, popped steering edit, or stash index), retaining only the saved composer text needed to restore the boundary.
2. Implement one move operation that saves/deletes the selected entry, persists stash changes before moving, and computes the adjacent queue position according to the contract. Keep runner withdrawal synchronized when popping pending steering.
3. Make Enter and interruption recovery source-aware so a popped steering message is either re-enqueued, stashed, or preserved on failure; preserve existing durable sidecar, resume-discovery, and delayed-consumption behavior.
4. Keep the existing above/below queue previews and 📣/📌 icon distinction, without a literal “stashed” label.

## Tests

- Cover the full navigation matrix: boundary ↔ latest pending steering; repeated Alt+Up through steering; steering→stash via both directions; empty/one/multiple stash items; stash edits, clear/delete, and end no-op; preservation of composer text.
- Cover Enter from pending-steering and stash selections while idle/running, enqueue failure/persistence failure, queue withdrawal races, interruption while steering is popped, and delayed consumption reconciliation.
- Retain sidecar round-trip/empty cleanup, stash-only resume discovery, rendering placement/icons, and history fallback coverage.
- Run `cargo fmt --all -- --check` and `just preflight`.

## Follow-up scope: preview placeholder icons and composer separator

- Preserve the preview segment identity (`pending steering` vs `stash`) for hidden-count placeholders, so a stash-side `N more...` uses 📌 and a steering-side placeholder uses 📣.
- Place the lower input separator immediately after the composer and before the below-input stash preview.
- Add rendering regressions for placeholder icon identity and row ordering. Run `just preflight`.

## Follow-up scope: history stepping takes precedence over stash navigation

When idle session-history stepping is active, Alt+Up and Alt+Down must continue history navigation before evaluating composer or stash contents. In particular, Alt+Down exits/restores the session log and discards edits to a recalled historical message, even if that text was cleared or modified. Add a regression test for editing a recalled message and moving forward; run `just preflight`.

## Follow-up scope: completed tool blocks remain clickable during the loop

A completed `ToolResult` at the live overlay tail is currently marked streaming solely because the enclosing agent loop remains active. Mouse hit testing excludes streaming sources, so its truncated block cannot expand until the loop advances. Change rendering metadata so completed tool results are non-streaming/foldable immediately, including paired and standalone results, while preserving non-foldable behavior for genuinely streaming tool intents/arguments. Add a regression that renders a completed, truncated tool result while the overall loop is active and verifies it is foldable/click-expandable before the next model step.

## Status and workflow

Approved and implemented in the `issue-8-session-draft-stash` worktree. Verification passes with `just preflight`; see the issue #8 planning thread for acceptance and progress updates. Changes are ready for commit and push as part of the accepted wrap-up.
