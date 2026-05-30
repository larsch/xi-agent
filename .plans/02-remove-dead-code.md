# Plan: Remove Dead Code — Empty Section Comment, `with_hooks`, `execute`

## Problem

Three dead-code items violate the project rule ("remove unused code rather
than suppressing it"):

1. **Empty section header** in `src/app.rs` (lines ~143–144):
   ```rust
   // ── Add-provider setup state ─────────────────────────────────────────────
   // ── Runtime/task state ───────────────────────────────────────────────────
   ```
   The "Add-provider setup state" section has no fields; the state lives in
   `ProviderManager`. The comment is a leftover from a prior refactor.

2. **`DefaultToolExecutor::with_hooks`** (`src/agent/types.rs` ~line 343):
   Suppressed with `#[allow(dead_code)]` without an explaining comment. Not
   used in production or in tests (tests use `TestExecutor` instead).

3. **`Tool::execute`** (`src/agent/types.rs` ~line 258):
   Also suppressed with `#[allow(dead_code)]`. Comment says "for tests and
   callers that don't need streaming" but it is never called.

## Approach

1. Delete the empty section comment block.
2. Remove `DefaultToolExecutor::with_hooks`. Verify no test calls it; if any
   test does, inline the construction at the call site first.
3. Remove `Tool::execute`. Only `execute_live` is called in production;
   `execute` is an untested wrapper.

## Affected files

- `src/app.rs`
- `src/agent/types.rs`

## Success criteria

- No `#[allow(dead_code)]` suppressions remain on these items.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.
- All tests pass.

## Risk

Low. Dead code removal with no behavioural change.
