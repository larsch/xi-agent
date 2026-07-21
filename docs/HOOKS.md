# Agent Hooks

Hooks allow you to define custom commands that run automatically at specific
points during an agent session.  They are configured in TOML files discovered
from multiple sources.  All hooks from all sources run — they are additive,
never replace each other.

## Configuration sources

Hooks are loaded from three layers in run order (deepest first):

| Layer | Sources (priority order per location) |
|-------|----------------------------------------|
| Project-local walk | cwd → root: `.xi/hooks.toml` → `.agents/hooks.toml` (first match per directory) |
| Home standalone   | `~/.xi/hooks.toml` → `$XDG_CONFIG_HOME/xi/hooks.toml` → `~/.agents/hooks.toml` |
| Global config     | `[hooks]` section in `~/.config/xi/config.toml` |

Each file uses the same `[[hooks.<point>]]` TOML format shown below.
Only one file is taken per directory level — if `.xi/hooks.toml` exists,
`.agents/hooks.toml` at the same level is skipped.

### Per-project hooks

Place a `.xi/hooks.toml` in your project root:

```toml
# .xi/hooks.toml — fires for any session with cwd in this project
[[hooks.pre_tool]]
bash = "echo 'project rule enforced'"
timeout = 5
```

This file will be picked up automatically when xi-agent's working directory
is inside (or below) that directory.

## Hook points

### Lifecycle hooks

These hooks fire at the main decision boundaries of the agent loop.  They
receive structured JSON on stdin describing the current action.

| Hook point              | When it fires                                                               |
|-------------------------|-----------------------------------------------------------------------------|
| `pre_tool`              | Just before a tool is executed                                              |
| `post_tool`             | Just after a tool completes                                                 |
| `pre_turn`              | At the start of each LLM cycle (before the model is called)                 |
| `post_turn`             | After each LLM cycle completes (may fire multiple times per user prompt)    |
| `on_done`               | **Once** when the agent finishes its full response to a user prompt         |
| `on_error`              | When an unhandled error occurs during the agent loop                        |
| `on_cancel`             | When the agent loop is cancelled by the user (Esc key or programmatic abort) |

### Interaction hooks

These hooks fire at user-interaction boundaries.  They receive structured JSON
on stdin.

| Hook point               | When it fires                                                              |
|--------------------------|----------------------------------------------------------------------------|
| `on_ask_user`            | The agent asks the user a question via the `ask_user` tool                  |
| `on_steering_consumed`   | A queued steering message is consumed at a turn boundary                    |

### Notification hooks

These hooks fire at informational moments.  Most receive **no stdin JSON**
(only the environment variables) — use them for triggers that don't need
event data.

| Hook point                | When it fires                                                              |
|---------------------------|----------------------------------------------------------------------------|
| `on_tool_intent`          | Model signals intent to use a tool (name known, arguments not yet streamed) |
| `on_first_thinking_token` | First thinking/chain-of-thought token arrives                              |
| `on_first_text_token`     | First visible text token arrives                                           |
| `on_status_update`        | Provider sends a transient status (e.g. rate-limit, retry)                 |
| `on_compacting`           | Session compaction begins                                                  |
| `on_compaction_done`      | Session compaction completes                                               |
| `on_external_change`      | External file modification detected before a turn                          |
| `on_idle`                 | TUI returns to idle, waiting for user input (fires after `on_done`)        |

## Configuration

Hooks use the **array-of-tables** syntax (`[[hooks.<point>]]`).  Each entry
defines one hook; multiple entries at the same point all run, in order.

### Execution methods

Each hook can specify how to run using one or more of these keys.  The runtime
picks the **first available** method for the current platform:

| Key          | How it runs               | Best for           |
|--------------|---------------------------|--------------------|
| `bash`       | `sh -c <value>`           | Linux / macOS      |
| `powershell` | `pwsh -c <value>`         | Windows            |
| `cmd`        | `cmd /c <value>`          | Windows (legacy)   |
| `command`    | Direct executable + `args`| Cross-platform     |

Platform-specific resolution order:

- **Linux / macOS**: `bash` → `powershell` → `command`
- **Windows**: `powershell` → `cmd` → `bash` → `command`

The `cmd` key is **Windows-only** (ignored on Linux/macOS).  The `powershell`
key works everywhere — it runs after `bash` on Unix and before `cmd`/`bash`
on Windows.

Common fields:

