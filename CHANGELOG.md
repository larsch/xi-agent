# Changelog

## v0.4.0 — Unreleased

### Added

- **Prompt cache support**: OpenAI and Anthropic providers now report
  cached-token counts in the info bar. Anthropic sends automatic ephemeral
  `cache_control` annotations on every request (matching API spec), OpenAI
  sends a stable session ID to improve cache routing. Unexpected cache
  misses (zero cached tokens despite a recent cache-populating turn)
  surface a ⚠️ suffix in the info bar context display.
- **Session resume context usage**: the info bar now shows the last known
  token utilisation immediately when resuming a session, instead of showing
  only the context window size until the next turn completes.
- **DeepSeek V4 context window**: hard-coded 1M token fallback entries for
  `deepseek-v4-flash` and `deepseek-v4-pro` (kludge until upstream metadata
  is available).
- **Alt+S shortcut**: toggle the info bar on and off without `/info`.

### Fixed

- **Steering race condition**: pressing Enter during streaming now defers
  the user message until the current assistant turn and all its tool calls
  are committed, preventing transcript corruption and tool-call skipping.
  Explicit cancellation still interrupts immediately.
- **Attachment ordering**: synthesized `read_file` events from `@filename`
  attachments are now placed after the submitted user prompt in the event
  stream, matching provider expectations.
- **`--print` model override**: the `--model` flag now correctly overrides
  the configured model in non-interactive mode.
- **`@file` missing-file handling**: references to nonexistent files are
  now silently ignored (no synthetic tool call, no error notice, no
  provider error). The `@file` text remains in the prompt unchanged.
- **Anthropic `cache_control` placement**: removed the invalid top-level
  `cache_control` field and moved it to individual content blocks, fixing
  400 errors from the Copilot Anthropic proxy and restoring prompt caching
  on both direct and proxied routes.
- **Info bar token persistence**: previous-turn token usage (input size,
  cached size) remains visible when starting a new prompt, instead of
  clearing at turn launch. Still resets correctly on `/new`.
- **Codex prompt cache hits**: parse `input_tokens_details.cached_tokens`
  from the OpenAI Responses API in the Codex provider, so cache-hit
  indicators appear for Copilot GPT-5.x models.

## v0.3.0 — 2026-05-31

### Added

- **`@filename` attachments**: type `@` in the chat input to get interactive
  file completion and attach file contents directly to your message.
- **Live subprocess output**: bash/exec tool output now streams into the UI in
  real time instead of appearing only after the command finishes.
- **Auto model picker**: when a provider has no model configured xi automatically
  opens the model picker so you can choose one without extra navigation.
- **Action verb placeholders**: while a tool call is still streaming in, the UI
  shows a meaningful verb (e.g. "Reading…", "Editing…") instead of a blank line.
- **Unified truncation indicators**: long tool outputs use consistent dimmed
  italic placeholder markers to signal hidden content.

### Fixed

- `edit_file` now returns an error if `old_text` and `new_text` are identical,
  preventing silent no-ops.
- Tool call pending labels appear immediately before any argument JSON arrives.
- `ask_user` question blocks render on the default background, consistent with
  agent response blocks; answers appear as normal user message blocks.
- `edit_file` diff truncation markers are now coloured by side (add/remove).
- Common-line diff placeholders are omitted for pure-addition or pure-removal
  hunks, reducing noise.

## v0.2.0 — 2026-05-24

First public release. xi is a focused AI agent for the terminal.

- **Multiple LLM providers**: OpenAI, Anthropic, Google Gemini, GitHub Copilot,
  Ollama, OpenRouter, Codex
- **Built-in tools**: read_file (with image support), write_file, edit_file,
  find_files, ask_user, bash, exec, python, custom user-defined tools, cmd
  (Windows), powershell (Windows)
- **Interactive TUI** with streaming responses, thinking tokens, tool call
  previews, session persistence, file change detection
- **Skills system**: pluggable AGENTS.md / SKILL.md expertise from home and
  project directories
- **Session management**: resume past sessions, session branching,
  compaction for long conversations
- **Non-interactive mode**: `xi --print "..."` for pipe-friendly inference
- **Custom tools**: executable protocol with `--describe` JSON interface

## v0.1.0 — Unreleased

Initial development. Internal use only prior to the v0.2.0 public release.
