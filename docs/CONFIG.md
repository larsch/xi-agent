# Xi Configuration Guide

Xi is configured via `~/.config/xi/config.toml`. All fields are optional;
missing values fall back to built-in defaults.

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
