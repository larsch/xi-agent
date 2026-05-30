# Plan: Shared Message-Traversal Core for Wire Serializers

## Problem

`src/llm/provider_format.rs` contains five near-identical functions
(`to_openai_wire`, `to_anthropic_wire`, `to_gemini_wire`, `to_codex_wire`,
`to_ollama_wire`). Each implements the same `while i < messages.len()` /
`match msg.role` traversal, grouping an assistant message with its subsequent
tool-call and tool-result pairs. Per-provider variation is only in how the
resulting groups are serialised to JSON.

Divergence already exists: the standalone `ToolCall` fallback path exists
only in the OpenAI formatter; image handling differs across functions without
being clearly documented.

## Approach

1. Define a `Turn` enum representing one logical unit of conversation:
   ```rust
   enum Turn<'a> {
       System(&'a Message),
       User(&'a Message),
       Assistant {
           msg: &'a Message,
           tool_pairs: Vec<(&'a Message, Option<&'a Message>)>, // (call, result)
       },
       StandaloneToolCall(&'a Message),
       StandaloneToolResult(&'a Message),
   }
   ```
2. Extract `fn group_messages(messages: &[Message]) -> Vec<Turn<'_>>` that
   performs the traversal once.
3. Rewrite each `to_*_wire` function as a mapping over `Vec<Turn>` — the loop
   disappears from each serialiser.
4. Document per-provider deviations (image handling, thinking tokens, system
   message skipping) in a table at the top of the module.

## Affected files

- `src/llm/provider_format.rs`

## Success criteria

- One traversal loop, not five.
- All existing `*_wire_*` unit tests pass unchanged.
- The standalone `ToolCall` divergence between formats is either unified or
  explicitly documented with a comment.
- `cargo clippy` clean.

## Risk

Medium. The traversal logic is subtle (e.g. paired tool calls and results, orphan
tool results). The existing tests provide good coverage; run them after each
serialiser is converted, not all at once.
