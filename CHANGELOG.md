# Changelog

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
