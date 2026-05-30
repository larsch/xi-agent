# Plan: Move `context_window_for_model` Out of `provider.rs`

## Problem

`src/provider.rs` is responsible for constructing `LlmProvider` instances
(a builder/factory concern). It also owns `context_window_for_model`, a
large hard-coded string-matching table that:

- Makes `provider.rs` depend on concrete provider types
  (`CopilotProvider::cached_context_window`, `OllamaProvider::cached_context_window`)
  for a cross-cutting metadata concern.
- Requires editing `provider.rs` on two unrelated occasions: when adding a
  new provider *and* when model context windows change.

## Approach

1. Create `src/context_window.rs`.
2. Move `context_window_for_model` (and the hard-coded table) there.
3. Move the `CopilotProvider` and `OllamaProvider` cache-lookup calls there too,
   or — better — push them behind a trait:
   ```rust
   pub trait ContextWindowCache {
       fn cached_context_window(model: &str) -> Option<usize>;
   }
   ```
   and have `context_window_for_model` accept a slice of cache sources, or use
   the existing static methods with explicit registration.
4. Update all callers of `context_window_for_model` (currently only within
   `provider.rs` and its tests).
5. Re-export from `provider.rs` for backwards compatibility if needed, or
   update callers directly.

## Affected files

- `src/provider.rs` — remove function and imports
- `src/context_window.rs` — new file
- `src/main.rs` (if it imports `context_window_for_model` directly)
- `src/app.rs` / other callers

## Success criteria

- `provider.rs` no longer imports from `llm::copilot` or `llm::ollama` for
  the context-window lookup.
- `context_window_for_model` and all its tests live in the new module.
- All existing tests pass; `cargo clippy` clean.

## Risk

Low. Pure move; no logic changes.
