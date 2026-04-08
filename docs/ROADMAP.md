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
Long agentic sessions silently degrade when the conversation history exceeds
the model's context window. Implement a soft-limit warning and a truncation
strategy (drop oldest non-system messages, or summarise) before the window
fills.

Tracked in Gitea: [issue #11](https://gitea.belunktum.dk/larsch/tau/issues/11).

---

## 🟢 Low priority

---

## ⚪ Out of scope (for now)

### Safety guardrails
Tool use remains unrestricted.
