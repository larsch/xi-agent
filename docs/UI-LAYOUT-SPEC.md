# User Interface Layout Spec

## Screen layout

The UI is a vertical stack of components in strict top-to-bottom order:

1. Output area
2. Indicator area (throbber, status message)
3. Menu selection area (question + options)
4. Input area

- The indicator area is hidden when there is no active status message or throbber.
- The menu selection area is hidden when there is no active question.
- The input area grows with content up to a maximum of 10 lines, then becomes scrollable.
- The output area shrinks to accommodate the other components.
- The output area is scrollable when its content exceeds the available space.
- The output area automatically scrolls to the bottom when new content is added, unless the user has manually scrolled up. In that case it does not auto-scroll until the user scrolls back to the bottom.

## Output area

The output area displays the conversation history.

### Background colors

| Content type         | Background  | Notes                     |
|----------------------|-------------|---------------------------|
| User prompt          | `#323240`   | With half-block separators before and after |
| Steering message     | `#571212`   |                           |
| `ask_user` context   | `#1b471f`   | Dimmed text               |
| `ask_user` question  | `#1b471f`   | Normal text, `❓` icon    |
| `ask_user` response  | `#1b471f`   | Italicized text           |

Agent responses and tool invocations are rendered without background color or separators.

### Icons

Each agent output line and tool invocation is prefixed with an indicator icon:

| Content                        | Icon |
|--------------------------------|------|
| Agent final response           | 💬   |
| Agent provisional response     | 💭   |
| Agent thinking                 | 🧠   |
| `bash` / `cmd` / `powershell`  | 💻   |
| `exec`                         | ⚙️   |
| `read_file`                    | 👀   |
| `write_file`                   | ✏️   |
| `edit_file`                    | 📝   |
| `find_files`                   | 🔍   |
| `ask_user`                     | ❓   |

### Markdown formatting

- Agent responses, thoughts, and thinking are rendered with markdown formatting.
- User prompts, steering messages, and `ask_user` context/question are rendered with markdown formatting.
- Code blocks are rendered with background `#1a1a1a` and foreground `#dcdcdc` in a monospace font.
- When a block element (code block, table, list) is prefixed with an icon, the icon causes indentation of the entire block. For inline paragraph content, the icon is rendered inline as part of the first line.

### Tool output

Each tool invocation renders as:

- **Tool intent**: first line, prefixed with the tool icon, containing the key invocation arguments
- **Tool body**: following lines, truncated as specified per tool

The source of the tool body depends on the tool:
- For retrieval and execution tools (`read_file`, `find_files`, `bash`, `exec`, etc.) the body comes from the **tool result** and is empty until execution completes.
- For content-generation tools (`write_file`, `edit_file`) the body comes from the **LLM-generated argument** and streams in while the LLM is generating it.

### Truncation

When a tool body is truncated, the truncated lines are replaced with a `... (N lines total)` line. The position of this line depends on the truncation direction:

- **Head-truncated**: `... (N lines total)` appears as the last line, after the shown content.
- **Tail-truncated**: `... (N lines total)` appears as the first line, before the shown content.

This convention applies consistently to all tool bodies.

#### `read_file`

- Intent: `👀 <path>` while tool is pending; `👀 <path> [<first>-<last>/<total>]` once result is available. When the file is not windowed (full file returned), no range suffix is shown.
- Body: content of the file, head-truncated to 8 lines.

#### `write_file`

- Intent: `✏️ <path>` — at minimum shows `✏️ write_file` until the `path` argument is available.
- Body: the content being written to the file (`tool_args["content"]`), head-truncated to 8 lines. Streams in as the LLM generates the `content` argument, sourced from `tool_partial_args` during streaming and from `tool_args` once finalized.

#### `edit_file`

- Intent: `📝 <path>`
- Body: a compact diff showing up to 4 lines of search text (red, `-` prefix) and up to 4 lines of replacement text (green, `+` prefix). Each side is independently truncated with `... (N lines total)` when it exceeds 4 lines. Streams in as the LLM generates the `new_text` argument.

#### `find_files`

