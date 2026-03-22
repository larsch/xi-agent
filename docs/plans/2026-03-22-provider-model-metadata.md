# Copilot model metadata from the REST API

**Date:** 2026-03-22  
**Status:** Done  
**Priority:** Medium  
**Risk:** Low-Medium — adds a cache + new deserialization; existing heuristics stay as fallback  
**Source:** TAU-REVIEW.md §7 (Provider Routing Logic Duplication) and §8 (Thinking Level Mapping)

---

## Problem

`src/provider.rs` contains three independent functions that each do their own
model-name prefix matching to answer questions about a Copilot model:

1. `classify_copilot_route()` — which API endpoint to use
2. `thinking_support_for()` — does this route expose reasoning effort?
3. `context_window_for_model()` — 50-line chain of `starts_with` / `contains` guards

All three are based on hard-coded guesses about model names.  New Copilot
models require updates in multiple places, and the guesses can become stale.

The Copilot `/models` REST endpoint (already called by `CopilotProvider::list_models()`)
returns richer metadata that includes:

```json
{
  "id": "claude-3.7-sonnet",
  "vendor": "Anthropic",
  "capabilities": {
    "limits": {
      "max_context_window_tokens": 200000,
      "max_output_tokens": 64000
    },
    "supports": { "streaming": true, "tool_calls": true }
  }
}
```

Key fields available in the response:

| Field | Replaces |
|---|---|
| `vendor` | `m.starts_with("claude")` → Anthropic routing heuristic |
| `capabilities.limits.max_context_window_tokens` | Hard-coded Copilot entries in `context_window_for_model()` |

The Chat Completions vs. Responses API distinction (for OpenAI-vendor models)
is **not** encoded in any known public field; name heuristics are retained for
that split.

## Goals

1. Parse `vendor` and `capabilities.limits.max_context_window_tokens` from the
   Copilot `/models` response and cache them in memory.
2. Use the cached `vendor` field to drive Anthropic vs. OpenAI routing, replacing
   the `starts_with("claude")` name heuristic as the primary path.
3. Use the cached context-window value in `context_window_for_model()` for
   Copilot models, so the display stays accurate when new models appear without
   a code change.
4. Keep existing name heuristics as a cold-start fallback (cache empty on first
   request before `list_models()` has been called).
5. No breaking changes to any public function signatures or the `LlmProvider` trait.

## Non-goals

- Replacing name heuristics for the OpenAI Chat Completions / Responses split
  (no public API field distinguishes these today).
- Driving `thinking_support_for()` directly from the API (it continues to derive
  from the route, which is correct).
- Migrating non-Copilot providers to the same cache (Ollama, Gemini, etc. have
  their own metadata mechanisms).
- Persisting the cache across process restarts (in-memory only).

## Design

### Cache structure in `src/llm/copilot.rs`

```rust
/// Metadata fetched from the Copilot `/models` endpoint for a single model.
#[derive(Debug, Clone)]
pub struct CopilotModelMeta {
    /// Provider vendor string as returned by the API, e.g. "Anthropic" or
    /// "Azure OpenAI".  Empty string when not present in the response.
    pub vendor: String,
    /// Maximum context-window size in tokens, when reported by the API.
    pub max_context_window_tokens: Option<usize>,
}

/// Process-global cache populated by `list_models()`.
/// Keyed by the exact model `id` returned by the API (case-preserved).
static COPILOT_MODEL_CACHE: OnceLock<RwLock<HashMap<String, CopilotModelMeta>>> =
    OnceLock::new();

fn cache() -> &'static RwLock<HashMap<String, CopilotModelMeta>> {
    COPILOT_MODEL_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}
```

### New deserialization structs (private to `copilot.rs`)

```rust
#[derive(Deserialize)]
struct ApiModelsResponse {
    data: Vec<ApiModelEntry>,
}

#[derive(Deserialize)]
struct ApiModelEntry {
    id: String,
    #[serde(default)]
    vendor: String,
    #[serde(default)]
    capabilities: ApiModelCapabilities,
}

#[derive(Deserialize, Default)]
struct ApiModelCapabilities {
    #[serde(default)]
    limits: ApiModelLimits,
}

#[derive(Deserialize, Default)]
struct ApiModelLimits {
    max_context_window_tokens: Option<usize>,
}
```

