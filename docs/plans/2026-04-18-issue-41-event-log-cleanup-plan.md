# Issue #41 plan — complete event-log migration cleanup

Date: 2026-04-18

## Scope

Complete the remaining cleanup and integration work for the append-only session event log refactor tracked in Gitea issue #41.

In scope:
- identify remaining old app/UI session-display paths that still depend on `Vec<Message>` and revision-based log caching
- migrate those paths to derive display state from the event log / projection layer
- remove obsolete state and cache machinery once no longer needed
- keep session persistence, resume, export, and provider request preparation working
- verify with targeted inspection plus full Rust preflight

Out of scope:
- new compaction behavior beyond what is already implemented
- unrelated UI redesigns
- issue-management follow-up beyond commenting/updating #41 after technical verification

## Approach

1. Inspect the remaining `App.messages`, `log_revision`, `cached_log_lines`, and UI rendering uses.
2. Determine the minimal safe migration path so visible log rendering reads from projection-backed state rather than duplicated mutable message state.
3. Remove obsolete fields/helpers/call sites once the event-log-backed path is complete.
4. Run formatting, clippy/tests/checks via `just preflight`.
5. If verified, comment on and close/update issue #41 appropriately.

## Success criteria

- `App` no longer keeps obsolete message-history state that duplicates event-log-derived session history.
- UI rendering no longer relies on the old blunt revision cache mechanism for session-log invalidation.
- Session resume, submit, export, and LLM projection still work.
- `just preflight` passes.

## Affected areas

- `src/app.rs`
- `src/ui.rs`
- possibly `src/ui/log.rs`
- projection/event-log integration points
- Gitea issue #41 follow-up comments/closure

## Risks / assumptions

- There may still be transient in-flight UI state that legitimately needs non-event-log message storage; if so, only durable/history duplication should be removed.
- UI rendering code may still rely on revision invalidation for non-session-log reasons; if so, replace only the obsolete portion, not unrelated redraw behavior.
- The prior issue comment may have overstated completion, so final issue update should accurately describe what was actually finished now.

## Verification

- targeted compile migration check: `cargo check --all-targets --all-features`
- full repo preflight passed via `just preflight`
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --all-features`
  - `cargo check --all-targets --all-features`
- observed test count after this cleanup: 524 passing
