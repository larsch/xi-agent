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

`name` and `description` are required and non-empty. Only these frontmatter fields are recognized. The closing `---` is required. Invalid files are skipped. The directory name does not determine the skill name.

## Loading

Skills are discovered when xi loads its context at startup. `/reload` reloads skills. A skill's body is supplied when loaded with `learn` or invoked with `/skill:<name>`. The prompt's available-skills list contains selected skills; agent filters affect that list and ordinary user-skill slash invocation. Built-in xi customization guides are loadable and slash-invokable regardless of those filters.
