# Custom Tools

Custom tools let you extend xi with small executables, without building them
into xi-agent. xi discovers tools when it loads its agent context at startup;
use `/reload` to discover changes during an interactive session.

## Discovery locations

xi searches these directories in order:

1. `~/.xi/tools/` — user tools
2. `./.xi/tools/` — tools for the current project
3. The xi configuration directory's `tools/` folder

Use `xi --print-dirs` to see the resolved paths. A tool executable must be
runnable on the current platform. On Unix, mark it executable (for example,
`chmod +x ~/.xi/tools/my-tool`).

## Tool protocol

Each executable must support two modes:

1. When invoked with `--describe`, write a JSON descriptor to stdout and exit
   successfully.
2. When invoked normally, read one UTF-8 JSON argument object from stdin and
   write the tool result to stdout. A non-zero exit status is treated as an
   error.

The descriptor must provide a tool name, description, and JSON Schema object
for its input parameters. `prompt_snippet` is optional; when present and
non-empty, it gives the model a concise hint about when to use the tool. The
full description is still provided as the tool's description.

Example descriptor:

```json
{
  "name": "word_count",
  "description": "Count words in the supplied text.",
  "prompt_snippet": "Count words in text",
  "parameters_schema": {
    "type": "object",
    "properties": {
      "text": {
        "type": "string",
        "description": "Text to count"
      }
    },
    "required": ["text"]
  }
}
```

Example executable in Python:

```python
#!/usr/bin/env python3
import json
import sys

if sys.argv[1:] == ["--describe"]:
    print(json.dumps({
        "name": "word_count",
        "description": "Count words in the supplied text.",
        "prompt_snippet": "Count words in text",
        "parameters_schema": {
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "Text to count"}
            },
            "required": ["text"]
        }
    }))
    raise SystemExit(0)

args = json.load(sys.stdin)
print(len(args["text"].split()))
```

Save it in a discovery directory, make it executable, then restart xi or run
`/reload`. The tool appears alongside built-in tools. Built-in tool names take
precedence over custom tools with the same name.

## Input and output guidance

The input object is UTF-8 JSON on stdin. Return plain UTF-8 text or JSON on
stdout; send diagnostics to stderr. Keep stdout reserved for the result. Avoid
putting rich or large structured payloads into shell arguments: use a
well-documented file or stdin interface instead.

Custom tools are executable code and run with the permissions of the xi
process. xi does not request confirmation before invoking them. Install only
tools you trust and consider what filesystem and network access they need.

xi passes custom tool output through unchanged: it applies no line or byte
truncation to the result sent to the model. A custom tool owns its output
bounds, so keep responses a reasonable size yourself (for example, summarise or
page large results). The session log still shows only a preview of a long body;
expanding the block reveals the full output.

## Built-in sudo tool (Unix)

Interactive Unix sessions also provide a built-in `sudo` tool. It accepts a
single `command` string and runs it through `sudo -A /bin/sh -c`. The parent xi
process displays an approval menu only when sudo requests a password:

- **Deny** (the default)
- **Approve once**
- **Approve once, save password**
- **Approve all in this session**

Passwords are entered in a masked transient prompt and are held only in
memory. They are never included in tool output, conversation history, logs, or
session files. Exiting xi clears saved approval and password state. The sudo
tool is unavailable in non-interactive print mode and is Unix-only.
