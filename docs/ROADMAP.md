# Roadmap

Items are grouped by priority.

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

See [plan](plans/2026-03-14-context-management.md).

---

## 🟢 Low priority

### Markdown rendering (currently just raw text)
Render assistant output as Markdown instead of raw text.

### Anthropic provider support
`AnthropicProvider` implementing `LlmProvider` against the Anthropic
Messages API (`/v1/messages`). Include native tool-calling support and
`thinking` block extraction.

---

## ⚪ Out of scope (for now)

### Safety guardrails
Tool use remains unrestricted.

### Additional built-in tools beyond the current set
No expansion of built-in tools is planned for now.
