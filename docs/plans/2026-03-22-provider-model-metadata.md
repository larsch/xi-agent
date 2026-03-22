# Copilot model routing metadata consolidation

**Date:** 2026-03-22  
**Status:** Planned  
**Priority:** Medium  
**Risk:** Low — internal reorganisation of `src/provider.rs`; no behaviour change  
**Source:** TAU-REVIEW.md §7 (Provider Routing Logic Duplication) and §8 (Thinking Level Mapping)

---

## Problem

When determining how to handle a Copilot request, `src/provider.rs` contains
two independent functions that each do their own model-name prefix matching:

```rust
fn classify_copilot_route(model: &str) -> CopilotApiRoute {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") { ... }
    else if m.contains("codex") || m.starts_with("gpt-5") { ... }
    else { ... }
}

pub fn thinking_support_for(kind: &ProviderKind, model: &str) -> ThinkingSupport {
    match kind {
        ProviderKind::Copilot => match classify_copilot_route(model) {
            CopilotApiRoute::OpenAiResponses => ThinkingSupport::Applied,
            CopilotApiRoute::AnthropicMessages => ThinkingSupport::Ignored("..."),
            CopilotApiRoute::OpenAiChatCompletions => ThinkingSupport::Ignored("..."),
        },
        ...
    }
}
```

Adding a new Copilot-routed model requires touching `classify_copilot_route`
and reviewing `thinking_support_for` separately. The association between a
model prefix, its API route, and whether thinking is applied lives nowhere
explicitly — it is only inferable by reading both functions together.

The same scattered pattern exists for `context_window_for_model` — a separate
50-line chain of `if m.starts_with(...)` — though this function is not
Copilot-specific.

## Goals

1. Introduce a `CopilotModelEntry` struct and a static lookup table
   that associates a model-name prefix with its route and thinking-support
   classification, as the single source of truth for Copilot model behaviour.
2. Rewrite `classify_copilot_route` and `thinking_support_for` to derive from
   the table, eliminating the duplicated matching logic.
3. No behaviour change — the rewritten functions must pass all existing tests.
4. Make the table the natural place to add future model entries.

## Non-goals

- Migrating `context_window_for_model` to the same table in this plan (deferred;
  it is not Copilot-specific and context window sizes are already loosely coupled
  to routing).
- Removing `classify_copilot_route` as a named function — it may stay as a
  thin wrapper over the table lookup.
- Changing `ProviderKind`, `ThinkingSupport`, or `CopilotApiRoute` enums.

## Design

### `CopilotModelEntry` and static table in `src/provider.rs`

```rust
struct CopilotModelEntry {
    /// Model-name prefix to match (case-insensitive).
    prefix: &'static str,
    route: CopilotApiRoute,
    thinking: ThinkingSupport,
}

/// Lookup table for Copilot model routing and thinking support.
///
/// Entries are checked in order; the first matching prefix wins.
/// Models not matching any entry fall through to the default (ChatCompletions).
static COPILOT_MODELS: &[CopilotModelEntry] = &[
    CopilotModelEntry {
        prefix: "claude",
        route: CopilotApiRoute::AnthropicMessages,
        thinking: ThinkingSupport::Ignored(
            "copilot anthropic route has no thinking mapping yet",
        ),
    },
    CopilotModelEntry {
        prefix: "gpt-5",
        route: CopilotApiRoute::OpenAiResponses,
        thinking: ThinkingSupport::Applied,
    },
];

/// Models matched by substring rather than prefix get their own entries.
/// Currently: any model whose name contains "codex".
static COPILOT_MODELS_CONTAINS: &[CopilotModelEntry] = &[
    CopilotModelEntry {
        prefix: "codex",          // matched with `contains`, not `starts_with`
        route: CopilotApiRoute::OpenAiResponses,
        thinking: ThinkingSupport::Applied,
    },
];

/// Default for unrecognised Copilot models.
const COPILOT_DEFAULT: CopilotModelEntry = CopilotModelEntry {
    prefix: "",
    route: CopilotApiRoute::OpenAiChatCompletions,
    thinking: ThinkingSupport::Ignored(
        "copilot chat-completions route does not expose reasoning.effort",
    ),
};
```

### Rewritten `classify_copilot_route`

```rust
fn classify_copilot_route(model: &str) -> CopilotApiRoute {
    copilot_model_entry(model).route
}

fn copilot_model_entry(model: &str) -> &'static CopilotModelEntry {
    let m = model.to_ascii_lowercase();
    COPILOT_MODELS
        .iter()
        .find(|e| m.starts_with(e.prefix))
        .or_else(|| COPILOT_MODELS_CONTAINS.iter().find(|e| m.contains(e.prefix)))
        .unwrap_or(&COPILOT_DEFAULT)
}
```

### Rewritten `thinking_support_for` (Copilot branch)

