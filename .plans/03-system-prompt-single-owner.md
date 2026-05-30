# Plan: Unify `system_prompt` Ownership

## Problem

The system prompt is stored in two places:

- `App::system_prompt: Option<String>` — set at startup and on reload.
- `AgentLoopConfig::system_prompt: Option<String>` — copied from `App` each
  time a turn is submitted (`app_submission.rs:42`).

Between turns, `AgentLoopConfig::system_prompt` may hold a stale copy. Any
code that reads it outside of an active turn will see the wrong value.

## Approach

**Option A (preferred):** Remove `App::system_prompt`. Store the prompt only
on `AgentLoopConfig` (or pass it as a parameter to `run_agent_loop`). Read it
from there everywhere. Update the reload path in `main.rs` to write directly
to `app.agent_config.system_prompt`.

**Option B:** Remove `AgentLoopConfig::system_prompt`. Pass the system prompt
as an explicit parameter to `run_agent_loop`, eliminating the field from the
config struct entirely.

Option A is preferred because `AgentLoopConfig` already carries it and is the
natural home for per-turn configuration.

## Affected files

- `src/app.rs` — remove field
- `src/app_submission.rs` — remove copy-on-submit
- `src/main.rs` — update read/write paths
- `src/agent/types.rs` — keep or adjust field on `AgentLoopConfig`
- `src/agent/mod.rs` — update `run_agent_loop` / loop body

## Success criteria

- Exactly one authoritative location for the system prompt.
- No window where `App` and the active config can disagree.
- All tests pass; `cargo clippy` clean.

## Risk

Medium. Touches the agent loop entry point. Validate that the reload path
(`RunResult::ReloadContext`) still updates the prompt correctly after the change.
