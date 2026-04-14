# Roadmap

Items are grouped by priority.

---

## ✅ Completed

- Text-based UI with input box and streaming output
- Core agent loop (streaming, tools, thinking output)
- Multiple providers (Copilot, OpenAI, Codex, Gemini, Ollama)
- Interactive authentication
- Basic tools (file read/write/edit, find files, ask user, shell commands)
- Bash on Unix, PowerShell and cmd.exe on Windows
- SKILL.md support
- AGENTS.md support
- Session persistence (resume conversations)
- Steering (type messages while agent loop is running)
- Markdown rendering in assistant output
- Open WebUI and Anthropic provider support

---

## 🟡 Medium priority

### Platform credential storage
Move secrets out of `auth.toml` and into the platform credential store:
- macOS Keychain
- Windows Credential Manager
- Linux Secret Service / keyring

Keep only non-secret metadata in the tau app config directory.

### Context compaction
Long agentic sessions are now compacted automatically before the model's
context window fills. Tau uses provider-reported usage when available to
trigger post-turn compaction, falls back to estimated context size when
needed, retries once after context-overflow errors, and records structured
compaction summaries in the session log. Users can also trigger compaction
manually with `/compact [instructions]` to steer what the summary should keep
or omit.

Tracked in Gitea: [issue #11](https://gitea.belunktum.dk/larsch/tau/issues/11).

---

## 🟢 Low priority

---

## ⚪ Out of scope (for now)

### Safety guardrails
Tool use remains unrestricted.
