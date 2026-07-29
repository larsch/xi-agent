# Changelog

## v0.5.0 — 2026-07-20

### Added

- **Ctrl-Z suspend/resume**: idle-only Ctrl‑Z suspend and foreground resume
  support. The TUI is recreated on resume to avoid cursor query races (#4, #5, #6).
- **Tab in ask_user**: pressing Tab in the ask_user prompt copies the
  highlighted option into the input field for editing before submitting.

### Fixed

- **Streaming edit diff stabilisation**: prefix-matching and symmetric
  common-lines computation produce fewer flickering diffs during streaming
  `edit_file` results.
- **Throbber gaps**: the bottom gap now shrinks instead of shifting content
  when the throbber appears, and the throbber is hidden only when tool output
  visually grows.
- **Streaming tool call labels**: streaming tool call labels and results use
  `┆` (not `╰`) for a cleaner flowing look.
- **Newlines in user messages**: newlines are now correctly preserved when
  rendering user messages in the log.
- **Provider menu login**: the login option in `/provider` menu no longer
  does nothing.
- **Provider setup prompt**: removed misleading "leave empty to keep
  current" hint from the API key prompt.
- **Ollama context window**: context window is now discovered via
  `/api/ps` instead of GGUF metadata; model tags are normalised in the
  cache.
- **`--model` CLI override**: the `--model` flag is now correctly applied
  to the provider instance on startup.
- **Step-back ask_user**: the ask_user question text is preserved when
  stepping back and re-answering.
- **Windows build**: `fmt`, `clippy`, and `check` now pass with zero
  warnings on Windows; cross-compile build scripts fixed.

### Internal

- `main.rs` decomposed into modules and handler functions.
- Terminal lifecycle extracted into its own module with a `recreate_terminal`
  helper.
- Dimmed hint text removed from provider setup input.
- Python program test command added.

## v0.4.0 — 2026-07-05

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
- **Theme configuration system**: themeable UI with CSS-style color specs
  and terminal color support via a TOML configuration file.
- **Agent-level hooks system**: pluggable hook points for tool lifecycle
  (`OnToolIntent`), streaming milestones (`OnFirstThinkingToken`,
  `OnFirstTextToken`), and session events (`OnIdle`, `OnCompacting`,
  `OnExternalChange`, `OnStatusUpdate`), with IPC event streaming for
  external tooling.
- **Alt+C shortcut**: copy the last assistant response to the system
  clipboard.
- **Step-back navigation**: step back through ask_user answers with full
  prompt UI restoration, enabling re-answering or branching from any
  decision point.
- **Mouse text selection**: click-drag to select and copy text from the
  log view.
- **Similar session scope**: a "similar" scope between `local` and
  `foreign` in the session resume picker for faster filtering.
- **`/new` resets FileTracker and reloads skills**, ensuring a clean
  environment for each fresh session.
- **`@file` backtick notation**: `@filename` mentions are rewritten to
  backtick-quoted paths for LLM consumption, reducing confusion.
- **`read_skill` and `edit_skill` tools**: embedded tools for listing,
  loading, and editing skills, with filesystem paths and scope
  indicators.
- **User message markdown rendering**: user prompts are now rendered with
  markdown formatting in the log view, matching assistant output.
- **Keyboard shortcuts help**: accessible via `?` key.
- **Block content alignment**: all block content consistently aligned to
  column 3 with margin markers.
- **Edit diff fillers**: adjacent "total lines" and "common lines" fillers
  in `edit_file` diffs are collapsed to reduce noise.
- **Provider synthesis**: built-in provider instances synthesised
  automatically; explicit provider selection required for ambiguous
  configurations.
- **OSC 52 clipboard**: clipboard integration via OSC 52 escape sequences,
  replacing the `arboard` crate for broader terminal compatibility.
- **Python 🐍 emoji**: the built-in Python tool now shows a Python emoji
  icon.

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
- **Copilot auth**: full token (with metadata) is now stripped correctly
  before use as Bearer auth, fixing authentication failures.
- **ask_user rendering**: questions now stream from partial data during
  tool call rendering and appear in the log; rendering layout improved.
- **Provider switching**: changing providers no longer uses a stale model
  list for fetching; model list auto-fetches on startup even when a model
  is already configured.
- **Config load failures** are now fatal, preventing silent overwrite of
  user configuration with defaults.
- **Anthropic null tool_args**: providers now guard against null tool
  arguments in Anthropic wire format.
- **Auth token expiry**: standardized across backends with a new
  `OAuthBackend` trait and test infrastructure.
- **Serde error messages**: Rust struct names in serde errors are now
  translated to model-friendly JSON concepts.
- **Tool descriptions**: reworded to prevent redundant `2>&1` and
  absolute-path annotations in shell commands.
- **System prompt** clarifies that file paths are relative to the working
  directory.
- **Input panel**: scroll-to-cursor behavior added when text exceeds the
  viewport.
- **Tool invocation labels**: leading and trailing empty lines trimmed;
  placeholder labels shown consistently during streaming; tool icons
  render normally even when labels are italic.
- **Output trimming**: leading and trailing empty wrapped lines removed
  from output blocks; body line limit enforced on wrapped visual lines,
  not logical lines.
- **Thinking display**: final streaming line uses ┆ instead of ╰; blank
  separator line removed between thinking and response; display stabilized
  by truncating wrapped lines.
- **Throbber**: no longer sticks or blocks Escape during token refresh;
  remains visible during retry; refreshes correctly on tool intent, args
  delta, and output chunks.
- **Streaming blocks**: padded during shrink to prevent layout jitter;
  partial-JSON headline blink eliminated.
- **Markdown**: extra blank line after pre-formatted text removed;
  HTML/XML tags now rendered verbatim instead of silently dropped.
- **OpenAI reasoning content**: always included in assistant wire format;
  OpenAI-specific parameters guarded by backend type.
- **OpenWebUI context window**: auto-discovered for OpenAI-compatible
  backends.
- **PowerShell** now runs noninteractively.
- **Windows build** fixed and tests made cross-platform.
- **Log redraw** now happens before disk I/O on submit for lower perceived
  latency.

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
