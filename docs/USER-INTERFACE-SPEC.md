# User Interface Specification

## Command line interface

- Start in interactive mode by default.
- Support non-interactive mode with `-p/--print` flag, which prints the result of each command to stdout instead of rendering a UI.
- In interactive mode, `--provider <name>` must exactly match one of the configured provider instance names shown by `/provider`, or `test`; unknown values are an error.
- In non-interactive mode, `--print` requires `--provider <name>`.
- In non-interactive mode, `--provider <name>` must exactly match one of the configured provider instance names shown by `/provider`, or `test`; unknown values are an error.
- User-facing provider errors in both interactive and non-interactive modes should be rendered in natural English first, followed by the original provider message as supporting detail.
- Error text should identify the active backend/provider label (for example `Open WebUI`) rather than leaking the transport/API name used underneath.

# Interactive UI

- The primary chat interface is a vertical stack with fixed region order:
  1. output viewport
  2. control band
  3. input area
- The control band rows have a strict top-to-bottom order:
  1. activity row (throbber / spinner)
  2. pending steering rows
  3. provider-status row (provider/system status text)
  4. interactive rows
- Interactive rows are mutually exclusive by mode:
  - menu rows (completions / selection) in normal interaction mode
  - login panel rows during `/login <provider>`
- Any control-band row may be hidden when inactive, but relative ordering is invariant.
- Render a scrollable, paginated view of the session history, with the most recent command at the bottom.
- Tool invocations are rendered as "<icon> <args>", where <icon> is a visual representation of the tool (e.g. a terminal icon for `bash`, a pencil for `edit`, etc.).
  - Shell tool calls (`bash` / `cmd` / `powershell`) show the command with embedded newlines preserved, up to 5 lines. When truncated, the display shows `…` on its own line.
  - `👀` read_file
  - `✏️` write_file
  - `📝` edit_file
  - `🔍` find_files
  - `💻` shell tools (`bash` / `cmd` / `powershell`)
  - `❓` ask_user
- System message/status icons:
  - `ℹ️` system/info (neutral system message)
  - `⚙️` system/state (configuration/environment change)
  - `⚠️` system/warning (non-fatal issue)
  - `❌` system/error (failure)
  - `✅` system/success (completed successfully)
- Assistant output uses phase-aware icons:
  - `🧠` for model thinking tokens (rendered in dimmed style)
  - `💭` for provisional assistant output (tool intent/tool-calling turn)
  - `💬` for final assistant output
- Streaming/status-row behavior:
  - On turn start, the activity row is shown immediately to indicate the system is waiting for stream/tool activity.
  - While visible assistant/tool output is actively arriving, the activity row is hidden.
  - If a turn is still active but output is temporarily idle, the activity row may reappear after a short delay.
  - The output viewport remains bottom-aligned at all times. The UI must not render trailing empty rows after the current output.
  - Control-band row toggles (activity row, pending steering rows, provider-status row, interactive rows) should avoid abrupt one-line jumps at stream boundaries; transitions should preserve stable output positioning.
- Assistant block rendering must be stable across streaming and committed states:
  - Leading and trailing empty lines/whitespace are not rendered.
  - If later chunks introduce non-whitespace text that makes previously hidden whitespace interior, that whitespace is rendered as part of the block.
  - The same trimming/presentation rules apply during streaming and after commit.
- User input is a text input field at the bottom of the screen, where users can type commands and submit them by pressing Enter.
- `Ctrl+I` toggles a one-line info bar (provider, model, thinking level, and context window). When provider-reported token usage is available and the model context window is known, the context section shows utilization as `used / max (percent)`; otherwise it falls back to showing only max context (or `unknown`).
- When automatic or manual compaction runs, the UI shows a visible `compacting…` status and then a `[compacted: Xk → Yk tokens]` marker in the session log.
- Slash commands include `/compact [instructions]`, which triggers immediate context compaction and passes optional user guidance into the compaction summary prompt.
- While the agent loop is running, Enter enqueues the typed text as a **steering message**. Queued steering messages are shown at the bottom of the chat log with a `🕹️` icon. When the agent loop consumes a steering message (at the first safe opportunity — before the next assistant turn, or after each tool execution), it is removed from the pinned area and inserted into the conversation as a regular user message in sequence.

