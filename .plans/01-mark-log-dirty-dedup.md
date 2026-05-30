# Plan: Deduplicate `bump_log_revision` / `mark_log_dirty`

## Problem

`App` has two methods that both call `self.log_view.invalidate()` with no
behavioural difference:

```rust
pub(crate) fn bump_log_revision(&mut self) { self.log_view.invalidate(); }
pub fn         mark_log_dirty(&mut self) { self.log_view.invalidate(); }
```

~25 call sites across `app.rs`, `app_agent_handlers.rs`, and
`app_interaction.rs` use whichever name happened to be convenient, creating
reader confusion about whether the two operations differ.

## Approach

1. Pick one canonical name — `invalidate_log_cache` — with `pub(crate)` visibility.
2. Remove the other method.
3. Update all call sites (automated search-and-replace).

## Affected files

- `src/app.rs`
- `src/app_agent_handlers.rs`
- `src/app_interaction.rs`

## Success criteria

- Exactly one method on `App` that calls `log_view.invalidate()`.
- `cargo clippy` clean, all tests pass.

## Risk

Low. Pure rename; no logic changes.
