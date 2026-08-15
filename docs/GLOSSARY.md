# Glossary

Concepts are ordered from largest scope to smallest.

## Session

A persisted conversation identified by a session ID. Contains the full durable
event log (`SessionEvent` history), which records every user message, assistant
response, tool call, tool result, and compaction summary.

Each session holds two read models derived from the event log: a **display
projection** for TUI rendering and an **LLM projection** for model input.

## Agent loop

The overarching process started by `run_agent_loop()` per user message
submission. It calls the LLM, executes any tool calls the model requests, feeds
results back, and repeats until the model produces a final answer without tool
calls.

One agent loop contains one or more **turns**.

## Turn

One round in the agent loop: a single **LLM invocation** followed by execution
of any **tool calls** the model requested in that response.

A turn begins at the pre-turn hook, streams tokens from the model, executes any
tool calls sequentially, and ends with `AgentEvent::TurnEnd`. The loop then
either begins the next turn (if tool calls were made) or terminates (if the
model gave a final answer).

## LLM invocation

The narrow act of calling the LLM and receiving a streaming response: text
tokens, thinking tokens, and/or tool call requests. Handled by
`stream_assistant_turn()` and represented by `TurnOutcome`.

An LLM invocation is the model-response half of a **turn**; the turn also
includes any tool execution that follows.

## Tool call

A single tool invocation within a turn. When the model requests multiple tool
calls in one response, they are executed sequentially in a batch via
`execute_tool_batch()`.

Each tool call emits `AgentEvent::ToolCallStart` before execution and
`AgentEvent::ToolCallEnd` with the result afterwards.

## Steering message

User input typed while the agent loop is running. Queued in a channel and
rendered with a 📣 icon until the loop consumes it at the next turn boundary.
Consumed steering is inserted into the conversation history after the completed
assistant turn and before the next turn begins.

Steering does not cancel already-emitted tool calls in the current turn.

## Compaction

Automatic or manual context-window management. When token usage crosses a
threshold, or the user invokes `/compact`, the agent generates a structured
summary of older history and appends a `SessionEvent::CompactionSummary`
boundary. The LLM projection substitutes the most recent summary for the older
events it replaces, keeping the conversation within the model's context window.

## Projection

A read model derived from the `SessionEvent` history. The **display projection**
produces rendered lines for the TUI; the **LLM projection** produces the
`Message` list sent to the model. Both are incremental — they cache state and
only recompute from the last seen event boundary.
