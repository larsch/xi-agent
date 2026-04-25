# Plan: App decomposition — issue #44

Date: 2026-04-24  
Status: **INCOMPLETE** — data extraction done; methods were not moved; app.rs target (~3,000 lines) not reached. Issue #44 reopened.  
Continued in: `2026-04-25-app-behaviour-extraction.md`  
Issue: https://gitea.belunktum.dk/larsch/tau/issues/44

---

## Context

A structural review (2026-04-24) identified that the provider setup domain
carries specialised per-preset branching that should be generalised before the
`ProviderManager` sub-struct is extracted.  Extracting bad abstractions into a
new home makes them harder to fix later.

Some sub-structs have already been extracted (see issue comment):
`CompletionState`, `SelectionState`, `LoginState`, `AskUserState`,
`AgentRuntime`.  app.rs is down to ~4,503 lines.

---

## Scope

### Phase 1 — Generalise provider setup abstractions (preliminary work)

Fix the three abstraction problems in the provider setup flow so that the
subsequent `ProviderManager` extraction is clean.

#### 1a. Add `url_normalization` to `BackendPresetDef`

**Problem:** `submit_pending_provider_base_url` (app.rs) and the two
normalizer methods (`normalize_ollama_endpoint`, `normalize_open_webui_url`)
branch on `BackendPreset` variant names.  The actual difference is semantic:
default scheme (`http` vs `https`) and default port (11434 vs none).

**Fix:** Add to `BackendPresetDef`:
```rust
pub url_normalization: Option<UrlNormalization>,

pub struct UrlNormalization {
    pub default_scheme: &'static str,  // "http" or "https"
    pub default_port: Option<u16>,
}
```
Populate it in `BACKEND_PRESET_CATALOG` for each preset that accepts a
user-supplied URL.  Replace both normalizer methods with a single
`normalize_endpoint_url(raw: &str, norm: &UrlNormalization) -> Option<String>`
free function.  `submit_pending_provider_base_url` uses
`instance.backend_preset.def().url_normalization` — no more preset-name match.

Also add `endpoint_label: &'static str` and `endpoint_hint: &'static str` to
`BackendPresetDef` to replace the `match instance.backend_preset { Ollama =>
"ollama URL: ", ... }` chains in `SetupInputKind::prompt_label` /
`prompt_hint`.

**Affected files:** `src/provider_instance.rs`, `src/app.rs`

#### 1b. Replace boolean setup flags with a typed `ProviderSetupStep`

**Problem:** App carries five fields to track the provider setup state machine:
```rust
pub setup_input_mode: Option<SetupInputKind>,
pub ollama_endpoint_input_mode: bool,
pub open_webui_url_input_mode: bool,
pub open_webui_token_input_mode: bool,
pub open_webui_pending_url: Option<String>,
```
`ollama_endpoint_input_mode` and `open_webui_url_input_mode` are both
`SetupInputKind::BaseUrl` for different presets — they carry no extra
information.  The two-step URL→token flow for OpenWebUI is not special; any
preset with `UserSupplied` endpoint AND `ApiKey` auth should use the same flow.

**Fix:** Replace all five with a single enum:
```rust
pub(crate) enum ProviderSetupStep {
    Idle,
    Endpoint,                        // SetupInputKind::BaseUrl (generic)
    ApiKey { pending_url: Option<String> },  // SetupInputKind::ApiKey, holds
                                             // URL from prior Endpoint step
    Name,                            // SetupInputKind::Name
}
```
`App` holds `pub(crate) provider_setup_step: ProviderSetupStep`.

Entry methods (`enter_ollama_endpoint_freeform_mode`,
`enter_open_webui_url_input_mode`, `enter_provider_base_url_input_mode`)
collapse into one `enter_provider_endpoint_input(instance)` that sets
`ProviderSetupStep::Endpoint` and pre-fills the textarea using
`url_normalization.default_endpoint_hint()` from the catalog.

`handle_chat_submit` in `main.rs` becomes a single `match
app.provider_setup_step { ... }` instead of five chained `if` blocks.

**Affected files:** `src/app.rs`, `src/main.rs`

#### 1c. Merge `ChangeOllamaEndpoint` / `ChangeOpenWebUi` into one `RunResult` variant

**Problem:** Two `RunResult` variants carry the same intent:
```rust
ChangeOllamaEndpoint { instance, url },
ChangeOpenWebUi { instance, url, api_key },
```
The difference (api_key presence) is already modelled by `auth_mode`.

