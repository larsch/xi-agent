# Plan: Unify per-provider LLM message serialization (Issue #45)

Date: 2026-04-22
Status: **Complete**

## Scope

Extract per-protocol message serialization from individual provider files into a
new shared module `src/llm/provider_format.rs`.

### In scope
- New `src/llm/provider_format.rs` with `to_openai_wire`, `to_anthropic_wire`,
  `to_gemini_wire`, `to_codex_wire`, `to_ollama_wire`
- All five functions apply `normalize_tool_name` consistently
- Refactor `openai.rs`, `anthropic.rs`, `gemini.rs`, `codex.rs`, `ollama.rs`
  to delegate to the shared module
- Dead typed structs (`OaiMessage`, `OllamaMessage`, etc.) removed
- 17 new tests in `provider_format.rs` covering all wire formats

### Out of scope
- `copilot.rs` (already a router, no own serialization)
- Any wire-format behavior changes (pure refactor)

## Result

- `src/llm/provider_format.rs` created (~640 lines incl. tests)
- 6 provider files simplified: −530 lines net across the repo
- All 588 tests pass; `just preflight` clean
