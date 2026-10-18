---
name: xi-config-agents-md
description: Explains persistent AGENTS.md instruction discovery, precedence, and loading behavior in xi.
metadata:
  hidden: "true"
---

# AGENTS.md instructions in xi

`AGENTS.md` files contain instructions included in the system prompt. They are not on-demand skills or hook commands.

## Discovery and precedence

For a named agent, an `AGENTS.md` beside its `SYSTEM.md` replaces the global home-directory instructions. Otherwise xi reads the first existing global candidate: `~/.xi/AGENTS.md`, `~/.agents/AGENTS.md`, or `~/AGENTS.md`.

For the working-directory chain, xi walks from the current directory to the filesystem root. At each level it reads the first existing candidate in this order: `.xi/AGENTS.md`, `.agents/AGENTS.md`, `AGENTS.md`. Files from each level are included. The home directory is not added again as a local entry.

## Content and loading

The file is plain Markdown; there is no frontmatter format. Its contents are included in the system prompt when xi builds the agent context. The named agent's `AGENTS.md`, when present, replaces the global entry; project-chain entries are still included.

The system prompt is built at session startup and when the agent context is rebuilt, including after `/reload` or an agent switch. Existing instructions already sent in earlier turns are not retroactively changed; later model requests use the rebuilt prompt.
