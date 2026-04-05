# AGENTS.md

This repository is a AI agent harness for the terminal, built with Rust.

## General rules

- Fix all compiler warnings before committing.
- Fix all clippy issues before committing (`cargo clippy`).
- Ensure all tests pass before committing (`cargo test`).

## Debugging

- Debug logs are written to `~/.cache/tau`.

## Commit preflight checks

Run `just preflight` before every commit. It enforces:

- Formatting (`cargo fmt --all -- --check`)
- Lint with warnings as errors (`cargo clippy --all-targets --all-features -- -D warnings`)
- Tests (`cargo test --all-features`)
- Compilation of all targets (`cargo check --all-targets --all-features`)

## Committing and pushing

- Never run `git commit` or `git push` without explicit user instruction.
- A passing preflight is not permission to commit — wait for the user to say so.
