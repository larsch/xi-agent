# User Guide

This guide covers the command line and everyday interactive controls. For
provider setup, see the [provider and model guide](PROVIDER-MODEL-SPEC.md); for
configuration options, see [CONFIG.md](CONFIG.md).

## Start xi

Launch the interactive terminal UI:

```sh
xi
```

To submit a prompt immediately after the UI opens:

```sh
xi --prompt Summarize this project
xi --prompt-file task.txt
```

To run without the UI, select a configured provider instance and provide a
prompt:

```sh
xi --provider my-provider --print Summarize this project
```

The final response is written to stdout. Tool activity and diagnostics are
written to stderr. Run `xi --help` for the complete option list, or
`xi --print-dirs` to see the paths used by your installation.

### Command-line options

| Option | Description |
|---|---|
| `-P`, `--provider <ID>` | Select a configured provider instance. |
| `--agent <NAME>` | Select a discovered agent profile for this run. |
| `-m`, `--model <NAME>` | Override the model name. |
| `-p`, `--print <PROMPT...>` | Send a prompt in non-interactive mode, stream the response, and exit. |
| `--prompt <PROMPT...>` | Open the UI and automatically submit the prompt. |
| `--prompt-file <PATH>` | Open the UI and submit the UTF-8 file contents as the initial prompt. |
| `--resume` | Resume the latest session for the current directory. |
| `--enable-session-ipc` | Enable the worktree session IPC control socket. |
| `--print-dirs` | Print the paths xi uses and exit. |
| `--print-system-prompt` | Print the generated system prompt without starting a session. |
| `--theme <PATH>` | Use a theme file instead of the configured theme. |
| `--theme-demo` | Show a representative transcript using the local test provider. |
| `-h`, `--help` | Show command-line help. |
| `-V`, `--version` | Show the installed version. |

## Interactive controls

Press `F1` inside xi to open the complete shortcut list. Common controls:

| Key | Action |
|---|---|
| `Enter` | Submit a message; while the agent is working, queue it as guidance for the next turn. |
| `Shift+Enter` | Insert a newline in the chat input. |
| `Esc` | Cancel the current action or agent loop; when idle, clear input. |
| `Page Up` / `Page Down` | Scroll the conversation. |
| `Alt+Up` / `Alt+Down` | Browse pending guidance, drafts, and session history. |
| `Ctrl+R` | Resume the latest session for the current directory. |
| `F2` | View the current system prompt. |
| `F3` | Inspect instruction, rule, and skill files. |
| `Ctrl+I` or `Alt+S` | Toggle provider and model information. |
| `Alt+O` | Toggle full tool output. |
| `Alt+C` | Copy the last assistant response. |
| `!` | Enter shell mode when chat input is empty. |
| `Ctrl+C` | Abort the agent loop or leave shell mode. |
| `Ctrl+D` | Delete forward when input is non-empty; otherwise quit or leave shell mode. |

## Slash commands

Type `/` to see command completions. Common commands include:

| Command | Action |
|---|---|
| `/new` | Start a new conversation. |
| `/resume` | Open the session picker. |
| `/provider` | Choose a provider instance; `/provider <id>` switches directly. |
| `/model` | Choose a model; `/model <name>` switches directly. |
| `/login` | Choose a service to connect; `/login <service>` starts its setup. |
| `/agent` | Choose an agent profile; `/agent <name>` switches directly. |
| `/thinking <level>` | Set reasoning effort for supported models. |
| `/compact [instructions]` | Compact session context, optionally with summary guidance. |
| `/export [path]` | Export the current session as a self-contained HTML file. |
| `/context` | View the current system prompt. |
| `/reload` | Reload instructions, skills, and agent profiles. |
| `/retry` | Retry the last turn after an error. |
| `/skill:<name> [args]` | Invoke a skill, optionally with arguments. |
| `/quit` | Quit xi. |

## Instructions, skills, and agents

Place `AGENTS.md` instructions in a project to give xi context about its
conventions and workflow. Skills are reusable `SKILL.md` guides invoked with
`/skill:<name>`. xi discovers skills in `.xi/skills` and `.agents/skills`
folders in the current directory and its ancestors, as well as user-level
skill folders. The built-in `xi-config-skills` guide describes discovery,
precedence, file format, and the skills discovered for the current session.

Agent profiles can customize instructions, tools, and skills. Select one with
`--agent` or `/agent`. See the [architecture guide](ARCHITECTURE.md#user-definable-agents)
for the profile format and discovery behavior.

## Configuration and related guides

- [Configuration](CONFIG.md)
- [Providers and models](PROVIDER-MODEL-SPEC.md)
- [Themes](THEME.md)
- [Agent hooks](HOOKS.md)
- [Custom tools](CUSTOM-TOOLS.md)
