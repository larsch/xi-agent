# Xi Configuration Guide

Xi uses a platform-specific `config.toml`: typically `~/.config/xi/config.toml`
on Linux and `%APPDATA%\xi\config\config.toml` on Windows. Run `xi --print-dirs`
to find the actual path on your system. All fields are optional; missing values
fall back to built-in defaults.

---

## Runtime features

Use one table for optional behavior. Every feature has its own built-in default;
only explicitly configured values are saved.

```toml
[features]
session-ipc = false
auto-compaction = true
desktop-notifications = true
terminal-bell = false
hook-ipc = false
```

| Feature | Default | Scope |
|---|---|---|
| `session-ipc` | Disabled | Process: worktree control endpoint and `agent_session` tool |
| `hook-ipc` | Disabled | Process: best-effort hook-event publisher |
| `desktop-notifications` | Enabled | Process: notifications while the terminal is unfocused |
| `terminal-bell` | Disabled | Process: terminal attention bell while unfocused, independent of toasts |
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
Headless `--print` mode disables IPC, desktop notifications and terminal bells;
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

## Desktop notification backend

Notifications are sent when a final response arrives or the agent asks a question,
only while the terminal is unfocused. The `desktop-notifications` feature above
controls both backends; `xi --disable=desktop-notifications` disables either one.
Headless `--print` mode never sends desktop notifications.

Set this **top-level** key before any TOML table headers:

```toml
notification_backend = "native" # Default: existing OS-native notifications
```

The experimental alternative is `notification_backend = "terminal"`. It sends
`OSC 777 ; notify ; title ; body ST` through the terminal, allowing a supporting
terminal to display the toast on the terminal host (including remote sessions,
if the connection passes the sequence through). Titles use the working-directory
name, and bodies retain the existing 200-character limit. Terminal control
characters are replaced with spaces; semicolons in titles are also replaced.
Terminal output is skipped if stdout is not a terminal.

### Windows Terminal Preview setup

Windows Terminal Preview 1.26.2734.0 supports OSC 777, but **permission is disabled
by default**. In the active profile in Windows Terminal's `settings.json`, enable:

```json
"compatibility.allowOSC777": true
```

This is a profile property (or a property of `profiles.defaults`), not a root-level
setting. It permits applications using that profile to request desktop toasts.
xi does not change Windows Terminal settings for you. See the
[upstream implementation](https://github.com/microsoft/terminal/pull/20012).

To test the protocol independently, run this in an interactive PowerShell 7
terminal, then switch to another application within five seconds:

```powershell
Start-Sleep -Seconds 5
[Console]::Write("`e]777;notify;xi test;Terminal notification test`e" + [char]92)
```

The sequence terminates with ESC followed by a backslash. Clicking the toast
should bring the originating terminal tab forward. Windows notification settings
or Do Not Disturb can still prevent a visible toast.

Then select `notification_backend = "terminal"`, restart xi, submit a prompt, and
switch away before the final response or `ask_user` question arrives. Verify a
single toast appears and clicking it returns to the originating tab. Repeat while
focused and with `--disable=desktop-notifications`; neither should send a toast.

### Fallback and prototype limits

Switch back to `notification_backend = "native"` (or remove the key) and restart
xi to restore native notifications. Invalid backend names are configuration
errors. The selected backend is not stored in conversations.

Selection is deliberately explicit: `WT_SESSION` identifies Windows Terminal,
not its OSC 777 permission. OSC 777 has no delivery acknowledgement; a successful
write does not establish that a toast appeared. Unsupported or permission-blocked
terminals may silently ignore it, so there is **no automatic fallback** or second
native toast. Write errors are logged without interrupting the agent.

The prototype implements OSC 777 only, not OSC 9 or Kitty's OSC 99. Terminal
multiplexers such as tmux may filter the sequence; passthrough wrapping and
automatic capability probing are not implemented.

---

## Terminal attention bell

The independent `terminal-bell` feature is disabled by default. When enabled,
xi writes one standalone BEL (`0x07`) at the same final-response and question
points as desktop notifications, only while the terminal is unfocused and stdout
is a terminal. It works with either notification backend, or with toasts disabled:

```console
xi --enable=terminal-bell
xi --enable=terminal-bell --disable=desktop-notifications
```

To persist it, add `terminal-bell = true` to your existing `[features]` table.
Use `--disable=terminal-bell` to override it for one invocation. It is not restored
from conversations; headless mode never rings, and explicitly enabling it with
`--print` is an error.

The terminal controls the effect of BEL. In Windows Terminal Preview 1.26, it
sets a bell indicator on the tab; for an inactive tab the indicator remains until
the tab gains focus. OSC 777 toasts alone do not set that indicator, and a BEL
used to terminate OSC 777 is not a standalone bell.

For a silent tab indicator plus taskbar flashing, set this property in the
Windows Terminal profile:

```json
"bellStyle": "taskbar"
```

This policy affects all applications in that profile. The default `audible`
policy can make a sound; `none` suppresses the tab indicator too. Avoid
`notification` or `all` when combining BEL with OSC 777, or Windows Terminal can
produce a second, bell-generated toast. xi does not override terminal bell policy.

Manual test in PowerShell 7 (switch to another tab during the delay):

```powershell
Start-Sleep -Seconds 3; [Console]::Write([char]7)
```

Verify the inactive tab gains an indicator and focusing it clears the indicator.
Then test xi with `--enable=terminal-bell --disable=desktop-notifications`: switch
tabs before the final response or question and verify the indicator without an
xi toast. While focused, or with `--disable=terminal-bell`, xi should not ring.

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