| Field           | Type          | Default | Description                                           |
|-----------------|---------------|---------|-------------------------------------------------------|
| `bash`          | string        | —       | Shell command (`sh -c`)                               |
| `powershell`    | string        | —       | PowerShell command (`pwsh -c`)                        |
| `cmd`           | string        | —       | CMD command (`cmd /c`)                                |
| `command`       | string        | —       | Executable path                                       |
| `args`          | string list   | `[]`    | Arguments for `command` (ignored for shell keys)      |
| `timeout`       | integer       | `30`    | Max seconds the hook may run                          |
| `cwd`           | string        | —       | Working directory for the hook process                |
| `include_tools` | string list   | `[]`    | Only fire for these tools (`pre_tool`/`post_tool`/`on_tool_intent`) |
| `exclude_tools` | string list   | `[]`    | Skip these tools (`pre_tool`/`post_tool`/`on_tool_intent`)         |

### Examples

**Play a sound when the agent finishes answering:**

```toml
[[hooks.on_done]]
bash = "mpg123 --quiet ~/sounds/done.mp3"
timeout = 15
```

**Log tool invocations to a file:**

```toml
[[hooks.pre_tool]]
bash = "echo \"[$XI_HOOK_POINT] $(date): $XI_SESSION_ID\" >> /tmp/xi-hooks.log"
```

**Run a script only when specific tools execute:**

```toml
[[hooks.post_tool]]
command = "/home/user/bin/log-tool-execution"
args = ["--to", "syslog"]
include_tools = ["bash", "exec", "run_python"]
```

**Alert on errors, excluding read_file errors:**

```toml
[[hooks.on_error]]
bash = "notify-send 'xi-agent error' 'Check the log'"
exclude_tools = ["read_file"]
```

**Cross-platform: bash on Linux, PowerShell on Windows:**

```toml
[[hooks.pre_tool]]
bash = "echo 'running tool'"
powershell = "Write-Host 'running tool'"
```

## Data passed to hooks

### Environment variables (all hooks)

| Variable          | Description                        |
|-------------------|------------------------------------|
| `XI_HOOK_POINT`   | Hook point name (e.g. `pre_tool`)  |
| `XI_SESSION_ID`   | Persistent session identifier      |

### Stdin JSON (lifecycle and interaction hooks)

These payloads let you inspect what the agent is doing.

`pre_tool`, `post_tool`, and `on_tool_intent`:

```json
{
  "tool": "bash",
  "arguments": { "command": "ls -la" }
}
```

For `on_tool_intent`, `arguments` is always `null` — the tool name is known
but arguments have not finished streaming yet.

`post_tool` adds result fields:

```json
{
  "tool": "bash",
  "arguments": { "command": "ls -la" },
  "exit_code": 0,
  "output_truncated": false
}
```

`exit_code` is `0` on success, `1` on error — it is a **binary flag**, not
the tool process's real exit code.

`on_error`:

```json
{
  "error": "Tool execution failed: ...",
  "tool": "bash",
  "arguments": { "command": "ls /nonexistent" }
}
```

`tool` and `arguments` are **optional** — they are omitted when the error is
not tied to a specific tool (e.g. a provider failure).

`on_cancel` — receives no stdin JSON (only environment variables).

`on_ask_user`:

```json
{
  "question": "Which approach would you prefer?"
}
```

`on_steering_consumed`:

```json
{
  "text": "stop using bash"
}
```

`on_compaction_done`:

```json
{
  "tokens_before": 12000,
  "tokens_after": 4500,
  "retained_event_count": 12
}
```

### Stdin JSON (notification hooks with data)

Only two notification hooks receive stdin JSON:

`on_tool_intent` — same shape as `pre_tool` but `arguments` is `null`:

```json
{
  "tool": "bash",
  "arguments": null
}
```

`on_status_update`:

```json
{
  "status": "Rate limited, retrying in 7s…"
}
```

All other notification hooks (`on_first_thinking_token`, `on_first_text_token`,
`on_idle`, `on_compacting`, `on_external_change`) receive no stdin JSON —
only the environment variables above.

## Behaviour

- Hooks run **synchronously** — the agent loop waits for the hook to finish
  (or time out) before proceeding.
- If the binary is missing or cannot be executed, a warning is logged and the
  loop continues.
- If the hook times out, it is killed (SIGTERM → SIGKILL after 2 s grace),
  a warning is logged, and the loop continues.
- Hook stdout/stderr are discarded.
- Hook exit codes are ignored.
- If no execution method is configured for the current platform, the hook is
  silently skipped.
