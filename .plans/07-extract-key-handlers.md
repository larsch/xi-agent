# Plan: Extract Key Handlers from `main.rs`

## Problem

`src/main.rs` (2,028 lines) mixes terminal setup/teardown, the main event
loop, and all keyboard command handlers:

- `handle_key_event` (~65 lines)
- `handle_global_key_shortcuts` (~65 lines)
- `handle_shell_mode_key` (~31 lines)
- `handle_selection_mode_key` (~82 lines)
- `handle_selection_enter` (~83 lines)
- `handle_chat_mode_key` (~91 lines)
- `handle_chat_submit` (~130 lines)
- `handle_slash_submit` (~130 lines)

All handlers take `&mut App` and are therefore untestable without a live
terminal. They also directly emit `AppEvent`s and manipulate `App` state,
tightly coupling input processing to the event loop.

## Approach

1. Create `src/input/mod.rs` (or `src/input.rs`).
2. Move all `handle_*` free functions there. They can remain free functions
   taking `(&mut App, …)` — no need to make them methods yet.
3. Move the `KeyDispatch` and `RunResult` enums to the same module (or to
   `src/app_event.rs` if they're logically app-level).
4. Move `handle_slash_submit` logic that maps `CommandAction` → `App` mutation
   into a method on `App` (e.g. `App::dispatch_slash_command`), enabling unit
   tests that bypass the terminal.
5. `main.rs` becomes: CLI parsing + terminal lifecycle + event loop wiring
   only.

## Affected files

- `src/main.rs` — remove handler functions, keep `main()` and `run_interactive()`
- `src/input.rs` (new) — handler functions
- `src/app.rs` or `src/app_interaction.rs` — new `dispatch_slash_command` method

## Success criteria

- `main.rs` is under ~600 lines.
- At least `handle_slash_submit` (the most complex handler) is covered by a
  unit test.
- `cargo clippy` clean, all tests pass.

## Risk

Medium. Large mechanical move; logic unchanged. The biggest risk is missing
a use/import. Migrate one handler at a time, running `cargo check` between each.
