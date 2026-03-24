# Proactive Auth Refresh + Typed Unauthorized Errors Plan

**Date:** 2026-03-24  
**Status:** Done  
**Priority:** High

## Chosen direction

Implement a two-layer auth-renewal strategy with shared logic:

1. **Proactive preflight refresh** using known credential `expires_at` before requests.
2. **Reactive refresh** on typed `Unauthorized` provider errors (not string matching).

Also refactor request failure handling for code reuse across chat requests and model-list requests.

## Problem

Current renewal behavior is brittle because:

- request/model-fetch failures are propagated as `String`
- auth failures are inferred via substring checks (`"401"` heuristics)
- refresh trigger logic is duplicated in app request paths

This can miss valid unauthorized states and causes maintenance drift.

## Scope

### In scope

- Add typed provider error representation in `llm` layer.
- Emit typed unauthorized errors from shared HTTP helper and model-list calls.
- Add auth preflight based on `expires_at` with a safety window.
- Centralize refresh trigger/retry bookkeeping in `App` for reuse.
- Apply shared logic to:
  - agent request path (submit/retry)
  - `/model` model fetch path

### Out of scope

- Broader status-specific UX beyond auth handling (e.g. dedicated 429 flow).
- Keyring migration / auth storage redesign.
- Changing provider auth protocols.

## Design outline

### 1) Introduce typed provider errors

Add `src/llm/error.rs` (re-exported from `src/llm/mod.rs`):

- `ProviderErrorKind`: `Unauthorized`, `Forbidden`, `RateLimited`, `ServerError`, `Network`, `Other`
- `ProviderError { kind, status_code, provider, message }`

Update signatures:

- `LlmEvent::Error(String)` -> `LlmEvent::Error(ProviderError)`
- `ModelListFuture` error type from `String` -> `ProviderError`

Provide helpers for compatibility:

- `impl Display for ProviderError`
- constructors like `ProviderError::unauthorized(...)`

### 2) Centralize HTTP->ProviderError mapping

In `src/llm/common.rs`:

- add reusable function to map reqwest/network/HTTP status failures to `ProviderError`
- 401 → `ProviderErrorKind::Unauthorized` (triggers refresh)
- 403 → `ProviderErrorKind::Forbidden` (does **not** trigger refresh — means "authenticated but not allowed")
- 429 → `ProviderErrorKind::RateLimited` (no special handling yet, but avoids future signature migration)
- 5xx → `ProviderErrorKind::ServerError`
- network/connection errors → `ProviderErrorKind::Network`
- keep existing response-body preview logging

Use this helper from streaming request setup and provider `list_models` HTTP calls.

### 3) Add auth token preflight from expiry

In `src/auth/mod.rs` (or `src/auth/store.rs` + thin facade):

- add `AuthTokenState` enum: `Missing | Valid | ExpiringSoon | Expired`
- add function `token_state(provider: &str, now_secs: i64, leeway_secs: i64) -> anyhow::Result<AuthTokenState>`
- derive from stored credentials `expires_at`

Define a named constant: `const AUTH_REFRESH_LEEWAY_SECS: i64 = 120;` in `src/auth/mod.rs`.

### 4) Refactor app refresh/retry logic for reuse

In `src/app.rs`:

- add `RetryTarget` enum (`AgentTurn`, `ModelFetch`)
- add a single helper:
  - checks refresh support/provider
  - checks `refresh_in_progress`
  - toggles retry flags by target
  - spawns `auth::refresh_provider`

Replace duplicated logic in:

- `apply_event` (reactive unauthorized)
- `apply_model_list` (reactive unauthorized)
- request start points (proactive preflight)

### 5) Proactive refresh integration points

Before outbound operations:

- `submit` / `submit_with_text` / `retry_last_request`
- `start_model_fetch`

Guard: only trigger when `!self.streaming && !self.refresh_in_progress`. If the user
is already streaming, skip the preflight — the reactive path will catch it if needed.

Behavior:

- if token is `Expired` or `ExpiringSoon` and no refresh in progress:
  - trigger shared refresh helper with correct `RetryTarget`
  - defer request; existing rebuild + retry flags resume automatically

Budget interaction: `auth_retry_budget` is set *after* preflight resolution, not
before. Preflight refresh does not consume the retry budget. The budget is set when
the actual LLM request starts, so that if preflight fails and the request also gets
a 401, one reactive retry is still available.

