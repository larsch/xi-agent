# Plan: Deduplicate Thinking-Support Check in `handle_slash_submit`

## Problem

In `src/main.rs` — `handle_slash_submit` — the exact same expression appears
twice (once for `CommandAction::Thinking`, once for `CommandAction::ThinkingNoArg`):

```rust
let thinking_supported = config
    .find_provider(&app.provider.current_instance.id)
    .map(|inst| {
        thinking_support_for_instance(inst, &app.provider.current_model)
            == ThinkingSupport::Applied
    })
    .unwrap_or(false);
```

Additionally, when `CommandAction::Thinking` is matched but thinking is not
supported, the command is silently dropped with no feedback to the user.

## Approach

1. Extract a free function:
   ```rust
   fn thinking_supported_for_current_provider(app: &App, config: &XiConfig) -> bool { … }
   ```
2. Call it from both match arms.
3. In the `Thinking` arm, push a notice message when thinking is not supported
   (consistent with how invalid levels are reported).

## Affected files

- `src/main.rs`

## Success criteria

- Single call site for the thinking-support check logic.
- Unsupported thinking command produces a visible notice.
- `cargo clippy` clean, all tests pass.

## Risk

Low. Logic is unchanged except for the new user-visible notice.
