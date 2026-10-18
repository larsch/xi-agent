---
name: xi-config-hooks
description: Explains xi hook configuration, points, payloads, precedence, and loading behavior.
metadata:
  hidden: "true"
---

# Hooks in xi

Hooks are configured commands that xi runs at named agent lifecycle, interaction, and notification points. Hook files and the global config are additive.

## Discovery and precedence

From the current working directory to filesystem root, xi reads `.xi/hooks.toml` at each level if present; otherwise it reads `.agents/hooks.toml` at that level. Home standalone sources are then merged in order: `~/.xi/hooks.toml`, `$XDG_CONFIG_HOME/xi/hooks.toml` (normally `~/.config/xi/hooks.toml`), and `~/.agents/hooks.toml`. Finally, hooks from the `[hooks]` section of `~/.config/xi/config.toml` are merged. Multiple entries at a hook point run sequentially in the resulting order.

## Format

```toml
[[hooks.pre_tool]]
bash = "echo \"$XI_HOOK_POINT\""
timeout = 5
include_tools = ["bash", "exec"]
```

Each `[[hooks.<point>]]` entry may specify `bash`, `powershell`, `cmd`, or `command` with optional `args`; at least one execution method is needed. Other fields include `cwd`, `timeout` (default 30 seconds), `include_tools`, and `exclude_tools`. Tool filters apply to `pre_tool`, `post_tool`, and `on_tool_intent`.

Points include `pre_tool`, `post_tool`, `pre_turn`, `post_turn`, `on_done`, `on_error`, `on_cancel`, `on_ask_user`, `on_steering_consumed`, `on_tool_intent`, `on_first_thinking_token`, `on_first_text_token`, `on_status_update`, `on_compacting`, `on_compaction_done`, `on_external_change`, and `on_idle`.

Hooks receive `XI_HOOK_POINT` and `XI_SESSION_ID`. Applicable points also receive one JSON object on stdin. `docs/HOOKS.md` describes payloads for each point.

## Loading and execution

Hook definitions are loaded when xi constructs its hook context at session startup. `/reload` refreshes skills, agents, and custom tools; it does not reload hook configuration. Hooks run synchronously. Missing executables, timeouts, and nonzero exit codes do not stop the agent loop; stdout and stderr are discarded. Hook commands run with the xi process's permissions.