### 6) Preserve reactive fallback

Keep unauthorized-triggered refresh as fallback for:

- clock skew
- stale local expiry
- server-side revocation

## Ordered implementation steps

1. **Type plumbing**
   - Add `ProviderErrorKind` + `ProviderError` in new `llm/error.rs`; re-export from `llm/mod.rs`.
   - Update `LlmEvent` and `ModelListFuture` type aliases.
2. **Common mapping**
   - Add status/network mapping helpers in `llm/common.rs`.
   - Update `send_streaming_request` to return typed errors.
3. **Provider updates**
   - Update each provider module to compile against typed errors.
   - Switch model-list HTTP error handling to typed errors.
4. **Auth preflight API**
   - Implement token-state helper using `expires_at` and leeway.
5. **App refactor**
   - Add shared `RetryTarget` helper for refresh orchestration.
   - Replace duplicated refresh trigger blocks.
6. **Proactive checks**
   - Add preflight at request/model-fetch initiation points.
7. **Tests + polish**
   - Add/adjust unit tests for typed unauthorized mapping and preflight logic.
   - Run fmt, clippy, test.

## Affected files (expected)

- `src/llm/error.rs` **(new)**
- `src/llm/mod.rs`
- `src/llm/common.rs`
- `src/llm/copilot.rs`
- `src/llm/openai.rs`
- `src/llm/ollama.rs` (type updates)
- `src/llm/codex.rs` (type updates)
- `src/llm/anthropic.rs` (type updates)
- `src/llm/gemini.rs` (type updates)
- `src/auth/mod.rs` and/or `src/auth/store.rs`
- `src/app.rs` (including `models_tx`/`models_rx` channel type: `Result<Vec<String>, String>` → `Result<Vec<String>, ProviderError>`, and `apply_model_list` signature)
- `src/agent/mod.rs` (matches on `LlmEvent::Error`; extract display string at the `AgentEvent::Error(String)` boundary)
- `src/agent/types.rs` (`AgentEvent::Error` — keep as `String` for now, convert via `Display` at the agent→app boundary)
- `src/agent/tests.rs` (update `LlmEvent::Error` construction in mocks)
- `src/main.rs` (match arms may need type adjustments)
- tests in touched modules

## Assumptions

- `expires_at` is stored as Unix epoch **seconds** for all supported auth providers.
  (Test values like `9_999_999_999` ≈ year 2286 confirm seconds, not milliseconds.
  Standard OAuth token responses also use seconds.)
- Refresh endpoints continue to work without additional user interaction.
- Rebuild-provider loop in `main.rs` remains the mechanism to apply refreshed creds.

## Risks and mitigations

- **Risk:** Broad signature changes across providers cause regressions.  
  **Mitigation:** Introduce typed error constructors + `Display`; migrate incrementally and compile frequently.

- **Risk:** Over-eager preflight refresh creates unnecessary refresh churn.  
  **Mitigation:** Use conservative leeway (120s) and guard with `refresh_in_progress`.
  Only trigger preflight when `!self.streaming && !self.refresh_in_progress`.

- **Risk:** Preflight refresh completes and triggers `login_needs_rebuild` while
  a request is already in flight from a concurrent submit.  
  **Mitigation:** Skip preflight when `self.streaming` is true; the reactive path
  handles auth failures for in-flight requests.

- **Risk:** Retry flags race between model-fetch and chat-turn paths.  
  **Mitigation:** Centralize flag mutation in one helper with explicit `RetryTarget`.

## Success criteria

- No substring matching for 401/unauthorized in app auth-retry logic.
- Unauthorized from providers is represented as typed error kind.
- 403 Forbidden does **not** trigger token refresh.
- Preflight expiry check triggers refresh before likely-expired requests.
- Shared refresh helper handles both request classes.
- `auth_retry_budget` remains available for reactive retry after preflight.
- Existing behavior remains seamless (automatic retry after refresh).

## Verification plan

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- Focused tests:
  - HTTP status 401 maps to `ProviderErrorKind::Unauthorized`
  - HTTP status 403 maps to `ProviderErrorKind::Forbidden` (does **not** trigger refresh)
  - HTTP status 429 maps to `ProviderErrorKind::RateLimited`
  - token-state classification around expiry/leeway boundaries (in seconds)
  - app refresh helper sets expected retry target flags
  - preflight does not consume `auth_retry_budget`
