# User Interface Specification

## Command line interface

- Start in interactive mode by default.
- Support non-interactive mode with `-p/--print` flag, which prints the result of each command to stdout instead of rendering a UI.

# Interactive UI

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
  - `🧠` for model thinking tokens
  - `💭` for provisional assistant output (tool intent/tool-calling turn)
  - `💬` for final assistant output
- User input is a text input field at the bottom of the screen, where users can type commands and submit them by pressing Enter.
- `Ctrl+I` toggles a one-line info bar (provider, model, thinking level, and context window). When provider-reported token usage is available and the model context window is known, the context section shows utilization as `used / max (percent)`; otherwise it falls back to showing only max context (or `unknown`).
- While the agent loop is running, Enter enqueues the typed text as a **steering message**. Queued steering messages are shown at the bottom of the chat log with a `🕹️` icon. When the agent loop consumes a steering message (at the first safe opportunity — before the next assistant turn, or after each tool execution), it is removed from the pinned area and inserted into the conversation as a regular user message in sequence.

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

