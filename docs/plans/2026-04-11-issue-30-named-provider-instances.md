# Plan — Issue #30: Named provider instances

## Direction

Replace the flat `ProviderKind`-centric model with **named provider instances**, each carrying a
service type and API type. Keep backward compatibility with existing config. Ship as one branch/PR.

## Scope

### In

- New domain types: `ServiceType`, `ApiType`, `ProviderInstance`
- Static service/API catalog with constraints and defaults
- New config format: `[[providers]]` list, read alongside old per-kind sections
- Config migration: old per-kind sections → synthesized instances at load time; new saves use `[[providers]]`
- `build_provider` and `thinking_support_for` dispatch by `ApiType` (not `ProviderKind`)
- `resolve_provider_kind` / `resolve_model` / `save_model` → instance-aware equivalents
- Provider selection UI: shows named instances, not abstract kind names
- Add-provider flow: name → service type → API type (where user-exposed) → endpoint/auth
- Multiple instances of the same service type (Ollama, Open WebUI)
- `ProviderKind` kept but deprecated internally, removed from public-facing flows once replaced

### Out (first version)

- User-defined arbitrary service types
- Unrestricted service/API cross-combinations
- Exposing API choice for services where tau should manage routing (Copilot)

## Ordered steps

1. **Domain types** (`src/provider.rs` or new `src/provider_instance.rs`)
   - `ServiceType` enum + catalog metadata (`allowed_apis`, `default_api`, `label`, etc.)
   - `ApiType` enum
   - `ProviderInstance` struct (id, name, service_type, api_type, base_url, api_key, model)
   - Static service catalog: one definition per `ServiceType`

2. **Config structures + migration** (`src/config.rs`)
   - Add `providers: Vec<ProviderInstance>` to `TauConfig`
   - On load: if `providers` is empty, synthesize instances from legacy per-kind sections
   - `save()`: write `[[providers]]` list; keep legacy keys for backward-compat reading only
   - Config round-trip tests: old → synthesized, new → parsed, migration idempotent

3. **Provider construction** (`src/provider.rs`)
   - `build_provider_for_instance(instance, thinking, config)` dispatching on `ApiType`
   - `thinking_support_for_instance` dispatching on `ApiType`
   - Keep old `build_provider(kind, ...)` as shim during transition

4. **Model resolution + persistence** (`src/main.rs`)
   - `resolve_model` and `save_model` keyed on instance id instead of `ProviderKind`
   - `resolve_provider_kind` replaced by `resolve_provider_instance`

5. **Provider/model selection UI** (`src/app.rs`)
   - Provider list shows named instances (from `config.providers`)
   - Selecting a provider sets `current_provider` to instance id
   - Model list and thinking support use instance-aware lookups

6. **Add-provider / setup flows** (`src/app.rs`, `src/main.rs`)
   - `/provider` → show named instances
   - Add new provider: prompt name → service type → API type (if exposed) → endpoint/auth
   - Ollama and Open WebUI setup flows updated to create new instances rather than overwrite single slot
   - `/login` flows for Copilot, Codex, Gemini kept working (auth stored separately via `AuthStore`)

7. **Tests + cleanup**
   - Config migration tests
   - Service catalog constraint tests
   - Instance construction tests
   - Remove dead `ProviderKind` paths once no longer needed
   - `just preflight` green

## Affected files

| File | Change |
|------|--------|
| `src/provider.rs` | Add `ServiceType`, `ApiType`, `ProviderInstance`; refactor `build_provider`, `thinking_support_for` |
| `src/config.rs` | Add `providers: Vec<ProviderInstance>`; migration logic |
| `src/main.rs` | `resolve_provider_instance`, `resolve_model`, `save_model`, `ChangeProvider` result |
| `src/app.rs` | Selection UI, add-provider flow, instance-aware state |
| `docs/ARCHITECTURE.md` | Update provider model description |

## Assumptions

- `AuthStore` stays separate — cloud provider credentials (Copilot, Codex, Gemini) don't move into
  `[[providers]]` beyond a reference
- `ProviderKind::Test` survives internally as a hidden service type
- Instance IDs are the user-assigned name (slugified); uniqueness enforced at add time
- Old config without `[[providers]]` is fully supported by migration on load

## Risks

- Wide diff across `app.rs` (2657 lines) and `main.rs` (1477 lines) — risk of conflicts with other
  in-flight work
- Hidden assumptions of "one instance per kind" scattered across `app.rs` selection state
- Thinking-level dispatch must stay correct after routing moves to `ApiType`

## Verification

- `just preflight` (fmt + clippy -D warnings + tests + check)
- Config round-trip: old config loads and migrates; new config saves and reloads correctly
- All existing provider types still work (manual or integration test)
- Provider selection UI shows named instances
- Two Ollama instances can be configured and selected independently

## Status

- [x] Step 1: Domain types
- [x] Step 2: Config structures + migration
- [x] Step 3: Provider construction
- [x] Step 4: Model resolution + persistence
- [x] Step 5: Provider/model selection UI (selection/completions now instance-based)
- [x] Step 6: Add-provider / setup flows
- [x] Step 7: Tests + cleanup for the current checkpoint

## Current completion point

The codebase now has a working provider-instance setup flow:

- `src/provider_instance.rs` defines `ServiceType`, `ApiType`, `ProviderInstance`, and the service catalog.
- `src/config.rs` supports `[[providers]]` and migrates legacy per-provider config into instance entries.
- `src/provider.rs` builds providers from `ProviderInstance` and routes thinking support by instance/API.
- `src/main.rs` resolves the active provider as an instance, persists model/provider selection by instance id, and drives add-provider setup by service type.
- `src/app.rs`, `src/commands.rs`, and `src/ui.rs` now use named provider instances for provider selection and `/provider` completions.
- The add-provider flow now supports:
  - provider name
  - service type
  - API type where user-visible
  - endpoint/base URL setup where needed
  - token/API-key setup where needed
- Existing Ollama/Open WebUI instance selection no longer forces re-setup.
- Setup prompt rendering is standardized through shared setup-input UI state.

Verification completed for this checkpoint:

- `cargo test --all-features`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `just preflight`

## Deferred follow-up work

Accepted for later follow-up rather than this checkpoint:

- edit provider flow
- delete provider flow
- polish multiline token / setup-input textarea behavior
- any further provider-management UX refinement

### Next resume point

If continuing on issue #30, resume with follow-up provider-management UX work and documentation updates:

- update `docs/ARCHITECTURE.md` for the provider-instance user flow
- consider add/edit/delete provider management actions
- refine multiline setup-input behavior
- reconcile any remaining deprecated internal `ProviderKind` paths if desired