```rust
pub fn thinking_support_for(kind: &ProviderKind, model: &str) -> ThinkingSupport {
    match kind {
        ProviderKind::Copilot => copilot_model_entry(model).thinking,
        ProviderKind::Codex   => ThinkingSupport::Applied,
        ProviderKind::Gemini  => ThinkingSupport::Applied,
        ProviderKind::OpenAi  => ThinkingSupport::Ignored(
            "openai chat-completions provider does not map thinking yet",
        ),
        ProviderKind::Ollama  => ThinkingSupport::Ignored(
            "ollama provider does not support mapped thinking levels",
        ),
    }
}
```

The `thinking_support_for` function no longer needs its own `match
classify_copilot_route(model)` arm — it reads directly from the table entry.

### Alternative: merging prefix/contains into one table

If the distinction between prefix-match and contains-match entries is
considered noise, a small `MatchKind` enum can unify the two:

```rust
enum MatchKind { Prefix, Contains }

struct CopilotModelEntry {
    pattern: &'static str,
    match_kind: MatchKind,
    route: CopilotApiRoute,
    thinking: ThinkingSupport,
}
```

This is the preferred approach if the contains-match set grows beyond one entry.
The primary design above keeps it simple for now.

## Affected files

| File | Change |
|------|--------|
| `src/provider.rs` | Add `CopilotModelEntry`, `COPILOT_MODELS`, `copilot_model_entry()`; rewrite the two functions |

No other files change.

## Tests

### Preserve existing tests

All tests in `src/provider.rs` under `mod tests` must continue to pass
unchanged — they are the correctness guard for this refactor.

### Add table-coverage tests

Add one test per static table entry to verify the table lookup agrees with the
documented intent. These tests double as documentation:

```rust
// --- prefix-matched entries ---

#[test]
fn copilot_entry_claude_routes_to_anthropic() {
    let entry = copilot_model_entry("claude-sonnet-4.5");
    assert_eq!(entry.route, CopilotApiRoute::AnthropicMessages);
    assert!(matches!(entry.thinking, ThinkingSupport::Ignored(_)));
}

#[test]
fn copilot_entry_gpt5_routes_to_responses() {
    let entry = copilot_model_entry("gpt-5.3-turbo");
    assert_eq!(entry.route, CopilotApiRoute::OpenAiResponses);
    assert_eq!(entry.thinking, ThinkingSupport::Applied);
}

// --- contains-matched entries ---

#[test]
fn copilot_entry_codex_in_name_routes_to_responses() {
    let entry = copilot_model_entry("gpt-5.3-codex");
    assert_eq!(entry.route, CopilotApiRoute::OpenAiResponses);
    assert_eq!(entry.thinking, ThinkingSupport::Applied);
}

// --- default ---

#[test]
fn copilot_entry_unknown_model_uses_default_route() {
    let entry = copilot_model_entry("some-future-model");
    assert_eq!(entry.route, CopilotApiRoute::OpenAiChatCompletions);
    assert!(matches!(entry.thinking, ThinkingSupport::Ignored(_)));
}

// --- case-insensitivity ---

#[test]
fn copilot_entry_lookup_is_case_insensitive() {
    assert_eq!(
        copilot_model_entry("CLAUDE-3").route,
        copilot_model_entry("claude-3").route,
    );
}
```

### Add a consistency assertion (optional, compile-time-ish)

A single test that iterates over all entries and asserts that no two entries
with the same `MatchKind` have one prefix that is a prefix of another (which
would make the second entry unreachable). This prevents accidental shadowing
when entries are added:

```rust
#[test]
fn copilot_model_table_has_no_shadowed_prefix_entries() {
    for (i, a) in COPILOT_MODELS.iter().enumerate() {
        for b in COPILOT_MODELS.iter().skip(i + 1) {
            assert!(
                !a.prefix.starts_with(b.prefix) && !b.prefix.starts_with(a.prefix),
                "table entries '{}' and '{}' shadow each other",
                a.prefix, b.prefix
            );
        }
    }
}
```

## Implementation steps

1. Add `CopilotModelEntry` struct and the two static tables.
2. Add `copilot_model_entry()` helper.
3. Replace body of `classify_copilot_route()` with `copilot_model_entry(model).route`.
4. Replace Copilot branch of `thinking_support_for()` with `copilot_model_entry(model).thinking`.
5. Run `cargo test` — existing tests must pass without modification.
6. Add new table-coverage tests.
7. Run quality gates.

## Verification checklist

1. `cargo fmt`
2. `cargo clippy --all-targets`
3. `cargo test` — all existing provider tests pass; new coverage tests added
4. No `if m.starts_with` / `if m.contains` chains remain in the Copilot routing
   code path

## Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Table entry order introduces a behaviour difference for a model that matches two prefixes | Low | The shadowing test catches overlapping entries; existing tests guard current behaviour |
| `CopilotApiRoute` or `ThinkingSupport` gains non-`Copy` fields later | Very low | Both are currently `Copy`; table entries are `'static` — a future change would require moving entries to `OnceCell` or similar |