### `fetch_and_cache_models()` helper (private)

Replaces the forwarding call to `self.models_provider.list_models()` with a
direct HTTP request that parses full metadata:

```rust
async fn fetch_and_cache_models(
    base_url: String,
    access_token: String,
    extra_headers: Vec<(String, String)>,
) -> Result<Vec<String>, String> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    // ... HTTP request (mirrors existing openai list_models logic) ...
    let parsed: ApiModelsResponse = response.json().await?;

    // Populate cache
    if let Ok(mut map) = cache().write() {
        for entry in &parsed.data {
            map.insert(entry.id.clone(), CopilotModelMeta {
                vendor: entry.vendor.clone(),
                max_context_window_tokens: entry.capabilities.limits.max_context_window_tokens,
            });
        }
    }

    let mut ids: Vec<String> = parsed.data.into_iter().map(|e| e.id).collect();
    ids.sort();
    Ok(ids)
}
```

### Updated `CopilotProvider::list_models()`

```rust
fn list_models(&self) -> ModelListFuture {
    let base_url = self.base_url.clone();       // new field, see below
    let token    = self.access_token.clone();   // new field
    let headers  = copilot_extra_headers();
    Box::pin(fetch_and_cache_models(base_url, token, headers))
}
```

`CopilotProvider` gains two new private fields (`base_url: String`,
`access_token: String`) so the async future can be constructed without
capturing `self`.  The existing `models_provider: OpenAiProvider` field
is removed.

### Public cache-query helpers (on `CopilotProvider`)

```rust
impl CopilotProvider {
    /// Look up the vendor string for `model` from the cache.
    /// Returns `None` when the cache is unpopulated or the model is absent.
    pub fn cached_vendor(model: &str) -> Option<String> {
        cache().read().ok()?.get(model).map(|m| m.vendor.clone())
    }

    /// Look up the context-window token limit for `model` from the cache.
    pub fn cached_context_window(model: &str) -> Option<usize> {
        cache().read().ok()?.get(model)?.max_context_window_tokens
    }
}
```

### Updated `classify_copilot_route()` in `src/provider.rs`

Cache is checked first; name heuristics serve as cold-start fallback:

```rust
fn classify_copilot_route(model: &str) -> CopilotApiRoute {
    // Primary: use vendor from API metadata if available.
    if let Some(vendor) = CopilotProvider::cached_vendor(model) {
        if vendor.eq_ignore_ascii_case("anthropic") {
            return CopilotApiRoute::AnthropicMessages;
        }
        // For OpenAI-vendor models, fall through to name heuristics below
        // (the Responses vs. Chat-Completions split is not in the API).
    }
    // Fallback / OpenAI sub-routing: name heuristics.
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") {
        CopilotApiRoute::AnthropicMessages
    } else if m.contains("codex") || m.starts_with("gpt-5") {
        CopilotApiRoute::OpenAiResponses
    } else {
        CopilotApiRoute::OpenAiChatCompletions
    }
}
```

### Updated `context_window_for_model()` in `src/provider.rs`

```rust
pub fn context_window_for_model(model: &str) -> Option<usize> {
    // Primary: Copilot API metadata (accurate, stays up to date).
    if let Some(cw) = CopilotProvider::cached_context_window(model) {
        return Some(cw);
    }
    // Fallback: hard-coded table for all other providers (Gemini, Ollama, etc.)
    // and Copilot models used before list_models() has been called.
    let m = model.to_ascii_lowercase();
    // ... (existing if-chain, unchanged) ...
}
```

## Affected files

| File | Change |
|---|---|
| `src/llm/copilot.rs` | Add cache statics and types; add `fetch_and_cache_models()`; expose `cached_vendor()` / `cached_context_window()`; update `list_models()` and `new()`; add `base_url`/`access_token` fields; remove `models_provider` field |
| `src/provider.rs` | Update `classify_copilot_route()` to check cache first; update `context_window_for_model()` to check cache first |

No other files change.

