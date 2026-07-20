# AGENTS.md

This repository is a AI agent harness for the terminal, built with Rust.

## Repository hosting

- The source code is hosted on **GitHub** at `https://github.com/larsch/xi-agent`.
- Use `gh` for GitHub PRs, issues on the code repo, and other GitHub operations.
- Use `tea` (Gitea CLI) **only** for project-tracking issues at `https://gitea.belunktum.dk/larsch/xi-agent`, not for code PRs or code-level issues.
- When checking PRs or issues, always resolve the relevant remote (`git remote -v`) before picking a tool.

## General rules

- Format all Rust code before committing (`cargo fmt --all --`).
- Fix all compiler warnings before committing.
- Fix all clippy issues before committing (`cargo clippy`).
- Ensure all tests pass before committing (`cargo test`).
- Remove unused code rather than suppressing it. Only use `#[allow(dead_code)]` when the compiler cannot see a real use (e.g. serde-populated fields, test helpers that must live outside `#[cfg(test)]`). Always add a comment explaining why.

## Working modes

- Use the `fastpath` skill for trivial, clearly bounded changes when appropriate.
- Use the `workflow` skill for non-trivial changes.
- Follow the active skill for stage gates, acceptance handling, reconciliation, and any repository follow-through.

## Debugging

- Debug logs are written to `~/.cache/xi`.

## Commit preflight checks

Before staging any Rust changes, run `cargo fmt --all --` to auto-fix formatting. Then run `just preflight` before every commit. Together they enforce:

- Formatting (`cargo fmt --all --` auto-fix, then `cargo fmt --all -- --check` as safety net)
- Lint with warnings as errors (`cargo clippy --all-targets --all-features -- -D warnings`)
- Tests (`cargo test --all-features`)
- Compilation of all targets (`cargo check --all-targets --all-features`)

## Issue and plan tracking

Open work items are tracked as Gitea issues at https://gitea.belunktum.dk/larsch/xi-agent/issues.

- Do not create local `docs/plans/` files for new work — create a Gitea issue instead (use the `gitea-cli` skill).
- When planning non-trivial work, the Gitea issue body should capture scope, approach, success criteria, steps, risks, and verification approach.
- When work is complete, close the Gitea issue with a comment summarising what was done (use the `gitea-cli` skill).
- `docs/plans/` is kept for temporary in-progress working notes only, not as a permanent record.

## Committing and pushing

- Follow the active skill's gate and authorization rules for `git commit` and `git push`.
- A passing preflight is not by itself permission to commit.
- Explicit user authorization in the current session turn, or an active wrap-up order, is required to commit and push.
