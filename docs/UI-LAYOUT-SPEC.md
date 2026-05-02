# User Interface Layout Spec

## Screen layout

- The text mode user interface is a vertical stack of components.
- The strict order of component is:
    1. Output area
    2. Indicator area (throbber, status message)
    3. Menu selection area
        - Question
        - Options
    4. Input area

- The indicator area is hidden when there is no status message or throbber to show.
- The menu selection area is hidden when there is no question to show.
- The input area grows with content to a maximum of 10 lines, and then becomes scrollable.
- The output area shrinks to accommodate the other components.
- The output area is scrollable when its content exceeds the available space.
- The output area automatically scrolls to the bottom when new content is added, unless the user has manually scrolled up to view previous content. In that case, it should not auto-scroll until the user scrolls back down to the bottom.

## Output area

- The output area displays the conversation history.
- User prompts are rendered with background color #1a1a1a.
- Steering messages are rendered with background color #571212.
- `ask_user` responses are rendered with background color #1b471f and italicized text.
- User prompts are rendered with the horizontal half-block separator before and after, matching the background color of the prompt.
- Agent responses and tool invocations are rendered without separators.
- Agent responses and tool invocations are prefixed with indicator icons:
    - Agent response: 💬
    - Agent thought: 💭
    - Agent thinking: 🧠
    - Bash/PowerShell/cmd tool invocation: 💻
    - read_file tool invocation: 👀
    - write_file tool invocation: ✏️
    - edit_file tool invocation: 📝
    - find_files tool invocation: 🔍
    - ask_user tool: ❓
- Agent response, thought, and thinking are rendered with markdown formatting.
- User prompts, steering messages, and `ask_user` responses are rendered with markdown formatting.
- Icons prefixing markdown content are rendered depending on the paragraph type:
    - Paragraphs: inline as a part of text
    - Blocks (code, tables, lists): prefix causing indentation of the entire block

- Tool invocations consist of:
    - tool intent (first line with indicator icon)
    - tool body (following lines)

### Agent response and thought formatting

- Agent responses and thoughts are rendered with markdown formatting.
- Code blocks in agent responses and thoughts are rendered with a dark background (#1a1a1a) and light text (#dcdcdc), and use a monospace font.
- Inline

### Tool output

Tools are rendered with the tool intent and tool body. The tool intent is the first line of the tool invocation, prefixed with the corresponding indicator icon. The tool body is rendered below the tool intent, with content depending on the type of tool and truncated as specified below.

#### `read_file`

- Icon: 👀
- Tool intent: <file_path> [start-end]
- Tool body: content of file being read, head-truncated, 8 lines

#### `write_file`

- Icon: ✏️
- Tool intent: <file_path>
- Tool body: content being written to a file, head-truncated, 8 lines

#### `edit_file`

- Icon: 📝
- Tool intent: <file_path>
- Tool body: unified diff of the file edit, head-truncated, 8 lines

#### `find_files`

- Icon: 🔍
- Tool intent: <search_query> in <directory_path>
- Tool body: list of found files, head-truncated, 8 files

#### `exec` and shell tools

- Icon: 💻
- Tool intent: <command>
- Tool body: output of the command execution, tail-truncated, 8 lines
    - stderr/stdout ordered as they would appear in a terminal, with lines interleaved accordingly and truncation applied to the combined output

#### `ask_user`

`ask_user` exchanges are rendered as a unified block in the output area once the
exchange is complete (question answered). While the question is pending, the
output area shows nothing for this call; the menu selection area is the active
interactive surface.

Once answered, the following is committed to the output area in order:

1. **Context** (if present): background `#1b471f`, dimmed text, no icon
2. **Question**: background `#1b471f`, normal text, `❓` icon
3. **Response**: background `#1b471f`, italicized text, no icon

- The response is never truncated.
- Context and question are rendered with markdown formatting.

# Menu selection area

- The menu selection area displays a question and a list of options for the user to select from.
- The menu selection is used for `ask_user` tool invocations.
- The menu selection area is hidden when there is no question to display.
