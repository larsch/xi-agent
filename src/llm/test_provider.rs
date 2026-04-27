//! A hidden test provider for exercising the tau UI without a real API
//! connection.  Activated via `--provider=test` or `/provider test`.
//! Never appears in the provider selection menu.  Never persists to config.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use async_stream::stream;
use tokio::time::{Duration, sleep};

use super::{AssistantPhase, LlmEvent, LlmStream, Message, ModelListFuture, Role, ToolDefinition};
use crate::llm::ProviderError;

pub struct TestProvider {
    /// Tracks the current step for scripted multi-turn sequences.
    /// 0 = idle (no sequence in progress).
    sequence_step: Arc<AtomicU8>,
    /// PID captured from step 1 of bash-background-job, used in steps 2–4.
    sequence_pid: Arc<std::sync::atomic::AtomicU32>,
}

impl TestProvider {
    pub fn new() -> Self {
        Self {
            sequence_step: Arc::new(AtomicU8::new(0)),
            sequence_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
}

// ── Pre-defined questions ─────────────────────────────────────────────────────

const ASK_QUESTION: &str = "Which option do you prefer?";
const ASK_TYPE_QUESTION: &str = "Please type your answer:";
const ASK_NOTYPE_QUESTION: &str = "Select one of the following:";

// ── Markdown fixture ──────────────────────────────────────────────────────────

const MARKDOWN_FIXTURE: &str = r#"# Markdown Showcase

This document exercises **every** major markdown feature rendered by tau. It is intentionally long and verbose so that scrolling, wrapping, and layout can all be verified in a single pass.

## Text styles

Normal text, **bold text**, *italic text*, ***bold and italic***, and `inline code`. You can also combine styles: **bold with `inline code` inside** or *italic with **nested bold** inside*.

Here is a second paragraph in the same section. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.

## Headings

### Level 3 heading

Some text under a level-3 heading. This paragraph is here to verify that the heading renders at the correct weight and that subsequent body text is clearly distinguished from it.

#### Level 4 heading

Some text under a level-4 heading. Headings at this depth are less common but should still render distinctly from body text.

## Lists

Unordered:

- Alpha — the first item, with a moderately long description to check line wrapping inside a list bullet
- Beta — the second item
  - Nested item one
  - Nested item two, also with a longer description to ensure indented wrapping works correctly
  - Nested item three
- Gamma — the third item
- Delta — the fourth item, added to give the list more visual weight

Ordered:

1. First step — initialize the project and install dependencies
2. Second step — configure the environment and set the required variables
3. Third step — run the test suite and verify all assertions pass
4. Fourth step — build the release artifact and publish

## Code block

```rust
fn main() {
    // This is a moderately long code block to verify that horizontal
    // scrolling or wrapping behaves correctly inside a fenced block.
    let message = "Hello from the test provider!";
    let repeated: String = std::iter::repeat(message).take(3).collect::<Vec<_>>().join(", ");
    println!("{repeated}");
}
```

```json
{
  "provider": "test",
  "model": "test",
  "commands": ["help", "markdown", "echo", "slow", "thinking", "status", "error", "system", "ask", "bash", "exec"],
  "persistent": false
}
```

## Blockquote

> The test provider exists so you can exercise the tau UI without burning real tokens or requiring authentication. It simulates every kind of LLM event — text tokens, thinking tokens, tool calls, status updates, and errors — through a simple command interface.
>
> Nested quote:
>
> > This is a nested blockquote. It should be indented further than the outer quote and still wrap correctly at the terminal width.

## Commands table

| Command                 | Arguments       | Description                                                                 |
|-------------------------|-----------------|-----------------------------------------------------------------------------|
| `help`                  | —               | Show a list of all available test provider commands                         |
| `markdown`              | —               | Stream this rich markdown document in full                                  |
| `echo <text>`           | text            | Stream the provided text back to the UI token by token without any delay    |
| `slow <text>`           | text            | Stream the provided text word by word with a 150 ms artificial delay        |
| `thinking <text>`       | text            | Emit a simulated chain-of-thought block followed by the provided answer     |
| `status <msg>`          | message         | Emit a transient StatusUpdate event and then confirm with a short response  |
| `error`                 | —               | Emit a provider error to test the error-display path in the UI              |
| `system`                | —               | Retrieve and display the full system prompt inside a fenced code block      |
| `ask [question]`        | question        | Invoke ask_user with three named options and freeform input enabled         |
| `ask-type [q]`          | question        | Invoke ask_user with freeform input only and no predefined option list      |
| `ask-notype [q]`        | question        | Invoke ask_user with three options and freeform input disabled              |
| `bash <cmd>`            | shell command   | Execute a real bash command and echo the result as a fenced code block      |
| `powershell <cmd>`      | shell command   | Execute a real powershell command and echo the result as a fenced code block |
| `cmd <cmd>`             | shell command   | Execute a real cmd command and echo the result as a fenced code block       |
| `exec <prog> [args…]`   | prog + args     | Execute a program via argv (shellword-split); no shell interpretation        |
| `bash-background-job`   | —               | 4-step scripted loop: start sleep 60, check running, kill, confirm gone     |
| `write`                 | —               | Issue a write_file tool call that writes a file to the system temp directory |

## Wide data table

This table has many columns and long cell values to stress-test column sizing and text wrapping behaviour.

| ID  | Component         | Status      | Owner              | Last Updated | Notes                                                                 |
|-----|-------------------|-------------|--------------------|--------------|-----------------------------------------------------------------------|
| 001 | LLM provider      | ✅ Complete  | @larsch            | 2026-04-06   | Supports streaming, tool calls, thinking tokens, and status updates   |
| 002 | Test provider     | ✅ Complete  | @larsch            | 2026-04-06   | Hidden from menu; never persists; activated via --provider=test       |
| 003 | Markdown renderer | 🔄 Ongoing  | @larsch            | 2026-04-05   | Tables, blockquotes, and nested lists under active refinement         |
| 004 | ask_user tool     | ✅ Complete  | @larsch            | 2026-03-20   | Supports options, freeform, and cancellation via Escape               |
| 005 | Session export    | ✅ Complete  | @larsch            | 2026-03-15   | Exports full conversation history to an HTML file with syntax highlighting |
| 006 | Thinking tokens   | ✅ Complete  | @larsch            | 2026-04-01   | Gemini, Codex, and Copilot Responses route all supported              |
| 007 | Token compaction  | 🔄 Ongoing  | @larsch            | 2026-04-04   | Reserve-based strategy; square-root budget curve                      |
| 008 | Windows support   | ⚠️ Partial  | @larsch            | 2026-03-28   | Bracketed paste heuristic in place; some edge cases remain on conhost |

## Done

End of markdown fixture. Scroll back up to verify that all sections rendered correctly.
"#;

// ── Emoji fixture ─────────────────────────────────────────────────────────────

/// A fixture that lists every emoji used in tau's tool labels so that
/// rendering alignment can be verified visually in the terminal.
///
/// Each line shows: the emoji (as tau would render it in a tool label),
/// a pipe, then a descriptive name.  The pipe should be vertically aligned
/// if all emojis advance the cursor by exactly 2 columns.
const EMOJI_FIXTURE: &str = "\
Emoji alignment test — the `|` column shows actual terminal cursor advance.\n\
Alignment may vary by terminal, font, and render state.\n\
\n\
```\n\
👀 | read_file  (U+1F440, wide)\n\
✏️ | write_file (U+270F+VS16)\n\
📝 | edit_file  (U+1F4DD, wide)\n\
💻 | bash       (U+1F4BB, wide)\n\
🔍 | find_files (U+1F50D, wide)\n\
❓ | ask_user   (U+2753, wide)\n\
⚙️ | exec/other (U+2699+VS16)\n\
🕹️ | steering   (U+1F579+VS16)\n\
⚠️ | warning    (U+26A0+VS16)\n\
✅ | checkmark  (U+2705, wide)\n\
❌ | red cross  (U+274C, wide)\n\
```\n\
";

// ── Help text ─────────────────────────────────────────────────────────────────

const HELP_TEXT: &str = r#"Test provider commands:

  help                  Show this help
  markdown              Stream a rich markdown document
  emoji                 Show emoji alignment test for all tool label glyphs
  echo <text>           Stream text back token by token
  slow <text>           Stream text with artificial delays
  thinking <text>       Emit thinking tokens then a text answer
  status <msg>          Emit a StatusUpdate event then confirm
  error                 Emit a provider error
  system                Show the full system prompt

  ask [question]        ask_user with options + freeform
  ask-type [question]   ask_user freeform only (no options)
  ask-notype [question] ask_user options only (no freeform)

  bash <command>        Execute a bash command (real)
  powershell <command>  Execute a powershell command (real)
  cmd <command>         Execute a cmd command (real)
  exec <prog> [args…]   Execute a program directly via argv (shellword-split, no shell)

  bash-background-job   4-step scripted loop: start sleep 60 in background,
                        check it is running, kill it, confirm it is gone

  write                 Issue a write_file tool call that writes a file to
                        the system temp directory
"#;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Stream a static string as word-sized tokens with an optional per-word delay.
fn stream_text(text: &'static str, delay: Option<Duration>) -> LlmStream {
    Box::pin(stream! {
        for word in text.split_inclusive(' ') {
            if let Some(d) = delay {
                sleep(d).await;
            }
            yield LlmEvent::Token {
                text: word.to_string(),
                phase: AssistantPhase::Final,
            };
        }
        yield LlmEvent::Done;
    })
}

/// Stream an owned string as word-sized tokens.
fn stream_owned(text: String) -> LlmStream {
    Box::pin(stream! {
        for word in text.split_inclusive(' ').map(ToOwned::to_owned).collect::<Vec<_>>() {
            yield LlmEvent::Token {
                text: word,
                phase: AssistantPhase::Final,
            };
        }
        yield LlmEvent::Done;
    })
}

/// Build a tool call stream for `ask_user`.
fn ask_user_stream(
    question: String,
    options: &'static [&'static str],
    allow_freeform: bool,
) -> LlmStream {
    let options_json: serde_json::Value = options
        .iter()
        .map(|s| serde_json::Value::String(s.to_string()))
        .collect::<Vec<_>>()
        .into();

    Box::pin(stream! {
        yield LlmEvent::ToolCallStart {
            id: "test-1".to_string(),
            name: "ask_user".to_string(),
        };
        yield LlmEvent::ToolCall {
            id: "test-1".to_string(),
            name: "ask_user".to_string(),
            args: serde_json::json!({
                "question": question,
                "options": options_json,
                "allowFreeform": allow_freeform,
            }),
        };
        yield LlmEvent::Done;
    })
}

/// Build a tool call stream for `write_file` targeting the system temp directory.
fn write_file_stream() -> LlmStream {
    let path = std::env::temp_dir()
        .join("tau-test-write.txt")
        .to_string_lossy()
        .into_owned();
    Box::pin(stream! {
        yield LlmEvent::ToolCallStart {
            id: "test-1".to_string(),
            name: "write_file".to_string(),
        };
        yield LlmEvent::ToolCall {
            id: "test-1".to_string(),
            name: "write_file".to_string(),
            args: serde_json::json!({
                "path": path,
                "content": "Hello from the tau test provider!\n",
            }),
        };
        yield LlmEvent::Done;
    })
}

/// Build a tool call stream for a shell tool (bash / powershell / cmd).
fn shell_tool_stream(tool_name: String, command: String) -> LlmStream {
    Box::pin(stream! {
        yield LlmEvent::ToolCallStart {
            id: "test-1".to_string(),
            name: tool_name.clone(),
        };
        yield LlmEvent::ToolCall {
            id: "test-1".to_string(),
            name: tool_name,
            args: serde_json::json!({ "command": command }),
        };
        yield LlmEvent::Done;
    })
}

/// Build a tool call stream for the `exec` tool using an argv parsed from
/// `invocation` via simple shellword (shlex) splitting.
///
/// The first token is `program`; the remainder become `args`.
/// Returns an error stream if `invocation` is empty or unparseable.
fn exec_tool_stream(invocation: String) -> LlmStream {
    let tokens = match shlex::split(&invocation) {
        Some(t) if !t.is_empty() => t,
        _ => {
            let msg = if invocation.trim().is_empty() {
                "exec: no program specified\n".to_string()
            } else {
                format!("exec: failed to parse shellwords from: {invocation}\n")
            };
            return stream_owned(msg);
        }
    };
    let program = tokens[0].clone();
    let args: Vec<String> = tokens[1..].to_vec();
    Box::pin(stream! {
        yield LlmEvent::ToolCallStart {
            id: "test-1".to_string(),
            name: "exec".to_string(),
        };
        yield LlmEvent::ToolCall {
            id: "test-1".to_string(),
            name: "exec".to_string(),
            args: serde_json::json!({ "program": program, "args": args }),
        };
        yield LlmEvent::Done;
    })
}

// ── LlmProvider impl ──────────────────────────────────────────────────────────

impl TestProvider {
    /// Drive the bash-background-job scripted sequence.
    ///
    /// Steps (called with the tool result from the *previous* step):
    ///   1 → start `sleep 60` in the background and print its PID
    ///   2 → parse PID from result; check process is running with `ps -p <pid>`
    ///   3 → kill it with `kill <pid>`
    ///   4 → verify it is gone with `ps -p <pid>`
    ///   5 → emit final summary text, reset to idle
    fn advance_sequence(&self, step: u8, tool_result: &str) -> LlmStream {
        let seq = Arc::clone(&self.sequence_step);
        let pid_store = Arc::clone(&self.sequence_pid);
        match step {
            1 => {
                seq.store(2, Ordering::SeqCst);
                shell_tool_stream("bash".to_string(), "sleep 60 & echo $!".to_string())
            }
            2 => {
                // Parse PID from the tool result (first token on first non-empty line).
                let pid: u32 = tool_result
                    .lines()
                    .find_map(|l| l.split_whitespace().next()?.parse().ok())
                    .unwrap_or(0);
                pid_store.store(pid, Ordering::SeqCst);
                seq.store(3, Ordering::SeqCst);
                shell_tool_stream("bash".to_string(), format!("ps -p {pid}"))
            }
            3 => {
                let pid = pid_store.load(Ordering::SeqCst);
                seq.store(4, Ordering::SeqCst);
                shell_tool_stream("bash".to_string(), format!("kill {pid}"))
            }
            4 => {
                let pid = pid_store.load(Ordering::SeqCst);
                seq.store(5, Ordering::SeqCst);
                shell_tool_stream("bash".to_string(), format!("ps -p {pid}"))
            }
            _ => {
                // Sequence finished — emit a summary.
                seq.store(0, Ordering::SeqCst);
                pid_store.store(0, Ordering::SeqCst);
                stream_owned(
                    "Background-job sequence complete: started sleep 60, confirmed it was running, killed it, and confirmed it was gone.\n"
                        .to_string(),
                )
            }
        }
    }
}

impl super::LlmProvider for TestProvider {
    fn stream_chat(&self, messages: Vec<Message>) -> LlmStream {
        self.stream_chat_with_tools(messages, vec![])
    }

