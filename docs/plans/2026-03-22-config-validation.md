# Config startup validation warnings

**Date:** 2026-03-22  
**Status:** Planned  
**Priority:** Medium  
**Risk:** Low — additive only; no existing behaviour changes  
**Source:** TAU-REVIEW.md §10 — Config Loading Lacks Validation

---

## Problem

`TauConfig` is loaded without any validation. Silent misconfigurations surface
only at the moment the user first sends a message:

```
User types message → submit → build_provider() → Err("Missing API key. Configure...")
→ shown as an assistant message
```

For providers that require a `config.toml` key (currently: OpenAI), this means
the user must first trigger an error to learn what is wrong. The error path
through `build_provider` already produces a good message, but it is delayed.

Additionally, unknown provider names in `config.toml` are silently treated as
the default (Copilot) with no indication that the name was not recognised.

## Goals

1. Add a `TauConfig::warnings() -> Vec<String>` method that returns
   human-readable warnings about obvious misconfigurations.
2. Display those warnings as assistant messages at startup, before the first
   user message.
3. Keep warnings non-fatal — tau should still start, not refuse to open.
4. Keep `config.rs` free of dependencies on `provider.rs` (no circular imports).

## Non-goals

- Network-based validation (checking OAuth tokens are valid, etc.).
- Hard failures on misconfiguration (`warnings`, not `errors`).
- Warning about every possible misconfiguration — only obvious, detectable cases.

## Design

### `TauConfig::warnings()` in `src/config.rs`

The method does pure structural checks with no external I/O:

```rust
impl TauConfig {
    /// Return a list of human-readable warnings about obvious configuration
    /// problems.  Does not perform any I/O or validate OAuth credentials.
    pub fn warnings(&self) -> Vec<String> {
        let mut ws: Vec<String> = Vec::new();

        // Known provider names (duplicated deliberately to avoid a circular
        // dependency on provider.rs).
        const KNOWN_PROVIDERS: &[&str] =
            &["copilot", "openai", "codex", "gemini", "ollama"];

        if let Some(ref p) = self.provider {
            if !KNOWN_PROVIDERS.contains(&p.as_str()) {
                ws.push(format!(
                    "Unknown provider '{p}' in config.toml \
                     (known: copilot, openai, codex, gemini, ollama)"
                ));
            }
            // OpenAI requires a config-file API key; surface the problem early.
            if p == "openai" && self.openai.api_key.is_none() {
                ws.push(
                    "Provider 'openai' is selected but [openai].api_key is not \
                     set in config.toml — requests will fail until it is added."
                        .to_string(),
                );
            }
        }

        ws
    }
}
```

### Display in `src/main.rs`

After `TauConfig::load()` succeeds, display any warnings before entering the
event loop:

```rust
for warning in config.warnings() {
    app.messages.push(Message::assistant(format!("[config warning: {warning}]")));
}
```

This must happen after `App::new()` and before `run()`.

## Affected files

| File | Change |
|------|--------|
| `src/config.rs` | Add `warnings()` method + unit tests |
| `src/main.rs` | Call `config.warnings()` and push messages after app init |

## Tests

All tests live in `src/config.rs` in the existing `mod tests` block.

```rust
#[test]
fn warnings_empty_for_default_config() {
    let cfg = TauConfig::default();
    assert!(cfg.warnings().is_empty());
}

#[test]
fn warnings_empty_for_known_provider_with_no_extra_requirements() {
    let cfg = TauConfig {
        provider: Some("copilot".into()),
        ..TauConfig::default()
    };
    assert!(cfg.warnings().is_empty());
}

#[test]
fn warnings_flags_unknown_provider() {
    let cfg = TauConfig {
        provider: Some("anthropic".into()),
        ..TauConfig::default()
    };
    let ws = cfg.warnings();
    assert_eq!(ws.len(), 1);
    assert!(ws[0].contains("Unknown provider 'anthropic'"));
}

#[test]
fn warnings_flags_openai_without_api_key() {
    let cfg = TauConfig {
        provider: Some("openai".into()),
        ..TauConfig::default()
    };
    let ws = cfg.warnings();
    assert_eq!(ws.len(), 1);
    assert!(ws[0].contains("api_key"));
}

#[test]
fn warnings_silent_for_openai_with_api_key() {
    let cfg = TauConfig {
        provider: Some("openai".into()),
        openai: crate::config::OpenAiConfig {
            api_key: Some("sk-test".into()),
            ..Default::default()
        },
        ..TauConfig::default()
    };
    assert!(cfg.warnings().is_empty());
}
```

## Verification checklist

1. `cargo fmt`
2. `cargo clippy --all-targets`
3. `cargo test` — all existing tests pass, new tests pass
4. Manual smoke test: set `provider = "typo"` in config.toml, verify warning
   appears at startup before any user input

## Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `KNOWN_PROVIDERS` list drifts out of sync with `ProviderKind` | Low | Add a comment cross-referencing `src/provider.rs`; the compiler won't catch it, but code review will |
| Warning message shown as assistant message looks strange | Low | Style the message consistently with other `[...]`-prefixed messages already in use |