**Fix:**
```rust
ConfigureProvider {
    instance: ProviderInstance,
    url: Option<String>,
    api_key: Option<String>,
},
```
The outer loop handler applies `url` and `api_key` generically (skip if
`None`), saves config, and rebuilds the provider.

**Affected files:** `src/main.rs`

---

### Phase 2 — Extract `ProviderManager`

After Phase 1 the provider-related fields and methods in `App` are clean.
Extract them into `src/provider_manager.rs`:

**Fields to move from `App`:**
- `provider_instances: Vec<ProviderInstance>`
- `current_model: String`
- `current_provider: String`
- `current_thinking: ThinkingLevel`
- `thinking_supported: bool`
- `provider_setup_step: ProviderSetupStep`  (from Phase 1)
- `pending_provider_setup: Option<PendingProviderSetup>`
- `pending_provider_removal: Option<PendingProviderRemoval>`

**Methods to move / delegate:**
- All `enter_provider_*`, `submit_pending_provider_*`,
  `finish_pending_provider_setup`, `clear_pending_provider_*`,
  `pending_provider_instance`, `pending_provider_setup_is_edit` etc.
- `record_model_changed`, `record_thinking_level_changed`

App holds `pub(crate) provider: ProviderManager` and delegates.

Also migrate the loop-local variables `current_instance`, `current_model`,
`current_thinking` in `main()` to read from / write through
`app.provider.current_instance()` etc., eliminating the dual-state sync.

**Affected files:** `src/app.rs`, `src/main.rs`, new `src/provider_manager.rs`

---

### Phase 3 — Extract `SessionManager`

Move session persistence fields into `src/session_manager.rs`:
- `session_store`, `current_session_id`, `current_cwd`,
  `resume_available_for_cwd`, `session_state`, `live_turn`,
  `pending_turn_events`

Methods: `init_session_persistence`, `current_cwd`, `should_show_resume_hint`,
`resume_session`, `new_conversation` (session-management parts), etc.

**Affected files:** `src/app.rs`, new `src/session_manager.rs`

---

### Phase 4 — Reduce pub field visibility on App

Once sub-structs are stable, make all remaining `App` fields `pub(crate)` or
private.  Fix any external call sites (primarily `src/ui/` submodules and
`src/main.rs`).

---

## Out of scope

- Moving key-handler functions to `src/input.rs` (separate issue).
- Introducing a `UiSnapshot` view-model for `ui::draw` (separate issue).
- `SelectionKind` typed enum on `SelectionState` (minor, can be a follow-up).

---

## Assumptions

- Phase 1 is purely a refactor: identical runtime behaviour, no flag changes.
- The `ProviderSetupStep` enum can express all current flows; no flow is lost.
- `UrlNormalization` covers Ollama (http + port 11434) and generic HTTP(S)
  services (https + no default port) — two variants suffices for current
  presets.

---

## Risks

- Phase 2 dual-state consolidation in `main.rs` is the riskiest step: the
  loop-local variables `current_instance` / `current_model` / `current_thinking`
  are read and written in ~109 places.  Do incrementally; compile-check after
  each RunResult arm is migrated.
- Phase 1b touches the textarea reset / init path which has subtlety (pre-fill
  logic, edit vs new setup).  Existing tests cover this; run them after each
  method rename.

---

## Verification

- `just preflight` passes after each phase.
- No behavioral change: provider setup flows (add Ollama, add OpenWebUI, edit
  provider, remove provider) work identically.
- `app.rs` line count reduces by ≥ 400 lines after Phase 2.
- `main.rs` RunResult arms reduce from 10+ to ~7 after Phase 1c.

---

## Ordered steps

1. **1a** — Add `UrlNormalization` + `endpoint_label`/`endpoint_hint` to
   `BackendPresetDef`; write generic normalizer; remove preset-name matches.
2. **1b** — Introduce `ProviderSetupStep`; collapse boolean flags; unify entry
   methods; simplify `handle_chat_submit`.
3. **1c** — Merge `ChangeOllamaEndpoint` + `ChangeOpenWebUi` into
   `ConfigureProvider`.
4. **Preflight check** after Phase 1.
5. **Phase 2** — Extract `ProviderManager`; consolidate dual state in `main.rs`.
6. **Preflight check** after Phase 2.
7. **Phase 3** — Extract `SessionManager`.
8. **Preflight check** after Phase 3.
9. **Phase 4** — Reduce visibility.
10. **Final preflight** + close issue #44.
