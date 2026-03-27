# Brainstorm: `run_interactive` tool

**Date:** 2026-03-27  
**Status:** Brainstorm complete — not yet planned

---

## Goal

Add a built-in tool that lets the model run an interactive program (e.g. `vim`,
`htop`, a REPL, a test suite that needs stdin) and hand the terminal over to
the user for the duration of the child process, then return to tau when it
exits.

---

## What exists today

- `BashTool` runs commands via `tokio::process::Command`, captures stdout/stderr
  as text, and returns a `ToolResult` string. The user never sees live output.
- The TUI holds a raw-mode alternate-screen terminal the entire time
  (`enable_raw_mode`, `EnterAlternateScreen` in `main.rs`).
- `AskUserTool` demonstrates the channel pattern for tools that need to
  interact with the main/TUI thread: sends a request over an unbounded channel,
  blocks on a oneshot reply.
- There is no existing mechanism to hand the terminal to a child process and
  reclaim it.

---

## Proposed design

### Unix

1. A new `RunInteractiveTool` holds a channel sender
   (`RunInteractiveRequestTx`), constructed alongside `AskUserTool` in
   `register_builtin_tools`.
2. `execute` sends a `RunInteractiveRequest { command, reply }` over the
   channel and awaits the oneshot reply.
3. In `main.rs`, the event loop drains the channel each tick. On receipt it:
   - Leaves alternate screen, disables raw mode, pops keyboard enhancement
     flags (full TUI teardown).
   - Runs `sh -c <command>` synchronously (blocking, inherited stdio).
   - Re-enters alternate screen, enables raw mode, pushes keyboard flags
     (full TUI restore).
   - Sends back `RunInteractiveResult { exit_code }`.
4. The tool returns `"exit 0"` or `"exit N"` as the tool result.

**Why delegate to main via channel?**  
TUI teardown/restore (`crossterm` stdout operations) must happen on the thread
that owns the terminal. The tool runs in a tokio task. The `ask_user` channel
pattern is the established way to bridge this.

### Windows

- No TUI teardown needed — tau keeps its own window.
- Spawn the command in a new console window using `cmd /c start /wait
  <command>`.
- Wait for exit; propagate exit code.
- Can run directly in the tool via `tokio::task::spawn_blocking` — no channel
  or main-thread delegation required.

---

## Constraints & risks

| # | Concern |
|---|---------|
| 1 | **Terminal hand-off (Unix)**: teardown/restore sequence must be safe even if the child panics or is killed. Use a guard or explicit cleanup path. |
| 2 | **Keyboard enhancement flags**: must be popped before and pushed again after, matching the state captured at startup. |
| 3 | **Output capture**: child's stdio is attached to the real terminal; the model only receives the exit code. |
| 4 | **Focus (Windows)**: the new window may not receive focus automatically depending on the terminal emulator. Document as a known limitation. |
| 5 | **`start /wait` reliability (Windows)**: works for most programs but may not propagate exit codes perfectly for all console apps. |

---

## Success criteria

- `run_interactive` appears in the tool list on both Unix and Windows.
- Calling it with e.g. `{"command": "vim README.md"}` hands the terminal to
  vim (Unix) or opens a new console window (Windows), and tau resumes when it
  exits.
- Tool result reports the exit code.
- All existing tests pass; clippy is clean; no compiler warnings.

---

## Next step

Load the `plan` skill and turn this into an ordered implementation plan.
