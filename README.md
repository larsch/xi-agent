# tau

**tau** is an experimental AI agent for the terminal, heavily inspired by
[pi](https://pi.dev/). It provides a minimalistic text-based UI for agentic
interactions with local and remote models, supporting tool calls, session
persistence, and interactive authentication.

**tau** is under active development, misses some features and polish, but is fully
functional for basic use and being used as the daily driver for its author. See
the [ROADMAP](docs/ROADMAP.md) for planned features and improvements.

* Supported providers: built-in hosted providers plus configured named provider instances
* Built-in tools: read_file, write_file, edit_file, find_files, ask_user, bash/cmd/powershell
* Standard `AGENTS.md` support
* Standard `SKILL.md` support (see below)
* Custom tools: define your own tools (see below)
* Caveats: **No safety guarantees** around tool calls; use with caution

## License

AGPL-3.0-only. See [LICENSE](LICENSE).

## Installation

Install from source:

```sh
cargo install --path .
```

## Command line options

| Short | Long | Description |
|-------|------|-------------|
| `-P` | `--provider <PROVIDER>` | Configured provider instance id to use |
| `-m` | `--model <MODEL>` | Model name to use (e.g. gpt-4o, llama3.1) |
| `-p` | `--print <PROMPT>...` | Run in non-interactive mode: send PROMPT, stream the response to stdout, and exit. Accepts multiple words without shell quoting |
| | `--print-dirs` | Print the file-system paths tau uses and exit |
| `-h` | `--help` | Print help |
| `-V` | `--version` | Print version |

## Keybindings

| Key             | Action                          |
|-----------------|-------------------------------|
| `Enter`         | Submit message (or queue steering message if agent loop is running) |
| `Shift+Enter`   | Insert newline in input         |
| `Page Up`       | Scroll chat up one page         |
| `Page Down`     | Scroll chat to bottom           |
| `Scroll wheel`  | Scroll chat (3 lines/tick)      |
| `Ctrl+I`        | Toggle provider/model info bar  |
| `Ctrl+R`        | Resume latest session for current folder |
| `Ctrl+C`        | Quit                            |
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
| `/quit`              | Quit                                             |

## Skills

Add custom agent capabilities and expertise by placing [SKILL.md](https://agentskills.io/) files in these directories; reference them with `/skill:<name>`:

- `~/.tau/skills`
- `~/.agents/skills`
- `%USERPROFILE%\\.agents\\skills` (Windows)
- `./.agents/skills`
- `./.tau/skills`

## Custom tools

Add custom tools by placing executable files in these directories (in this order):

- `~/.tau/tools`
- `~/.agents/tools`
- `%USERPROFILE%\\.agents\\tools` (Windows)
- `./.agents/tools`
- `./.tau/tools`

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

The tool will receive input in JSON format according to its declared
`parameters_schema`, and can respond with text or JSON output.

## Non-interactive mode

Send a single prompt and stream the response to stdout:

```sh
tau --print "explain the Cargo.toml"
tau -p "what does src/agent/mod.rs do"
```

Tool calls are printed to stderr; final output goes to stdout, making it
pipeline-friendly.
