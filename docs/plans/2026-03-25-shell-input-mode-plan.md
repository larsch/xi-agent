Date: 2026-03-25
Status: Proposed
Priority: High

Goal
- Add shell command input mode triggered by leading `!` with lightweight visual distinction, fast shell switching, and persisted local transcript that is excluded from AI request payloads.

Product constraints (amended)
- No shell-mode headline/banner.
- Only a subtle Ctrl+S hint.
- Input prefix must stay minimal: `[bash/cmd/PS] <PWD><prompt-char> `
- Prompt char must vary by shell type:
  - bash: `$`
  - cmd: `>`
  - PS: `>`

Behavior spec
1) Enter shell mode
- If chat input is empty and user presses `!`, consume `!` and switch to shell mode.
- Do not keep literal `!` in the buffer after mode switch.

2) Shell mode editing
- Separate edit buffer from normal chat input.
- Prefix shown inline before editable command text:
  - `[bash] ~/repo$ `
  - `[cmd] C:\repo> `
  - `[PS] C:\repo> `
- Subtle hint only (dim/right-aligned where possible): `Ctrl+S switch`.

3) Exit shell mode
- Backspace on empty shell input -> return to normal input mode (equivalent to deleting `!`).
- Ctrl+D on empty shell input -> return to normal input mode.
- Ctrl+C in shell mode -> return to normal input mode (must not quit app).

4) Shell selection
- Ctrl+S cycles through available shells for current platform.
- Preserve typed command text when cycling shells.

5) Execute
- Enter runs command in currently selected shell.
- Keep user in shell mode after execution for rapid iteration.

6) Output handling
- Show output in chat log with clear local-shell styling.
- Save output in session persistence.
- Do not include shell command/output entries in LLM request messages.

Technical design

A) New shell mode state in App
- Add `InputMode` enum:
  - `Chat`
  - `Shell { selected: ShellKind, available: Vec<ShellKind> }`
- Add shell buffer (`TextArea`) separate from chat buffer.
- Add helpers:
  - `enter_shell_mode()`
  - `exit_shell_mode()`
  - `cycle_shell()`
  - `shell_input_is_empty()`
  - `submit_shell_command()`

B) Shell runtime module
- Add `src/shell.rs`:
  - `ShellKind` enum: `Bash`, `Cmd`, `PowerShell`
  - `discover_available_shells()` (platform aware)
  - `run_shell_command(kind, cwd, command) -> ShellRunResult`
  - `prompt_char(kind) -> char`
  - `display_name(kind) -> "bash" | "cmd" | "PS"`
- Use async subprocess via `tokio::process::Command`.

C) Session + LLM exclusion
- Extend `llm::Message` with `include_in_llm: bool` (`#[serde(default = "default_true")]`).
- Default constructors set `include_in_llm = true`.
- Shell transcript messages set `include_in_llm = false`, `hidden = false`.
- In `App::submit`, `submit_with_text`, `retry_last_request` filter on `include_in_llm` when building provider request messages.

D) Event loop changes (main.rs)
- Before global Ctrl+C quit behavior, branch on shell mode:
  - in shell mode Ctrl+C => exit shell mode, continue loop.
- In chat mode:
  - detect `!` as mode switch only when input is empty.
- In shell mode:
  - Ctrl+S => cycle shell
  - Backspace empty => exit shell mode
  - Ctrl+D empty => exit shell mode
  - Enter => execute shell command
  - Esc behavior unchanged for global agent/login handling unless explicitly conflicting

E) UI changes (ui.rs)
- Add shell input background color distinct from normal input.
- No headline block.
- Render one-line prefix + editable command line with wrapping support.
- Prefix format exactly: `[name] <PWD><prompt-char> `
- Add subtle hint only (dim text): `Ctrl+S switch`.
- Render shell transcript entries in log:
  - command line entry (local shell icon/label)
  - output block entry (stdout/stderr merged, exit note if non-zero)

Data model for shell transcript
- Add two helper constructors on `Message` (optional but recommended):
  - `Message::local_shell_command(shell_label, cwd, command)`
  - `Message::local_shell_output(text, is_error)`
- Both set `include_in_llm = false`.

Execution details
- Command entry formatting example:
  - `⚙ [bash] ~/repo$ ls -la`
- Output entry formatting:
  - content only; add `exit N` line for non-zero exit.

Validation and edge cases
- Empty Enter in shell mode: no-op.
- If only one shell is available, Ctrl+S does nothing (no warning noise).
- If shell spawn fails, record a visible local error output entry (still `include_in_llm = false`).
- Keep autoscroll behavior consistent with normal new messages.

Implementation order
1. Add `include_in_llm` field and request filtering.
2. Add shell module (`src/shell.rs`) and subprocess execution.
3. Add app shell state + helpers.
4. Wire key handling in `main.rs`.
5. Implement UI prefix rendering + subtle hint + shell theme.
6. Add transcript rendering polish.
7. Add/adjust tests.

Test plan
- App unit tests:
  - enter shell mode from leading `!`
  - backspace/Ctrl+D/Ctrl+C exits shell mode only
  - Ctrl+S cycles and wraps
  - shell messages excluded from LLM payload
- UI tests:
  - no shell headline rendered
  - prefix format `[name] <PWD><prompt-char> `
  - subtle hint present (dim) without dominant chrome
- Shell runner tests:
  - stdout/stderr capture
  - non-zero exit appends `exit N`
- Session tests:
  - shell transcript persists and reloads
  - `include_in_llm=false` survives roundtrip

Verification gates
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
