---
name: release
description: releasing a new version of xi-agent — bumping the version, updating the changelog, tagging, creating the GitHub release, and publishing to crates.io. use when the user says "release X.Y.Z" or asks to cut or publish a release.
---

# Release process for xi-agent

Cut a release from the repo root (`/home/larsch/prj/xi-agent`). The previous
release commit shows the exact shape of a release change (`git show vX.Y.Z`).

## 1. Review changes since the last tag

```bash
git tag --sort=-v:refname | head
git log vX.Y.Z..HEAD --oneline
```

Group the commits into changelog sections (`Added`, `Changed`, `Fixed`,
`Performance`, `Internal`).

## 2. Bump the version

Edit `version = "..."` in `Cargo.toml`, then update `Cargo.lock`:

```bash
cargo check --all-targets --all-features --quiet
```

## 3. Update CHANGELOG.md

Add a `## vX.Y.Z — YYYY-MM-DD` section at the top, above the previous version,
using the existing subsection style.

Guidelines:

- **Do not hard-wrap bullet text** — keep each bullet on one line. This same
  text feeds the GitHub release notes, where hard wraps render as unwanted
  line breaks.
- Distinguish **new** from **changed**: a pre-existing feature whose behavior
  changed goes under `### Changed`, not `### Added`.
- Describe performance symptoms accurately (e.g. "high CPU load", not "hang").

## 4. Preflight

```bash
just preflight
```

Must pass: fmt, clippy (`-D warnings`), tests, and `cargo check --all-targets`.

## 5. Commit and tag

The commit hook enforces Conventional Commits — `release: vX.Y.Z` is
**rejected**. Use:

```bash
git add CHANGELOG.md Cargo.toml Cargo.lock
git commit -m "chore(release): vX.Y.Z"
git tag vX.Y.Z          # lightweight tag (matches existing convention)
git push origin main
git push origin vX.Y.Z
```

## 6. GitHub release

```bash
gh release create vX.Y.Z --title "vX.Y.Z" --notes-file /tmp/relnotes.md
```

Gotchas:

- Do **not** pass `--target vX.Y.Z`; `target_commitish` must be a branch or
  SHA, not a tag. The tag name as the first positional argument is enough.
- Write the notes to a file and pass `--notes-file`. Each bullet must be on a
  single line (no hard wraps).

## 7. Publish to crates.io

```bash
cargo publish --dry-run   # verify package contents and build
cargo publish
```

- The crate `xi-agent` already exists; publish the next version.
- Check the current max version at `https://crates.io/api/v1/crates/xi-agent`
  — send a `User-Agent` header; crates.io rejects requests without one.

## Gotchas summary

- Commit message must be Conventional Commits (`chore(release): ...`).
- Tags are lightweight (not annotated).
- GitHub release notes must not contain hard-wrapped lines.
- `gh release create` rejects `--target <tag>`.
- Changelog: "new" vs "changed" matters; be precise about performance symptoms.
