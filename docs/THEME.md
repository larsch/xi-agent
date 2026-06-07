# Xi Theme Guide

Xi's visual appearance is fully configurable via a **theme file**. This guide
describes the file format, all available options, and how to select a theme.

---

## Location and selection

The default theme file is:

```
~/.config/xi/theme.toml
```

You can also point Xi at a different file in two ways:

1. **CLI flag** — pass `--theme <path>` when starting Xi.
2. **config.toml** — set `theme = "<path>"` in `~/.config/xi/config.toml`.

All fields in `theme.toml` are optional. Missing fields fall back to Xi's
built-in defaults, so you only need to specify the values you want to change.

---

## Format overview

The theme file is [TOML](https://toml.io). Styles are organised into sections
matching the UI area they affect. Within each section, dotted keys express
nested structure without requiring a separate section header for every leaf:

```toml
[log]
user.bg = "#323240"
user.padding_style = "half-block"

[markdown]
table.header.fg = "#c8d2f0"
table.header.bg = "#1e2840"
table.header.bold = true
```

---

## Style attributes

Most theme values are **style specs** — a flat set of optional attributes that
map directly to terminal rendering:

| Attribute  | Type    | Description                        |
|------------|---------|------------------------------------|
| `fg`       | color   | Foreground (text) color            |
| `bg`       | color   | Background color                   |
| `bold`     | bool    | Bold text                          |
| `dim`      | bool    | Dimmed/faint text                  |
| `italic`   | bool    | Italic text                        |
| `underline`| bool    | Underlined text                    |
| `visible`  | bool    | Whether the element is shown at all (default: `true`). Set to `false` to hide it completely. Applies to any style spec — e.g. `edge_marker.visible = false` hides the left-edge marker, `status.cost.visible = false` hides the cost display. |

The `visible` attribute is particularly useful in `[tools]` for suppressing
placeholder counters or noisy tool output — see the `[tools]` section for
examples. But it is valid on any named style spec throughout the theme file.

Colors are specified as one of:

- **`"#rrggbb"`** hex string — exact RGB value, e.g. `"#a0c8ff"`
- **CSS/HTML color name** — e.g. `"cornflowerblue"`, `"darkslategray"`. Resolved
  to a fixed RGB value at parse time; the full list of ~140 names is defined by
  the [CSS Color Level 4 spec](https://www.w3.org/TR/css-color-4/#named-colors).
- **Terminal palette name** — `"black"`, `"red"`, `"green"`, `"yellow"`,
  `"blue"`, `"magenta"`, `"cyan"`, `"white"`, and their `"bright-*"` variants
  (e.g. `"bright-cyan"`). These resolve to whatever color the terminal's own
  palette assigns to that slot — useful when you want the theme to adapt to
  the user's terminal color scheme rather than pin exact RGB values.

> **Note:** CSS names and `#rrggbb` values are pinned to a specific color
> regardless of terminal. Terminal palette names are intentionally
> palette-agnostic and will look different across terminal themes.

### Prefix style

Elements that render a leading label or icon — tool calls, assistant messages,
input fields, and the log edge marker — all use the same `prefix` sub-spec.
The `prefix` key is always a sub-table of the element, never a top-level
attribute (e.g. `edge_marker.prefix.text`, not `edge_marker.text`):

| Attribute      | Type   | Description                          |
|----------------|--------|--------------------------------------|
| `prefix.text`  | string | The prefix string, e.g. `"💬 "` or `"$ "` |
| `prefix.fg`    | color  | Foreground color of the prefix       |
| `prefix.bg`    | color  | Background color of the prefix       |
| `prefix.bold`  | bool   | Bold prefix                          |
| `prefix.dim`   | bool   | Dimmed prefix                        |
| `prefix.italic`| bool   | Italic prefix                        |

### Padding

Padding is space *inside* a styled region, between its border and its content.
Left and right padding is always rendered as blank cells — `padding_style`
does not apply to horizontal sides. Top and bottom padding share a single
`padding_style` that controls how those rows are drawn.

| Attribute        | Type         | Description                                    |
|------------------|--------------|------------------------------------------------|
| `padding`        | integer      | Shorthand: sets all four sides                 |
| `padding_x`      | integer      | Sets left and right                            |
| `padding_y`      | integer      | Sets top and bottom                            |
| `padding_top`    | integer      | Top padding rows                               |
| `padding_bottom` | integer      | Bottom padding rows                            |
| `padding_left`   | integer      | Left padding columns                           |
| `padding_right`  | integer      | Right padding columns                          |
| `padding_style`  | padding style| Rendering style for top and bottom rows only. Left and right padding is always blank — `padding_style` has no effect on horizontal sides. |

Resolution order (most specific wins): individual side > axis > global.

**Padding styles** (apply to top and bottom rows only):

| Value          | Description                                                  |
|----------------|--------------------------------------------------------------|
| `"blank"`      | Padding rows inherit the *surrounding* background — the block's own `bg` does not extend into them (default) |
| `"solid"`      | Padding rows are filled with the block's own `bg`, extending it outward |
| `"half-block"` | A single-row transition using `▀`/`▄` half-block characters to blend the block's `bg` into the surrounding color — visually sits between `blank` and `solid` |

In other words: `blank` keeps the block's background contained to its content
rows; `solid` expands it into the padding; `half-block` does a one-row
anti-aliased blend at each edge.

---

### Margin

Margin is space *outside* a styled region, between it and adjacent elements.
Margins are always rendered as blank cells (they do not belong to any
element's background). All margin fields follow the same shorthand cascade as
padding.

| Attribute        | Type    | Description                   |
|------------------|---------|-------------------------------|
| `margin`         | integer | Shorthand: sets all four sides |
| `margin_x`       | integer | Sets left and right           |
| `margin_y`       | integer | Sets top and bottom           |
| `margin_top`     | integer | Top margin rows               |
| `margin_bottom`  | integer | Bottom margin rows            |
| `margin_left`    | integer | Left margin columns           |
| `margin_right`   | integer | Right margin columns          |

**Margin collapsing:** when two adjacent blocks have a bottom margin and a top
margin, the gap between them is `max(margin_bottom, margin_top)` — not the
sum. This matches the CSS box model and prevents margins from stacking up
between a sequence of blocks.

```toml
[log]
# 1 line below user blocks, 1 line above ask_user blocks —
# collapses to 1 line between them
user.margin_bottom = 1
ask_user.margin_top = 1
```

---

## Sections

### `[log]`

The main conversation log — user messages, assistant messages, tool calls, and
tool results. The `steering` element styles queued instructions that appear
above the input; each instruction is always rendered with a prefix, so
`steering.prefix.text` controls the leading label on every line.

```toml
[log]
# User message block
user.bg = "#323240"
user.padding_y = 1
user.padding_style = "half-block"

# Ask-user response block
ask_user.bg = "#1b471f"
ask_user.padding_y = 1
ask_user.padding_style = "half-block"

# Left-edge marker rendered beside each block — uses prefix style
edge_marker.prefix.text = "│"
edge_marker.prefix.fg = "#6e6e78"

# Assistant message prefixes.
# "provisional" is used while the model is still streaming or thinking —
# the response may change. "final" is applied once the turn completes and
# the message is settled. Switching the prefix at that point gives a clear
# visual signal that the response is done.
assistant.provisional.prefix.text = "💭 "
assistant.final.prefix.text = "💬 "

# Thinking/reasoning text — the model's internal reasoning is shown dimmed
# above the final answer when extended thinking is enabled.
assistant.thinking.dim = true

# Steering / queued instructions panel. Each queued instruction is always
# rendered with a prefix — the prefix.text is prepended to every line.
steering.fg = "#c8c878"
steering.italic = true
steering.prefix.text = "🕹️ "
steering.prefix.fg = "#c8c878"

# Diff rendering inside tool results
diff.added.fg = "#55ff55"
diff.removed.fg = "#ff5555"
diff.unchanged.fg = "#888888"
```

---

### `[input]`

The user input area at the bottom of the screen. Each mode has a **panel**
style (the surrounding area) and a **field** style (the editable widget,
including its optional prompt prefix).

```toml
[input]
# Normal (agent) input
normal.bg = "#1e1e28"
normal.padding_y = 1
normal.padding_style = "half-block"
normal.field.prefix.text = "> "
normal.field.prefix.fg = "#788c8c"
normal.placeholder.fg = "#788c8c"
normal.placeholder.italic = true

# Shell mode input
shell.bg = "#182220"
shell.field.prefix.text = "$ "
shell.field.prefix.fg = "#8cdc8c"
shell.field.prefix.bold = true

# Ask-user response input
ask_user.bg = "#321e0f"
ask_user.field.prefix.text = "? "
ask_user.field.prefix.fg = "#ffdc50"

# Style applied to @file / @url tokens as they are resolved inline in the
# input — when you type "@" followed by a file path or URL, the resolved
# token is highlighted in this color to distinguish it from plain text.
at_file.fg = "#00ffff"
```

---

### `[menu]`

Autocomplete popup and selection menus (e.g. model picker, option lists).

```toml
[menu]
# Command completion popup
completion.bg = "#161626"
completion.selected.bg = "#373764"
completion.cmd.fg = "#78c8ff"
completion.desc.fg = "#8c8ca0"
completion.match.fg = "#ffdc50"
completion.match.bold = true

# Multi-option selection menu (e.g. ask_user choices)
selection.bg = "#121e12"
selection.selected.bg = "#1e5a1e"
selection.item.fg = "#8cdc8c"
selection.header.bg = "#142d14"
```

---

### `[status]`

The status bar appears at the top of the screen and shows the active provider,
model, session cost, and idle/busy state.

```toml
[status]
provider.fg = "#a0c8ff"
model.fg = "#648c64"
cost.fg = "#dcb450"
idle.fg = "#a0a0b4"
```

---

### `[info]`

The info/help panel (key bindings, session metadata). The keys listed below
are all available keys for this section.

```toml
[info]
bg = "#14141e"
separator.fg = "#3c3c50"
key.fg = "#646482"
value.fg = "#b4c8ff"
hint.fg = "#3c3c50"
hint.italic = true
```

---

### `[login]`

The provider login / authentication screen. The keys listed below are all
available keys for this section.

```toml
[login]
header.bg = "#141e3c"
content.bg = "#0f1630"

# Instruction text
instruction.fg = "#b4b4c8"

# Status info line
status.fg = "#ffffff"

# "URL:" label
url_key.fg = "#78c8ff"

# URL value
url_val.fg = "#64dc64"

# "Code:" label
code_key.fg = "#78c8ff"

# Code value
code_val.fg = "#ffff00"
```

---

### `[markdown]`

Markdown rendering in assistant messages.

```toml
[markdown]
code.fg = "#d2a064"

table.header.fg = "#c8d2f0"
table.header.bg = "#1e2840"
table.header.bold = true
table.row_even.bg = "#161822"
table.row_odd.bg = "#1e2330"
table.data.fg = "#d2d7e1"
table.separator.fg = "#000000"
```

---

### `[tools]`

Per-tool presentation. Each tool entry has four sub-specs:

- **`prefix`** — the icon/label rendered before the tool name once the
  argument is known (e.g. `"📝 foo.rs"`).
- **`headline`** — the style of the full tool call line (prefix + command or
  filename). Defaults to `body` colors when unset, so you only need to
  specify it when you want the headline to look different from the output.
- **`body`** — the style of the tool result content area.
- **`placeholder`** — the style used *before* the argument has streamed far
  enough to extract a meaningful label. While Xi is waiting for the filename,
  command, or path to arrive in the JSON stream, it shows a short pending
  label like `"editing…"` or `"running…"`. The `placeholder.text` field
  overrides that default text; the other attributes style it. Once the real
  argument is known the placeholder is replaced by the `prefix` style.

  As the argument streams in, some tools display progressive counters
  alongside the placeholder text. Each counter is a named sub-key of
  `placeholder` and is a full style spec. Set `visible = false` on any
  counter to suppress it, or on `placeholder` itself to skip the placeholder
  entirely and wait silently until the argument is known:

  | Sub-key | Tools | Description |
  |---|---|---|
  | `placeholder.lines` | `bash`, `exec`, `cmd`, `powershell`, `read_file`, `find_files` | Running count of output/result lines |
  | `placeholder.common_lines` | `edit_file` | Lines unchanged between old and new text |
  | `placeholder.changed_lines` | `edit_file` | Lines added or removed |

The special key `default` is the fallback for any tool not explicitly listed,
including custom tools. The special key `executing` is a group fallback that
applies to shell-executing tools (`bash`, `exec`, `cmd`, `powershell`) — more
specific than `default` but less specific than a named tool entry.

Resolution order for any attribute (first match wins):
1. `[tools.<name>]` — tool-specific entry
2. `[tools.executing]` — for shell tools only
3. `[tools.default]` — all tools

This means you can style all shell tools at once via `[tools.executing]` and
only override individual tools where needed. The snippet below illustrates a
partial override — only `prefix.text` is specified for `exec`, everything else
falls through to `[tools.executing]`:

```toml
[tools.executing]
prefix.fg = "#00ffff"
body.fg = "#00ffff"
placeholder.text = "running…"
placeholder.fg = "#666688"
placeholder.italic = true
placeholder.lines.fg = "#555577"

# Override just the prefix symbol for exec; all other attributes
# inherit from [tools.executing]
[tools.exec]
prefix.text = "⚡ "
```

The full default configuration for all built-in tools is shown below:

```toml
[tools.default]
prefix.text = "⚙️ "
prefix.fg = "#aaaaaa"
body.fg = "#aaaaaa"
placeholder.fg = "#666688"
placeholder.italic = true

# Group default for all shell-executing tools (bash, exec, cmd, powershell)
[tools.executing]
prefix.text = "💻 "
prefix.fg = "#00ffff"
headline.fg = "#00ffff"
headline.bold = true
body.fg = "#64b4b4"
placeholder.text = "running…"
placeholder.fg = "#666688"
placeholder.italic = true
placeholder.lines.fg = "#555577"

[tools.read_file]
prefix.text = "👀 "
prefix.fg = "#add8e6"
headline.fg = "#add8e6"
headline.bold = true
body.fg = "#6488a0"
placeholder.text = "reading…"
placeholder.fg = "#666688"
placeholder.italic = true
placeholder.lines.fg = "#555577"

[tools.write_file]
prefix.text = "📄 "
prefix.fg = "#add8e6"
headline.fg = "#add8e6"
headline.bold = true
body.fg = "#6488a0"
placeholder.text = "writing…"
placeholder.fg = "#666688"
placeholder.italic = true

[tools.edit_file]
prefix.text = "📝 "
prefix.fg = "#add8e6"
headline.fg = "#add8e6"
headline.bold = true
body.fg = "#6488a0"
placeholder.text = "editing…"
placeholder.fg = "#666688"
placeholder.italic = true
placeholder.common_lines.fg = "#555577"
placeholder.changed_lines.fg = "#888844"

[tools.find_files]
prefix.text = "🔍 "
prefix.fg = "#add8e6"
headline.fg = "#add8e6"
headline.bold = true
body.fg = "#6488a0"
placeholder.text = "finding…"
placeholder.fg = "#666688"
placeholder.italic = true
placeholder.lines.fg = "#555577"

[tools.ask_user]
prefix.text = "❓ "
prefix.fg = "#ffdc50"
body.fg = "#ffdc50"
placeholder.text = "asking…"
placeholder.fg = "#666688"
placeholder.italic = true
```

> **Note:** The Windows shell tools `cmd` and `powershell` follow the same
> pattern as `bash` — they support `prefix`, `body`, `placeholder`, and
> `placeholder.lines`. They are not shown above since they fall back to
> `[tools.default]` on most systems, but can be overridden the same way as
> any other tool.

Custom tools defined in your skills or tool configs can be styled the same way
using their tool name as the key:

```toml
[tools.my_custom_tool]
prefix.text = "🛠️ "
prefix.fg = "#ff88ff"
body.fg = "#ff88ff"
```

---

## Minimal example

A small override file — only the values you care about, everything else uses
the built-in defaults:

```toml
[log]
user.bg = "#1a1a2e"
user.padding_style = "half-block"

[input]
normal.field.prefix.text = "λ "
normal.field.prefix.fg = "#a0c8ff"
shell.field.prefix.text = "$ "

[tools.bash]
prefix.text = "$ "
prefix.fg = "#00ffff"
body.fg = "#00ffff"
placeholder.text = "running…"
placeholder.fg = "#444466"
placeholder.italic = true
```
