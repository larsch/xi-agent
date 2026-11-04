# Xi Configuration Guide

Xi is configured via `~/.config/xi/config.toml`. All fields are optional;
missing values fall back to built-in defaults.

---

## Runtime features

Use one table for optional behavior. Every feature has its own built-in default;
only explicitly configured values are saved.

```toml
[features]
session-ipc = false
auto-compaction = true
desktop-notifications = true
hook-ipc = false
```

| Feature | Default | Scope |
|---|---|---|
| `session-ipc` | Disabled | Process: worktree control endpoint and `agent_session` tool |
| `hook-ipc` | Disabled | Process: best-effort hook-event publisher |
| `desktop-notifications` | Enabled | Process: notifications while the terminal is unfocused |
| `auto-compaction` | Enabled | Session: threshold and context-overflow compaction |

Override configuration for one invocation with repeatable switches:

```console
xi --enable=session-ipc --disable=desktop-notifications
xi --resume --disable=auto-compaction
```

Both `--enable=FEATURE` and `--enable FEATURE` are accepted (likewise `--disable`).
The last occurrence for each feature wins, in command-line order. Unknown names
and non-boolean configuration values are errors. CLI overrides never rewrite
`config.toml`.

Precedence is **CLI > saved session value (session-scoped features only) > config
> built-in default**. Session-scoped effective values are recorded when the
conversation is first used and before work using changed values. They restore on
resume and follow the retained history when branching. Legacy sessions without
saved values use configuration/defaults. Starting a new session discards the old
session's preferences, not invocation-wide CLI overrides.

IPC enablement and notifications are never restored from conversation files.
Headless `--print` mode continues to disable IPC and desktop notifications;
explicit CLI enablement of these features in that mode is rejected. Automatic
compaction is configurable in both modes; disabling it does not disable manual
`/compact`.

Keep feature-specific parameters in their existing sections, for example:

```toml
[hook_ipc]
endpoint = "custom-endpoint"
```

The legacy `--enable-session-ipc` option remains supported but is hidden from
help. It participates in the same last-wins ordering as `--enable=session-ipc`.
Legacy `enable_session_ipc` and `[hook_ipc].enabled` configuration is still read;
explicit `[features]` entries win. When xi saves configuration it writes the
canonical `[features]` entries without expanding unspecified defaults.

Cargo build features, such as `restart`, remain separate from runtime features.

---

## Display thresholds

These control how much content is shown in the UI during and after a turn.
They are presentation choices and do not affect how much content is sent to
the model (those limits are separate and not user-configurable).

```toml
[display]
# Maximum lines of a shell command shown in the live turn view
max_shell_command_lines = 5

# Characters before a command label switches from single-line to multi-line display
max_one_line_chars = 120
```

---

## Theme

To use a custom theme file:

```toml
theme = "~/.config/xi/my-theme.toml"
```

See [THEME.md](THEME.md) for the full theme file format and all available options.
