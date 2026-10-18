---
name: xi-config-tools
description: Explains xi custom tool discovery, executable protocol, precedence, and loading behavior.
metadata:
  hidden: "true"
---

# Custom tools in xi

Custom tools are executables that extend xi's tool registry. Each executable runs with the permissions of the xi process.

## Discovery and precedence

xi searches `~/.xi/tools`, `./.xi/tools`, and the xi configuration directory's `tools` folder, in that order. `xi --print-dirs` reports resolved directories. A custom tool must be executable on the current platform. Built-in tool names take precedence over custom tools with the same name.

## Executable protocol

When invoked with `--describe`, the executable writes a JSON descriptor to stdout and exits successfully. The descriptor provides `name`, `description`, and a JSON Schema object in `parameters_schema`; `prompt_snippet` is optional.

When invoked normally, it reads one UTF-8 JSON object from stdin and writes a UTF-8 result to stdout. Nonzero exit status is treated as an error. Diagnostics belong on stderr.

## Loading

Custom tools are discovered and described when xi loads its context at startup. `/reload` reloads custom tools. Invalid, non-executable, or failing descriptors are skipped. See `docs/CUSTOM-TOOLS.md` for complete protocol examples.