    fn stream_chat_with_tools(
        &self,
        messages: Vec<Message>,
        _tools: Vec<ToolDefinition>,
    ) -> LlmStream {
        // If a scripted sequence is in progress, advance it on every ToolResult.
        if let Some(last) = messages.last()
            && last.role == Role::ToolResult
        {
            let step = self.sequence_step.load(Ordering::SeqCst);
            if step > 0 {
                return self.advance_sequence(step, &last.content.clone());
            }
            // Not in a sequence — echo the result as before.
            let content = last.content.clone();
            let response = format!("Tool result:\n\n```\n{content}\n```\n");
            return stream_owned(response);
        }

        // Otherwise parse the last user message as a command.
        let input = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.trim().to_string())
            .unwrap_or_default();

        let (cmd, rest) = match input.split_once(char::is_whitespace) {
            Some((c, r)) => (c.to_ascii_lowercase(), r.trim().to_string()),
            None => (input.to_ascii_lowercase(), String::new()),
        };

        match cmd.as_str() {
            "help" => stream_text(HELP_TEXT, None),

            "markdown" => stream_text(MARKDOWN_FIXTURE, None),

            "echo" => {
                let text = if rest.is_empty() {
                    "(nothing to echo)".to_string()
                } else {
                    rest
                };
                stream_owned(text + "\n")
            }

            "slow" => {
                let text = if rest.is_empty() {
                    "(nothing to slow-stream)".to_string()
                } else {
                    rest
                };
                // stream_owned with a per-word delay
                Box::pin(stream! {
                    for word in text.split_inclusive(' ').map(ToOwned::to_owned).collect::<Vec<_>>() {
                        sleep(Duration::from_millis(150)).await;
                        yield LlmEvent::Token {
                            text: word,
                            phase: AssistantPhase::Final,
                        };
                    }
                    yield LlmEvent::Done;
                })
            }

            "thinking" => {
                let answer = if rest.is_empty() {
                    "Thinking complete.".to_string()
                } else {
                    rest
                };
                Box::pin(stream! {
                    let thought = "Let me consider this carefully... The test provider is exercising the thinking UI path. This simulated chain-of-thought is intentionally verbose to give the UI something to render.";
                    for chunk in thought.split_inclusive(' ') {
                        yield LlmEvent::ThinkingToken(chunk.to_string());
                    }
                    for word in answer.split_inclusive(' ').map(ToOwned::to_owned).collect::<Vec<_>>() {
                        yield LlmEvent::Token {
                            text: word,
                            phase: AssistantPhase::Final,
                        };
                    }
                    yield LlmEvent::Done;
                })
            }

            "status" => {
                let msg = if rest.is_empty() {
                    "Test status message".to_string()
                } else {
                    rest
                };
                Box::pin(stream! {
                    yield LlmEvent::StatusUpdate(msg);
                    let confirmation = "Status sent.\n";
                    for word in confirmation.split_inclusive(' ') {
                        yield LlmEvent::Token {
                            text: word.to_string(),
                            phase: AssistantPhase::Final,
                        };
                    }
                    yield LlmEvent::Done;
                })
            }

            "error" => Box::pin(stream! {
                yield LlmEvent::Error(ProviderError::other("test", "test error triggered by 'error' command"));
            }),

            "system" => {
                let system_content = messages
                    .iter()
                    .find(|m| m.role == Role::System)
                    .map(|m| m.content.clone())
                    .unwrap_or_else(|| "(no system prompt found)".to_string());
                let response = format!("System prompt:\n\n```\n{system_content}\n```\n");
                stream_owned(response)
            }

            "ask" => {
                let question = if rest.is_empty() {
                    ASK_QUESTION.to_string()
                } else {
                    rest
                };
                ask_user_stream(question, &["Option A", "Option B", "Option C"], true)
            }

            "ask-type" => {
                let question = if rest.is_empty() {
                    ASK_TYPE_QUESTION.to_string()
                } else {
                    rest
                };
                ask_user_stream(question, &[], true)
            }

            "ask-notype" => {
                let question = if rest.is_empty() {
                    ASK_NOTYPE_QUESTION.to_string()
                } else {
                    rest
                };
                ask_user_stream(question, &["Choice 1", "Choice 2", "Choice 3"], false)
            }

            "bash" => {
                let command = if rest.is_empty() {
                    "echo 'hello from test provider'".to_string()
                } else {
                    rest
                };
                shell_tool_stream("bash".to_string(), command)
            }

            "powershell" => {
                let command = if rest.is_empty() {
                    "Write-Host 'hello from test provider'".to_string()
                } else {
                    rest
                };
                shell_tool_stream("powershell".to_string(), command)
            }

            "cmd" => {
                let command = if rest.is_empty() {
                    "echo hello from test provider".to_string()
                } else {
                    rest
                };
                shell_tool_stream("cmd".to_string(), command)
            }

            "exec" => exec_tool_stream(rest),

            "emoji" => stream_text(EMOJI_FIXTURE, None),

            "bash-background-job" => {
                self.sequence_step.store(1, Ordering::SeqCst);
                self.advance_sequence(1, "")
            }

            "write" => write_file_stream(),

            "" => stream_text("Type 'help' for a list of test provider commands.\n", None),

            _ => {
                let msg = format!("Unknown command: '{cmd}'. Type 'help' for a list.\n");
                stream_owned(msg)
            }
        }
    }

    fn list_models(&self) -> ModelListFuture {
        Box::pin(async { Ok(vec!["test".to_string()]) })
    }
}
