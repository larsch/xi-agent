---
name: xi-customization
description: Helps create, write, edit, or troubleshoot xi skills, AGENTS.md instructions, rule files, hooks, agent profiles, and custom tools. Use when asked to customize xi, make behavior persistent, or add global or project-local instructions for future sessions.
---

# Changing how xi behaves

This guide maps requests about xi's behavior to the relevant mechanism. The focused guides below describe each mechanism's discovered files, format, scope, precedence, and loading point. These `xi-config-*` guides are hidden from the available-skills list, but they are loadable by calling the `learn` tool with the exact name:

- `xi-config-skills`: reusable guidance loaded on demand.
- `xi-config-agents-md`: persistent instructions included in the system prompt.
- `xi-config-rules`: persistent Markdown rule files included in the system prompt.
- `xi-config-hooks`: commands run at agent lifecycle and interaction points.
- `xi-config-agents`: named agent profiles that change identity and filter tools or skills.
- `xi-config-tools`: executable tools discovered by xi.

Load the relevant guide with `learn` using its exact skill name. Each guide is also available through `/skill:<name>`. The `xi-config-` prefix distinguishes these embedded guides from user-created skills with similar names.

These mechanisms have different effects and loading times. Skills, instructions, rules, agent profiles, hooks, and custom tools are separate mechanisms; consult the corresponding guide for the relevant loading behavior. This guide covers these customization mechanisms only; it does not cover templates or provider configuration.
