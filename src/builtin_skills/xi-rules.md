# Rule files in xi

Rule files are Markdown documents included as persistent instructions in the system prompt. They are separate from `AGENTS.md` entries and from on-demand skills.

## Discovery and precedence

xi searches `.xi/rules` and `.agents/rules` at each directory from the current working directory to the filesystem root, with `.xi/rules` first at each level. Home rule roots are searched afterward: `~/.xi/rules` and `~/.agents/rules` (and `%USERPROFILE%\\.agents\\rules` on Windows).

All Markdown files are eligible. Files are sorted by name within each directory. When filenames collide, the first encountered file wins, so nearer project files take precedence over ancestor and home files.

## Content and loading

Rule files are plain Markdown; they have no frontmatter format. Their contents are read when xi builds the system prompt for the agent context. The prompt includes each file as a separate rule document with its source path. The system prompt is built at startup and when the agent context is rebuilt, including after `/reload` or an agent switch. Existing instructions already sent in earlier turns are not retroactively changed; later model requests use the rebuilt prompt.
