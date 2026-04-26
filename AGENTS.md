# AGENTS.md

This repository is a AI agent harness for the terminal, built with Rust.

## General rules

- Fix all compiler warnings before committing.
- Fix all clippy issues before committing (`cargo clippy`).
- Ensure all tests pass before committing (`cargo test`).

## Working modes

- Use the `fastpath` skill for trivial, clearly bounded changes when appropriate.
- Use the `workflow` skill for non-trivial changes.
- Follow the active skill for stage gates, acceptance handling, reconciliation, and any repository follow-through.

## Debugging

- Debug logs are written to `~/.cache/tau`.

## Commit preflight checks

Run `just preflight` before every commit. It enforces:

- Formatting (`cargo fmt --all -- --check`)
- Lint with warnings as errors (`cargo clippy --all-targets --all-features -- -D warnings`)
- Tests (`cargo test --all-features`)
- Compilation of all targets (`cargo check --all-targets --all-features`)

## Issue and plan tracking

Open work items are tracked as Gitea issues at https://gitea.belunktum.dk/larsch/tau/issues.

- Do not create local `docs/plans/` files for new work — create a Gitea issue instead (use the `gitea-cli` skill).
- When planning non-trivial work, the Gitea issue body should capture scope, approach, success criteria, steps, risks, and verification approach.
- When work is complete, close the Gitea issue with a comment summarising what was done (use the `gitea-cli` skill).
- `docs/plans/` is kept for temporary in-progress working notes only, not as a permanent record.

## Committing and pushing

- Follow the active skill's gate and authorization rules for `git commit` and `git push`.
- A passing preflight is not by itself permission to commit.
- Explicit user authorization in the current session turn, or an active wrap-up order, is required to commit and push.
