---
name: xi-config-skills
description: Explains xi skill discovery, format, scope, precedence, and loading behavior, with a snapshot of skills discovered in this session.
metadata:
  hidden: "true"
---

# Skills in xi

A skill is reusable guidance loaded on demand. It is not automatically applied as a persistent instruction merely because xi discovered it.

## Discovery and precedence

xi recursively searches these roots for subdirectories containing `SKILL.md`, in order:

1. Project roots from the current working directory to filesystem root, with `.xi/skills` before `.agents/skills` at each level.
2. Global roots: `~/.xi/skills`, `~/.agents/skills`, and on Windows `%USERPROFILE%\\.agents\\skills`.
3. Skills under each discovered agent profile's `skills/` directory.

Missing roots are skipped. Symlinked directories are followed with cycle detection. For standard roots, the first skill with a given `name` wins. Agent-scoped skills are selected after agent filters; active agent-specific definitions take precedence over default-agent and standard definitions with the same name. `learn` can load the full discovered registry, including skills outside the active agent's selected list.

## File format

Each skill is a directory containing `SKILL.md` with frontmatter beginning on the first line:

```markdown
---
name: summarize-data
description: Explains how to summarize tabular data.
---

# Guidance
Skill body in Markdown. Relative references resolve from this directory.
```

`name` and `description` are required and non-empty. The closing `---` is required. Invalid files are skipped. The directory name does not determine the skill name. `metadata` is a mapping of string keys to string values; xi reads `metadata.hidden` when its value is `"true"`.

## Visibility

A skill with `metadata.hidden: "true"` is omitted from the prompt's available-skills list. It remains loadable by exact name through `learn`, can be invoked as `/skill:<name>`, and appears in the F3 inventory as hidden but available. Hidden status does not change its scope or discovery.

## Loading

Skills are discovered when xi loads its context at startup. `/reload` reloads skills. A skill's body is supplied when loaded with `learn` or invoked with `/skill:<name>`. The prompt's available-skills list contains selected skills; agent filters affect that list and ordinary user-skill slash invocation. Built-in xi customization guides are loadable and slash-invokable regardless of those filters.
