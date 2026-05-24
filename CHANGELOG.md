# Changelog

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
