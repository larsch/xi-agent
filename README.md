# xi-agent

[![Rust](https://github.com/larsch/xi-agent/actions/workflows/rust.yml/badge.svg)](https://github.com/larsch/xi-agent/actions/workflows/rust.yml)

**xi** is a fast, transparent terminal agent. Every tool call streams live as it
happens — you watch the agent read, edit, run commands, and reason in real time.
No hidden steps, no pre-canned workflows.

Inspired by [pi](https://pi.dev/) but more streamlined: colours, emoji, and
block characters give structure without noise. The UX stays compact even during
busy tool loops.

* Raw agent workflow — see everything, no black boxes
* Compact, styled output — clear block delineation with colour and emoji
* **You** define the instructions and skills that fit your workflows; xi provides a smooth, streamlined harness for the model to interact with your environment
* Cross-platform from the start — bash, powershell, python, and more (`read_file`, `write_file`, `edit_file`, `find_files`, `ask_user`, `exec` (Unix), `bash` (Unix) / `cmd`, `powershell` (Windows), `python`)
* Lightweight Rust binary — low memory, fast startup, single executable
* Standard `AGENTS.md` and `SKILL.md` support
* Custom tools and skills — extend without bloat
* Caveats: **No safety guards** on tool calls; you are in control

## Providers

| Provider | Type | Auth |
|---|---|---|
| **GitHub Copilot** | Cloud — managed model routing | `/login copilot` |
| **OpenAI API** | Cloud — OpenAI models | `OPENAI_API_KEY` |
| **OpenAI Codex** (chatgpt.com) | Cloud — OpenAI Codex | `/login codex` |
| **OpenRouter** | Cloud — multi-model gateway | API key in config |
| **Google Gemini** (Cloud Code Assist) | Cloud — Gemini models | `/login gemini` |
| **Ollama** | Self-hosted | none |
| **ollama.com** | Cloud — Ollama-hosted models | API key in config |
| **Open WebUI** | Self-hosted | API key in config |
| **OpenAI-compatible endpoint** | Any OpenAI-compatible API | API key in config |

Configure named provider instances in `~/.xi/config.toml` and select them with `-P <name>` or `/provider <name>`.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).

## Installation

Install from [crates.io](https://crates.io/crates/xi-agent):

```sh
cargo install xi-agent
```

Or install from source:

```sh
cargo install --path .
```

## Command line options

| Short | Long | Description |
|-------|------|-------------|
| `-P` | `--provider <PROVIDER>` | Configured provider instance id to use |
| `-m` | `--model <MODEL>` | Model name to use (e.g. gpt-4o, llama3.1) |
| `-p` | `--print <PROMPT>...` | Run in non-interactive mode: send PROMPT, stream the response to stdout, and exit. Accepts multiple words without shell quoting |
| | `--print-dirs` | Print the file-system paths xi uses and exit |
| `-h` | `--help` | Print help |
| `-V` | `--version` | Print version |

## Keybindings

| Key             | Action                          |
|-----------------|-------------------------------|
| `F1`            | Show keyboard shortcuts         |
| `Enter`         | Submit message (or queue steering message if agent loop is running) |
| `Shift+Enter`   | Insert newline in input         |
| `Page Up`       | Scroll chat up one page         |
| `Page Down`     | Scroll chat to bottom           |
| `Scroll wheel`  | Scroll chat (3 lines/tick)      |
| `Ctrl+I`        | Toggle provider/model info bar  |
| `Ctrl+F`        | Toggle full tool output         |
| `Ctrl+R`        | Resume latest session for current folder |
| `Ctrl+Z`        | Suspend xi only when the UI is idle and no agent/shell subprocess is running |
| `Ctrl+D`        | Quit when input is empty (or leave shell mode if shell input is empty) |
| `Ctrl+E`        | Edit the selected custom provider (provider picker) |
| `Ctrl+S`        | Cycle between available shells (shell mode) |
| `!`             | Enter shell mode when input is empty |
| `Alt+C`         | Copy the last assistant response |
| `Alt+Up` / `Alt+Down` | Step backward / forward through session history |
| `Ctrl+C`        | Quit (or leave shell mode)      |
| `Esc`           | Abort current agent loop; also cancel login/slash/selection contexts |

## Slash commands

| Command              | Description                                      |
|----------------------|--------------------------------------------------|
| `/new`               | Start a new conversation                         |
| `/model`             | Open interactive model picker                    |
| `/model <name>`      | Switch to a named model                          |
| `/provider`          | Open interactive provider picker                 |
| `/provider <name>`   | Switch to a configured provider instance         |
| `/thinking <level>`  | Set reasoning effort (off / minimal / low / medium / high / xhigh) |
| `/login`             | Open interactive auth provider picker (copilot / codex / gemini) |
| `/login <provider>`  | Authenticate provider                            |
| `/resume`            | Open session picker (local + foreign sessions)   |
| `/compact [instructions]` | Compact session context now, optionally with summary instructions |
| `/export [path]`     | Export this session to a self-contained HTML file |
| `/reload`            | Reload AGENTS.md context and available skills    |
| `/skill:<name>`      | Invoke a skill by name (e.g. `/skill:plan`)      |
| `/quit`              | Quit                                             |

## Skills

Add custom agent capabilities and expertise by placing [SKILL.md](https://agentskills.io/) files in these directories; reference them with `/skill:<name>`:

- `~/.xi/skills`
- `~/.agents/skills`
- `%USERPROFILE%\\.agents\\skills` (Windows)
- `./.agents/skills`
- `./.xi/skills`

## Custom tools

Add custom tools by placing executable files in these directories (in this order):

- `~/.xi/tools`
- `~/.agents/tools`
- `%USERPROFILE%\\.agents\\tools` (Windows)
- `./.agents/tools`
- `./.xi/tools`

Tools must respond to a `--describe` option and output a JSON description of the
tool's interface, including its name, description, and expected input
parameters. This allows the agent to understand how to use the tool effectively.
For example:

```json
{
  "name": "my_tool",
  "description": "A tool that does something useful",
  "parameters_schema": {
    "type": "object",
    "properties": {
      "input": {
        "type": "string",
        "description": "The input for the tool"
      }
    },
    "required": ["input"]
  }
}
```

The tool will receive input as UTF-8 JSON on stdin according to its declared
`parameters_schema`, and can respond with UTF-8 text or JSON output. For rich or
structured writes, tool authors should provide a UTF-8 file/stdin interface
rather than requiring shell-quoted payload arguments.

## Non-interactive mode

Send a single prompt and stream the response to stdout:

```sh
xi --print "explain the Cargo.toml"
xi -p "what does src/agent/mod.rs do"
```

Tool calls are printed to stderr; final output goes to stdout, making it
pipeline-friendly.
