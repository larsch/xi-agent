# tau

An AI agent harness for the terminal heavily inspired by pi (https://pi.dev/).
Chat with local and remote LLMs using a streaming TUI, with full tool-calling
support so the model can read files, run shell commands, edit code, and feed
results back into the conversation.

Built with Rust + [ratatui](https://github.com/ratatui/ratatui) +
[tui-textarea](https://github.com/rhysd/tui-textarea).

## Status

Active development, self-hosted with a few rough edges. The core agentic loop is
working end-to-end: multi-provider streaming, tool-calling, inline tool display,
thinking output, and slash commands are all implemented. See
[ROADMAP](docs/ROADMAP.md) for what's coming next.

- [x] Initial text-based UI with input box and streaming output
- [x] Core agent loop (streaming, tools, thinking output)
- [x] Multiple providers (Copilot, OpenAI, Codex, Gemini, Ollama)
- [x] Interactive authentication
- [x] Basic tools (file read/write/edit, find files, ask user, shell commands)
- [x] Bash on Unix, PowerShell and cmd.exe on Windows
- [x] SKILL.md support
- [x] AGENTS.md support
- [x] Session persistence (resume conversations)
- [x] Steering (type messages while agent loop is running)

High priority:

Medium priority:

- [ ] Platform credential storage
- [ ] Context compaction

Low priority:

- [ ] Markdown rendering (currently just raw text)
- [ ] Anthropic provider support

Out of scope (for now):

- [-] Safety guardrails (tool use is unrestricted)
- [-] Additional built-in tools beyond the current set

## License

AGPL-3.0-only. See [LICENSE](LICENSE).

## Build & Run

```sh
cargo build --release
./target/release/tau
```

## Providers

Supported providers:

- `copilot`
- `openai`
- `codex`
- `gemini`
- `ollama`

Authentication notes:
- `copilot`, `codex`, and `gemini` use interactive `/login <provider>`.
- `openai` uses `[openai].api_key` in `config.toml`.

## Configuration

tau supports an optional config file at:

- `$XDG_CONFIG_HOME/tau/config.toml` (preferred)
- `~/.config/tau/config.toml` (fallback)

Precedence (highest → lowest):

1. CLI flags (`--provider`, `--model`)
2. `config.toml`
3. Built-in defaults

When you change provider/model in the TUI (`/provider`, `/model`), tau writes
that selection back to `config.toml` so it persists across restarts.

Example:

```toml
provider = "openai"

[openai]
api_key = "sk-..."
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"

[copilot]
model = "gpt-4o"

[codex]
base_url = "https://chatgpt.com/backend-api/codex"
model = "gpt-5.4"

[gemini]
base_url = "https://cloudcode-pa.googleapis.com"
model = "gemini-2.5-pro"

[ollama]
base_url = "http://localhost:11434"
model = "llama3.1"
```

Environment variables:

| Variable      | Description            |
|---------------|------------------------|
| `TAU_DEBUG`   | Enable debug logging   |

## CLI flags

| Flag                    | Short | Description                                      |
|-------------------------|-------|--------------------------------------------------|
| `--provider <name>`     | `-P`  | Override provider (copilot / openai / codex / gemini / ollama) |
| `--model <name>`        | `-m`  | Override model name                              |
| `--print <prompt…>`     | `-p`  | Non-interactive: print response and exit         |

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
| `/provider <name>`   | Switch provider (copilot / openai / codex / gemini / ollama) |
| `/thinking <level>`  | Set reasoning effort (off / minimal / low / medium / high / xhigh) |
| `/login`             | Open interactive auth provider picker (copilot / codex / gemini) |
| `/login <provider>`  | Authenticate provider                            |
| `/resume`            | Open session picker (local + foreign sessions)   |
| `/quit`              | Quit                                             |

## Skills discovery

`/skill:<name>` uses skills discovered from these roots (in this order):

- `~/.tau/skills`
- `~/.agents/skills`
- `%USERPROFILE%\\.agents\\skills` (Windows)
- `./.agents/skills`
- `./.tau/skills`

Each skill is expected in a subdirectory containing `SKILL.md` with YAML frontmatter.

## Built-in tools

The agent has built-in tools out of the box:

| Tool          | Emoji | Description                                       |
|---------------|-------|---------------------------------------------------|
| `read_file`   | 👀    | Read a file (with optional offset/limit)          |
| `write_file`  | ✍️    | Write or overwrite a file                         |
| `edit_file`   | 📝    | Replace exact text in a file                      |
| `find_files`  | 🔍    | Search files by name glob or content pattern      |
| `ask_user`    | ❓    | Ask the user a question and wait for an answer    |
| `bash`        | 💻    | Run shell commands (non-Windows)                  |
| `powershell`  | 💻    | Run PowerShell commands (Windows)                 |
| `cmd`         | 💻    | Run `cmd.exe /C` commands (Windows)               |

## Non-interactive mode

Send a single prompt and stream the response to stdout:

```sh
tau --print explain the Cargo.toml
tau -p what does src/agent/mod.rs do
```

Tool calls are printed to stderr; final output goes to stdout, making it
pipeline-friendly.

## Thinking / reasoning notes

- `/thinking` applies only where provider/model mappings support it.
- For Gemini provider:
  - Gemini 2.x models use `thinkingBudget`.
  - Gemini 3.x models use `thinkingLevel`.
