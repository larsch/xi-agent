---
name: xi-config-agents
description: Explains xi agent profile discovery, format, filters, and loading behavior.
metadata:
  hidden: "true"
---

# Agent profiles in xi

Agent profiles are directory-based definitions that change the system-prompt identity and can filter the available tools and skills. An agent profile is distinct from `AGENTS.md`, which supplies persistent instructions.

## Discovery and precedence

xi recursively searches `~/.xi/agents`, `~/.agents/agents`, and on Windows `%USERPROFILE%\\.agents\\agents`, followed by project roots `.agents/agents` and `.xi/agents` in the current directory. A profile is a subdirectory containing `SYSTEM.md`, or legacy `AGENT.md` if `SYSTEM.md` is absent. Project-local profiles shadow global profiles with the same `name`.

## Definition format

```markdown
---
name: reviewer
description: Reviews code changes.
mode: primary
include_tools: ["*"]
exclude_tools: []
include_skills: ["*"]
exclude_skills: []
---

You are a code reviewer.
```

`name` and `description` are required. `mode` defaults to `primary`; `subagent` is reserved for orchestration. Tool and skill include filters default to all when omitted; exclusions are applied after inclusions. The body replaces the default identity paragraph. The parser accepts inline string lists for filter fields.

An optional `AGENTS.md` alongside the definition replaces global home instructions for that agent; project-chain instructions remain included.

## Loading and selection

Agent profiles are loaded during context loading at startup. `/reload` reloads profiles. Switching agents rebuilds the system prompt and applies the selected profile's filters. The active agent selection is associated with the session, not persisted in global configuration.
