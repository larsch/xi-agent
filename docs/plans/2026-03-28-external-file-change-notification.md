# Plan: External File Change Notification

**Date:** 2026-03-28  
**Status:** Completed ✅

## Goal

Automatically notify the model when a file it previously read, wrote, or edited
has been modified externally (i.e. by a process other than the agent's own tool
calls) since the agent last touched it. Notification is delivered as a single
synthetic user message injected into the conversation before the next LLM turn,
plus a new `AgentEvent::ExternalFileChange` for the UI.

## Scope

**In scope:**
- Tracking files touched by `read_file`, `write_file`, `edit_file` (built-ins).
- mtime-first / SHA-256-on-mtime-change detection.
- One synthetic user message per turn boundary listing all changed paths.
- For small changes (diff ≤ 50 lines): inline unified diff (--- +++ @@ format,
  3 lines of context) in the notification message.
- For large changes (diff > 50 lines): warn-only (no diff content).
- New `AgentEvent::ExternalFileChange { paths: Vec<PathBuf> }`.

**Out of scope:**
- Custom tools (no tracker access from custom tool processes).
- `bash` / `find_files` tools.
- inotify / real-time watching.
- Persisting tracker state across sessions.

## Approach

A new `FileTracker` struct (`src/agent/file_tracker.rs`) maintains a
`HashMap<PathBuf, FileSnapshot { mtime: SystemTime, hash: [u8; 32], content: String }>`.
The stored content is needed to compute a diff against the new version on change.

**record(path):** stat + read text + SHA-256 → store snapshot (mtime, hash, content).  
**check_modified() → Vec<ChangedFile { path, old_content, new_content }>:**
for each entry, stat first; if mtime unchanged skip; re-hash; if hash differs →
collect with old+new content and update snapshot.

Diff generation (in `run_agent_loop` or a helper): use the `similar` crate to
produce a unified diff between `old_content` and `new_content`. Count the total
lines in the diff output. If ≤ 50 → inline the diff block in the message.
If > 50 → warn-only (no diff).

The threshold (50 lines) is a named constant `DIFF_INLINE_MAX_LINES`.

The tracker is held as `Arc<Mutex<FileTracker>>` in `AgentLoopConfig` and
cloned into `ReadFileTool`, `WriteTool`, `EditTool` at registration time.

At the top of every loop iteration in `run_agent_loop` (before the LLM call),
`check_modified()` is called. If non-empty, a `Message::user(...)` notification
is inserted into the turn's messages and `AgentEvent::ExternalFileChange` is sent.

## Success Criteria

1. Agent reads `foo.rs` → external write → ⚠️ message with inline diff appears before next LLM turn (when diff ≤ 50 lines).
2. Agent reads `big.rs` → large external rewrite (diff > 50 lines) → warn-only message, no diff.
3. External save with identical bytes (mtime-only bump) → no notification.
4. No external change → no notification.
5. Multiple changed files → one message listing all paths, each with its diff (or warn) block.
6. All existing tests pass; new `FileTracker` unit tests pass.
7. Zero `cargo clippy` warnings.

## Implementation Steps

1. Add `similar = "2"` to `Cargo.toml`.
2. Create `src/agent/file_tracker.rs` with `FileTracker`, `FileSnapshot` (mtime + hash + content), `record`, `check_modified` returning `Vec<ChangedFile { path, old_content, new_content }>`.
3. Expose via `src/agent/mod.rs`.
4. Add `file_tracker: Arc<Mutex<FileTracker>>` to `AgentLoopConfig` in `src/agent/types.rs`.
5. Add `AgentEvent::ExternalFileChange { paths: Vec<PathBuf> }` to `src/agent/types.rs`.
6. Update `register_builtin_tools()` to accept and thread `Arc<Mutex<FileTracker>>` into the three file tools.
7. Update `ReadFileTool`, `WriteTool`, `EditTool` to call `tracker.lock().record()` after success.
8. Update `run_agent_loop` to call `check_modified()`, generate unified diffs via `similar`, apply `DIFF_INLINE_MAX_LINES` threshold, compose notification message, inject into messages, and send event.
9. Handle `AgentEvent::ExternalFileChange` in `App::apply_event`.
10. Wire tracker into `start_agent_task` and `App::new` / test helper.
11. Write unit tests for `FileTracker` (no-change, mtime-only, mtime+content-change) and diff threshold helper.
12. `cargo clippy` + `cargo test` clean.

## Risks / Assumptions

- Clock resolution on FAT32 / some network FSes may cause false negatives — acceptable, best-effort.
- Storing file content in `FileSnapshot` uses more memory than a hash-only approach; acceptable for typical code files (KBs, not MBs). Binary files that fail UTF-8 decode are silently skipped (no tracking, no diff).
- `similar` crate adds a new dependency; it is small and widely used.
- `sha2` crate is already a dependency — no extra dep for hashing.
- New `AgentEvent` variant will cause exhaustive-match compiler errors in `app.rs`; easy to resolve.

## Verification Outcome

- 270 tests pass (`cargo test`), including 6 new `FileTracker` unit tests.
- Zero `cargo clippy` warnings.
- Interactive verification: agent read `/tmp/tau-verify-test.txt`, file was
  modified externally (line appended), next turn triggered ⚠️ notification
  with inline unified diff. Model read the secret from the diff without a
  `read_file` round-trip. ✅

One implementation note: `AgentEvent::ExternalFileChange` carries both
`paths` and `notification` (the pre-formatted message text). This avoids
reconstructing the message in `App::apply_event` and keeps the UI handler
to a single `messages.push` + `bump_log_revision`.