## Tests

### Preserve existing tests

All tests in `src/provider.rs` under `mod tests` must continue to pass unchanged
— they exercise the fallback heuristic path, which is still present.

### New unit tests in `src/llm/copilot.rs`

```rust
#[test]
fn cached_vendor_returns_none_for_unknown_model() {
    assert!(CopilotProvider::cached_vendor("unknown-model-xyz").is_none());
}

#[test]
fn cached_context_window_returns_none_for_unknown_model() {
    assert!(CopilotProvider::cached_context_window("unknown-model-xyz").is_none());
}

#[test]
fn cache_round_trips_vendor_and_context_window() {
    // Directly insert into the cache to simulate a list_models() call.
    cache().write().unwrap().insert(
        "test-anthropic-model".to_string(),
        CopilotModelMeta {
            vendor: "Anthropic".to_string(),
            max_context_window_tokens: Some(200_000),
        },
    );
    assert_eq!(
        CopilotProvider::cached_vendor("test-anthropic-model").as_deref(),
        Some("Anthropic")
    );
    assert_eq!(
        CopilotProvider::cached_context_window("test-anthropic-model"),
        Some(200_000)
    );
}
```

### New unit tests in `src/provider.rs`

```rust
#[test]
fn classify_copilot_route_uses_cached_vendor_for_anthropic() {
    // Pre-populate cache with a synthetic model whose name gives no hint.
    crate::llm::copilot::test_helpers::insert_cache(
        "future-ai-model",
        "Anthropic",
        Some(200_000),
    );
    assert_eq!(
        classify_copilot_route("future-ai-model"),
        CopilotApiRoute::AnthropicMessages
    );
}

#[test]
fn context_window_prefers_cache_over_hard_coded_table() {
    crate::llm::copilot::test_helpers::insert_cache("gpt-4o", "Azure OpenAI", Some(999_999));
    // Cache wins over the hard-coded 128_000 for gpt-4o.
    assert_eq!(context_window_for_model("gpt-4o"), Some(999_999));
}
```

A `#[cfg(test)] pub mod test_helpers` block in `copilot.rs` exposes
`insert_cache()` so the provider-level tests can seed the global cache
without duplicating the locking boilerplate.

## Implementation steps

1. Add `CopilotModelMeta`, cache statics, `cache()` accessor, and
   deserialization structs to `copilot.rs`.
2. Add `fetch_and_cache_models()` async function.
3. Add `cached_vendor()` and `cached_context_window()` static methods to
   `CopilotProvider`.
4. Add `base_url` and `access_token` fields to `CopilotProvider`; remove
   `models_provider` field; update `new()` and `list_models()` accordingly.
5. Update `classify_copilot_route()` in `provider.rs` to check cache first.
6. Update `context_window_for_model()` in `provider.rs` to check cache first.
7. Run `cargo test` — existing tests must pass without modification.
8. Add new cache unit tests.
9. Run quality gates.

## Verification checklist

1. `cargo fmt`
2. `cargo clippy --all-targets`
3. `cargo test` — all existing provider + copilot tests pass; new tests added
4. With a live Copilot token: `list_models()` populates the cache; a model
   selected from the picker routes via `cached_vendor()` rather than name
   heuristics (visible in debug log: `copilot transport resolved: …`)
5. Context-window display in the info bar shows the API-reported value for a
   model fetched via `list_models()`

## Risks and mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Copilot API response omits `vendor` or `capabilities.limits` for some models | Low–Medium | All fields use `#[serde(default)]`; missing fields leave the cache entry with `None` / empty string, and fallback heuristics take over |
| Global cache causes test pollution across parallel test runs | Medium | Cache is write-once per key (new inserts overwrite); `test_helpers::insert_cache` is only available in `#[cfg(test)]`; tests using it should be in a single-threaded test or use unique model names |
| `OnceLock<RwLock<…>>` read contention | Very low | CLI tool with one active task at a time; contention is negligible |
| `base_url` / `access_token` fields increase `CopilotProvider` size | Negligible | Both are small heap strings; `CopilotProvider` is always boxed behind `Arc<dyn LlmProvider>` |
