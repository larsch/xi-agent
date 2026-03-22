# Active plans and design notes

This directory intentionally contains only active, decision-relevant planning/design artifacts.

## Current files

- `2026-03-14-context-management.md` — planned context window strategy.
- `2026-03-14-tests.md` — planned test strategy expansion.
- `2026-03-15-provider-auth-redesign-design.md` — auth redesign reference design.
- `2026-03-15-docs-cleanup-plan.md` — documentation cleanup/reconciliation plan.

### TAU-REVIEW.md follow-up plans (ordered by risk, lowest first)

- `2026-03-22-typed-tool-args.md` — Replace per-tool manual JSON field extraction
  with typed `#[derive(Deserialize)]` structs and a shared `parse_args` helper.
  Zero interface changes; purely internal. (Review §5)

- `2026-03-22-config-validation.md` — Add `TauConfig::warnings()` to surface
  obvious misconfigurations (unknown provider name, openai without api_key) as
  startup messages rather than deferred runtime errors. (Review §10)

- `2026-03-22-provider-model-metadata.md` — Consolidate the two independent
  Copilot model-name matching functions (`classify_copilot_route` and
  `thinking_support_for`) into a single static metadata table. (Review §7, §8)

- `2026-03-22-agent-cancellation.md` — Two-phase: (A) add `.kill_on_drop(true)`
  to shell tool commands so child processes are killed on abort; (B) add an
  explicit `watch::Receiver<bool>` cancellation parameter to `run_agent_loop`
  for deterministic, testable inter-turn cancellation. (Review §9)

Historical exploratory plans and superseded implementation notes were pruned during the 2026-03-15 documentation cleanup to reduce drift and maintenance overhead.