## Tool projection reference output (compact mode)

These examples are normative for **compact interactive rendering** only. They are
intended to stay close to the actual terminal layout and to define output shape,
streaming stability, and tool-specific usefulness.

They do **not** define LLM-visible content. The LLM projection may include more
complete tool result text than the compact UI shows.

### Rendering invariants

- Tool presentation is **tool-specific**. The UI should show only information
  that helps the user understand what the model is doing and whether progress is
  meaningful.
- Tool call and tool result rendering should flow through a **single render
  pipeline**. Avoid ad hoc per-location formatting rules.
- Streaming updates should be **monotonic** where possible: append detail or
  reveal a result, rather than substantially reformatting earlier lines.
- Compact mode should avoid generic result headers such as `tool result` or
  `success`. If a tool needs status text, that status should be part of that
  tool's own presentation.
- When a tool result is long, the compact UI may show a preview/excerpt rather
  than the full result.
- Future verbosity modes such as compact / medium / full may be added later;
  these examples define compact mode only.

### Baseline layout reference

Representative terminal layout:

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 💬 I'll inspect the projection code and the UI renderer.                    │
│ 👀 src/projection.rs                                                        │
│ 👀 src/ui/log.rs                                                            │
│ 🔍 **/*projection*                                                          │
│ src/projection.rs                                                           │
│ docs/plans/2026-04-25-projection-cleanup.md                                 │
├──────────────────────────────────────────────────────────────────────────────┤
│ ⠋ thinking…                                                                 │
│ provider: OpenAI / gpt-5                                                    │
├────────────────────────────────────────────────────────────────────────���─────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

The exact box drawing and widths may vary by terminal size; the important part
is the relative placement:
- session log in the output viewport
- control-band rows beneath it
- input area at the bottom

### `read_file`

#### Streaming / before result

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 💭 Let me inspect the projection logic.                                     │
│ 👀 src/projection.rs                                                        │
├──────────────────────────────────────────────────────────────────────────────┤
│ ⠋ thinking…                                                                 │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Completed, windowed read

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 💭 Let me inspect the projection logic.                                     │
│ 👀 src/projection.rs [1-120/874]                                            │
│ //! Projections over the session event log.                                 │
│ //!                                                                         │
│ //! A projection is a pure function (or stateful renderer) that derives     │
│ …                                                                            │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Completed, tiny read

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 👀 Cargo.toml                                                               │
│ [package]                                                                   │
│ name = "tau"                                                               │
│ version = "…"                                                              │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Error

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 👀 src/missing.rs                                                           │
│ Failed to read src/missing.rs: No such file or directory                    │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- The stable anchor is the file path line.
- If line-range metadata is available, show it on that same line.
- The result body should be a compact excerpt, not a reformatted summary.

### `find_files`

#### Streaming / before result

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 🔍 **/*projection*                                                          │
├──────────────────────────────────────────────────────────────────────────────┤
│ ⠋ thinking…                                                                 │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Completed with matches

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 🔍 **/*projection*                                                          │
│ src/projection.rs                                                           │
│ docs/plans/2026-04-18-session-state-and-projection-simplification-plan.md   │
│ docs/plans/2026-04-25-projection-cleanup.md                                 │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└─────────────────────────────────���────────────────────────────────────────────┘
```

#### Completed with no matches

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 🔍 **/*tool*result*                                                         │
│ No files found matching pattern                                             │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- The pattern is the primary progress signal.
- The result should remain list-like; no extra framing is needed.

### `write_file`

#### Completed

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ✏️ docs/notes.md                                                             │
│ Written 18 lines to docs/notes.md                                           │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- For file-creation/write tools, a terse transactional result is enough.
- Do not echo the newly written file content in compact mode.

