# Plan: Session Step-Back via Branching

**Date:** 2026-05-05  
**Status:** Approved, ready to build

---

## Goal and scope

Allow the user to step back through `UserMessage` boundaries in the active
session log using `Alt+Up` / `Alt+Down`, edit the repopulated input field,
and resubmit — creating a **new branched session** from the kept events plus
the new message. The original session log is never modified.

### In scope
- `step_cursor: Option<usize>` + `saved_input: Option<String>` in `App`
- `Alt+Up` / `Alt+Down` / Escape / Enter key handling
- Split log rendering: kept (normal) + discarded (dimmed)
- New session creation from a subset of events on Enter
- Status bar indicator while stepping
- Scroll-to-cutpoint with context on both sides

### Out of scope
- Tool-result-level stepping
- Provenance / `BranchedFrom` event linking original and branch
- Step-back while agent loop is running (no-op guard only)
- Cache optimization (deferred)

---

## Approach

A `step_cursor: Option<usize>` in `App` holds the event index of the
`UserMessage` at the current cutoff. While it is `Some(i)`:

- Display renders `events[0..i]` normally and `events[i..]` dimmed (post-processing all spans with `DarkGray + DIM`).
- The textarea is repopulated with the content of the `UserMessage` at `i`.
- A status bar note shows `[step back: N of M]`.

On Enter: a new session is created from `events[0..i]` plus the (possibly
edited) textarea content as a new `UserMessage`. The active session switches
to the branch. The original session is untouched.

On Escape / stepping forward past the end: `step_cursor` clears and the
saved input is restored.

---

## Code-level done conditions

| Symbol / area | Done condition |
|---|---|
| `App::step_cursor` | Field present; set/cleared correctly; never set while loop active |
| `App::saved_input` | Saved on first `Alt+Up`; restored on cancel/forward-to-end |
| `App::user_message_boundaries()` | Returns `Vec<usize>` of event indices; unit tested |
| `App::step_back()` | Moves cursor to previous boundary; saves input on first call; updates scroll |
| `App::step_forward()` | Moves cursor forward; clears state at end |
| `App::cancel_stepping()` | Equivalent to stepping all the way forward |
| `App::commit_step_branch()` | Creates new session; switches active session; clears step state |
| `dim_lines(Vec<Line>) -> Vec<Line>` | Free fn in `ui/log.rs`; DarkGray+DIM on all spans; unit tested |
| Split render path | `build_log_lines_cached` uses split render when `step_cursor` is `Some`; cache keyed on `(revision, step_cursor, width)` |
| Status bar | Shows `[step back: N of M]` when `step_cursor` is `Some` |
| Key bindings | `Alt+Up` / `Alt+Down` in `main.rs`; guarded against active loop and shell mode |
| `EventLog::new_from_events` | Constructor; writes events to a fresh path immediately; unit tested |
| `SessionStore::create_session_from_events` | Creates session, writes initial events; returns new session ID; unit tested |

---

## Ordered steps

1. **`EventLog` / `SessionStore`**
   - Add `EventLog::new_from_events(path: PathBuf, events: &[SessionEvent]) -> anyhow::Result<Self>`
   - Add `SessionStore::create_session_from_events(cwd: &str, events: &[SessionEvent]) -> anyhow::Result<String>`
   - Unit tests for both

2. **`App` state fields**
   - Add `step_cursor: Option<usize>` and `saved_input: Option<String>` to `App`
   - Initialize both to `None` in `App::new`

3. **Step navigation methods on `App`**
   - `user_message_boundaries() -> Vec<usize>`
   - `step_back()` — save input on first call; update cursor, textarea, scroll
   - `step_forward()` — advance cursor; clear state at end
   - `cancel_stepping()` — same as stepping to end
   - All no-ops when `runtime` is active

4. **`commit_step_branch()`**
   - Takes textarea content as new `UserMessage` text
   - Calls `create_session_from_events` with `events[0..step_cursor]`
   - Switches `session.current_session_id` and `session.session_state`
   - Clears `step_cursor` / `saved_input`
   - Falls through to normal submission

5. **Key bindings in `main.rs`**
   - `Alt+Up` → `app.step_back()`
   - `Alt+Down` → `app.step_forward()`
   - `Enter` while `step_cursor.is_some()` → `app.commit_step_branch()` then submit
   - `Escape` while `step_cursor.is_some()` → `app.cancel_stepping()`

6. **Split rendering**
   - Add `dim_lines(Vec<Line<'static>>) -> Vec<Line<'static>>` to `ui/log.rs`
   - Update `build_log_lines_cached` to split on `step_cursor` when `Some`
   - Cache key: `(revision, step_cursor, width)`
   - Scroll target: line index at boundary, viewport centered

7. **Status bar**
   - `render_activity` in `ui/status.rs` shows `[step back: N of M]` when stepping

8. **Tests**
   - `user_message_boundaries()` correct indices
   - Step cycle correctness
   - `commit_step_branch()` new session event contents
   - `dim_lines()` span styles

---

## Affected files

- `src/event_log.rs`
- `src/session.rs`
- `src/app.rs`
- `src/app_submission.rs`
- `src/main.rs`
- `src/ui.rs`
- `src/ui/log.rs`
- `src/ui/status.rs`

---

## Risks and assumptions

- `Alt+Up` must be intercepted before tui-textarea processes it (already the
  pattern for other `Alt` keys in `main.rs` — confirmed safe).
- `project_display_messages` is a pure function over a slice, so rendering a
  sub-slice of events is correct without any changes to the projection.
- Scroll-to-boundary requires the line count of the kept render buffer, which
  is available after the first `build_log_lines` call.

---

## Verification

- `cargo test` — all unit tests pass including new ones
- Manual: step back in multi-turn session → correct dimming and input
- Manual: edit and Enter → new session in session list; original unchanged
- Manual: Escape while stepping → state fully restored; no new session
- Manual: `Alt+Up` while loop active → no-op
- `just preflight` passes clean
