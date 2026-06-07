# Agent Hooks

Hooks allow you to define custom commands that run automatically at specific
points during an agent session.  They are configured in
`~/.config/xi/config.toml`.

## Hook points

| Hook point   | When it fires                                                               |
|--------------|-----------------------------------------------------------------------------|
| `pre_tool`   | Just before a tool is executed                                              |
| `post_tool`  | Just after a tool completes                                                 |
| `pre_turn`   | At the start of each LLM cycle (before the model is called)                 |
| `post_turn`  | After each LLM cycle completes (may fire multiple times per user prompt)    |
| `on_done`    | **Once** when the agent finishes its full response to a user prompt         |
| `on_error`   | When an unhandled error occurs during the agent loop                        |

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

Common fields:

| Field           | Type          | Default | Description                                     |
|-----------------|---------------|---------|-------------------------------------------------|
| `bash`          | string        | —       | Shell command (`sh -c`)                         |
| `powershell`    | string        | —       | PowerShell command (`pwsh -c`)                  |
| `cmd`           | string        | —       | CMD command (`cmd /c`)                         |
| `command`       | string        | —       | Executable path                                 |
| `args`          | string list   | `[]`    | Arguments for `command` (ignored for shell keys)|
| `timeout`       | integer       | `30`    | Max seconds the hook may run                    |
| `cwd`           | string        | —       | Working directory for the hook process          |
| `include_tools` | string list   | `[]`    | Only fire for these tools (`pre_tool`/`post_tool`) |
| `exclude_tools` | string list   | `[]`    | Skip these tools (`pre_tool`/`post_tool`)       |

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
include_tools = ["bash", "exec", "python"]
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

### Stdin JSON (tool hooks: `pre_tool`, `post_tool`)

```json
{
  "tool": "bash",
  "arguments": { "command": "ls -la" }
}
```

### Stdin JSON (`post_tool`, adds exit info)

```json
{
  "tool": "bash",
  "arguments": { "command": "ls -la" },
  "exit_code": 0,
  "output_truncated": false
}
```

### Stdin JSON (`on_error`)

```json
{
  "error": "Tool execution failed: ...",
  "tool": "bash",
  "arguments": { "command": "ls /nonexistent" }
}
```

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
