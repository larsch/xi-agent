# AGENTS.md

This repository is a AI agent harness for the terminal, built with Rust.

## General rules

- Fix all compiler warnings before committing.
- Fix all clippy issues before committing (`cargo clippy`).
- Ensure all tests pass before committing (`cargo test`).

## Debugging

- Debug logs are written to `~/.cache/tau`.

## Commit preflight checks

- Compiles without warnings (`cargo build --all-features`).
- Passes all tests (`cargo test --all-features`).
- Passes clippy checks (`cargo clippy --all-features`).
- Passes formatting checks (`cargo fmt --all --check`).