### `edit_file`

#### Completed

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 📝 src/projection.rs                                                        │
│ Successfully edited src/projection.rs                                       │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Error

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 📝 src/projection.rs                                                        │
│ old_text must match exactly once                                            │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- The path is the important anchor.
- The compact result should expose the transactional outcome, not a patch.

### `bash` / `cmd` / `powershell`

#### Streaming multi-line command

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 💻 cargo test --all-features                                                │
│    cargo clippy --all-targets --all-features -- -D warnings                 │
├──────────────────────────────────────────────────────────────────────────────┤
│ ⠋ thinking…                                                                 │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Completed with useful tail output

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 💻 cargo test --all-features                                                │
│    cargo clippy --all-targets --all-features -- -D warnings                 │
│ test result: ok. 148 passed; 0 failed; 0 ignored                            │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Completed with long output

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 💻 just preflight                                                           │
│ …                                                                            │
│ cargo check --all-targets --all-features                                    │
│ Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.31s         │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Error

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 💻 cargo test                                                               │
│ error: test failed, to rerun pass `--lib`                                   │
│ exit 101                                                                     │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- The command itself is the stable anchor.
- Compact mode should prefer the most informative tail of the output.
- If output is long, showing the trailing window is preferable to full output.

### `exec`

#### Streaming

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ⚙️ cargo test --lib                                                         │
├──────────────────────────────────────────────────────────────────────────────┤
│ ⠋ thinking…                                                                 │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Completed

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ⚙️ cargo test --lib                                                         │
│ test result: ok. 24 passed                                                  │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- `exec` should feel like a process invocation, similar to shell tools, but
  preserve argv-style readability.

### `ask_user`

#### Prompt shown to the user

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ❓ I found two plausible approaches.                                         │
│                                                                              │
│ Which one should I use?                                                      │
├──────────────────────────────────────────────────────────────────────────────┤
│ > 1                                                                          │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Answer captured

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ ❓ I found two plausible approaches.                                         │
│                                                                              │
│ Which one should I use?                                                      │
│ 1                                                                            │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- `ask_user` is conversational, not transactional.
- The question should be shown in full; compact mode should not ellipsize it
  aggressively.

### Multi-tool turn shape

A turn with multiple tools should keep each tool anchored to its own invocation
line and append only the tool-specific compact result beneath it.

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ 💭 I'll inspect the projection path and the UI renderer.                    │
│ 👀 src/projection.rs [1-120/874]                                            │
│ //! Projections over the session event log.                                 │
│ 👀 src/ui/log.rs [1-120/588]                                                │
│ use ratatui::{                                                              │
│ 🔍 **/*projection*                                                          │
│ src/projection.rs                                                           │
│ docs/plans/2026-04-25-projection-cleanup.md                                 │
├──────────────────────────────────────────────────────────────────────────────┤
│ >                                                                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

The important property is that each tool's display remains compact, local, and
stable, rather than being wrapped in generic headers or reformatted into a
shared verbose result structure.

## Login panel

When `/login <provider>` is active (currently `copilot`, `codex`, or `gemini`) the input area is replaced by a login panel
injected at the bottom of the screen (the same vertical layout slot as the
selection menu).

Layout:

```
┌─ header row ──────────────────────────────────── Esc cancel ─┐
│  <status/progress line>                                       │
│                                                               │
│  URL: [open in browser →]   ← OSC 8 hyperlink                │
│  Code: XXXX-YYYY            ← device flow only               │
└───────────────────────────────────────────────────────────────┘
```

- The browser is opened automatically via `xdg-open` / `open` / `start`.
- The `open in browser →` label is an OSC 8 terminal hyperlink; clicking it
  opens the URL in terminals that support hyperlinks (Kitty, WezTerm, iTerm2,
  GNOME Terminal ≥ 3.26, foot, …). In other terminals it renders as underlined
  text.
- `Esc` cancels the flow; success or failure is appended to the chat log.
- The panel height adjusts automatically to the number of content rows.

