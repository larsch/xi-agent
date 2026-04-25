# Plan: Clean up `projection.rs` — remove dead-code suppressor and consolidate callers

Date: 2026-04-25

## Context

`projection.rs` carries `#![allow(dead_code)]` at the top (line 26). This was
added during the session-state migration (2026-04-18) when the projections were
introduced but not yet fully wired up. The migration is now complete —
`SessionState` uses `DisplayProjection` and `LlmProjection`, and
`project_llm_messages` is called from three places — but the suppressor was
never removed.

Current callers of `projection.rs` public API:

| Symbol | Callers |
|---|---|
| `project_llm_messages` | `agent/compaction.rs`, `app.rs` (×2, in tests) |
| `project_display_messages` | `session_state.rs` (export path) |
| `DisplayProjection` | `session_state.rs` (owns it) |
| `LlmProjection` | `session_state.rs` (owns it) |

The `#![allow(dead_code)]` suppresses any symbols in the file that are not
externally used. With it in place, the compiler will not warn if a helper
function or struct field becomes unused — a silent regression risk.

Additionally, the comment at line 11 says `DisplayProjection` is for "stateful
incremental UI renderer (future)" — that future is now the present;
`DisplayProjection` is used by `SessionState`. The module-level doc is stale.

---

## Scope

### In scope

1. **Remove `#![allow(dead_code)]`** from `projection.rs`.
2. **Fix any warnings** that surface after removal:
   - Unused functions, structs, or fields become visible immediately.
   - Either remove them or gate them behind `#[cfg(test)]` if test-only.
3. **Update the module-level doc comment** to reflect current reality:
   - `project_llm_messages` → used by agent compaction + tests
   - `project_display_messages` → used by `SessionState` export path
   - `DisplayProjection` → owned by `SessionState` for committed display state
   - `LlmProjection` → owned by `SessionState` for committed LLM state
   - Remove the "(future)" annotation.
4. **Audit `app.rs` test-only uses of `project_llm_messages`:** The two calls
   in `app.rs` are inside `#[cfg(test)]` blocks. Confirm they are test-only and
   note whether they could/should use `SessionState::llm_messages_for_test()`
   instead (which already exists). If the direct calls to `project_llm_messages`
   in app tests are redundant, replace them with the `SessionState` accessor and
   note if `project_llm_messages` becomes test-only itself (gate it).

### Out of scope

- Changing any projection logic or behavior.
- Moving `projection.rs` content into another module.
- The broader session-state refactor (already completed).

---

## Affected files

- `src/projection.rs` (primary)
- `src/app.rs` (test-only call sites, minor)

---

## Assumptions

- After removing the pragma, all currently-used symbols in `projection.rs` will
  be reachable from their callers — no real dead code exists.
- If any symbol is truly dead (no callers at all), it should be deleted rather
  than annotated.

---

## Risks

- Low risk — purely additive (removing a suppressor, updating docs, deleting
  dead code). No behavior changes.
- The compiler may surface one or two unused private helpers; these need case-
  by-case decisions (delete vs. move to `#[cfg(test)]`).

---

## Verification

- `just preflight` passes with zero `dead_code` warnings.
- No `#![allow(dead_code)]` or `#[allow(dead_code)]` remains in `projection.rs`.
- Module-level doc accurately describes each public symbol and its consumer.

---

## Ordered steps

1. Remove `#![allow(dead_code)]` from `projection.rs`.
2. Run `cargo check --all-targets --all-features` and list all new warnings.
3. For each warning: delete unused code, gate behind `#[cfg(test)]`, or confirm
   it is used and the warning is spurious.
4. Update module-level doc comment.
5. Audit `app.rs` test-only calls to `project_llm_messages`; replace if a
   better accessor exists.
6. `just preflight`.