- Intent: rendered from available arguments — `🔍 <pattern>`, `🔍 <pattern> in <path>`, or `🔍 in <path>` — using whichever fields are present. At minimum shows `🔍 find_files` if no arguments are yet available.
- Body: list of matched file paths, head-truncated to 8 entries.

#### `bash` / `cmd` / `powershell`

- Intent: `💻 <command>` — multiline commands preserved up to 5 lines, then `…` on its own line. Streams in as the LLM generates the `command` argument.
- Body: combined stdout/stderr output interleaved as produced, tail-truncated to 8 lines.

#### `exec`

- Intent: `⚙️ <program> [args…]` — rendered as a shell-quoted argv string, using whichever fields (`program`, `args`) are available. At minimum shows `⚙️ exec`.
- Body: combined stdout/stderr output interleaved as produced, tail-truncated to 8 lines.

#### Custom and unknown tools

- Intent: `⚙️ <best available argument summary>` — using the first available string argument, or the tool name only if no arguments are available.
- Body: tool result content, tail-truncated to 8 lines.

#### `ask_user`

`ask_user` exchanges are rendered as a unified block in the output area once the
exchange is complete. While the question is pending, the output area shows
nothing for this call; the menu selection area is the active interactive surface.

Once answered, the following is committed to the output area in order:

1. **Context** (if present): background `#1b471f`, dimmed text, no icon
2. **Question**: background `#1b471f`, normal text, `❓` icon
3. **Response**: background `#1b471f`, italicized text, no icon

Context, question, and response all share the same background, forming a single
visual block. The response is never truncated.

## Live streaming

During an active agent turn the output area renders live state. The goal is
that the streaming view looks as close as possible to the final committed view,
and that all transitions are monotonic — content may appear or be appended, but
existing rendered lines must not be rewritten or replaced.

### Assistant text

- Thinking tokens (`🧠`) appear as they arrive, rendered in dimmed style.
- Assistant text tokens appear as they arrive, prefixed with `💭` while the
  turn is provisional (tool-calling phase) and `💬` once final.
- The icon does not change retroactively once displayed; the final icon is
  chosen at commit time and only affects the committed view.
- Leading and trailing whitespace is not rendered during streaming or after
  commit.

### Tool intent line

- The tool intent line appears as soon as the tool name is known, even before
  arguments have finished streaming.
- The intent line shows the best available summary from the partially streamed
  argument JSON. For tools with a designated streaming field (e.g. `path`,
  `command`, `pattern`, `content`), that field is extracted and shown as soon
  as it is parseable. If no meaningful value is available yet, the intent shows
  the icon and tool name only.
- Once the full arguments are available the intent line updates to its final
  form. This update must not change the number of lines occupied by the intent.

### Tool body

- For tools whose body comes from the LLM-generated argument (`write_file`,
  `edit_file`), the body streams in as the argument is generated. The visible
  window advances as new lines arrive beyond the truncation limit.
- For tools whose body comes from the tool result (`read_file`, `find_files`,
  `bash`, `exec`, etc.), the body is empty until execution completes, then
  appears in full.
- At commit the body snaps to its final truncated form. For streaming-argument
  tools this means the window shifts from tail (newest content) to head (start
  of content) — this reflow at commit is expected and permitted.
- The truncation rules (head/tail, line counts, `... (N lines total)`) are the
  same during streaming and after commit. The `read_file` intent line gains the
  `[first-last/total]` range suffix when the result arrives; before that it
  shows only the path.

### Stability rules

The following changes are permitted during a streaming turn:

- The tool intent line may update once as arguments complete streaming.
- The tool body may grow by appending new lines while streaming.
- The tool body window may advance (tail-to-head reflow) at commit for streaming-argument tools.
- The `read_file` intent line gains the `[first-last/total]` suffix when the result arrives.
- The assistant icon may change from `💭` to `💬` at turn commit.

All other rendered content must remain stable. Lines must not be removed,
replaced, or shifted.

## Menu selection area

The menu selection area is used for `ask_user` tool invocations. It is hidden
when there is no active question.

- The question is always shown.
- If the `ask_user` call includes options, they are shown as a selectable list
  below the question and the user navigates and confirms with the keyboard.
- If no options are provided, the menu selection area shows only the question
  and the input area accepts a free-form text answer.
