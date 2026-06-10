use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

/// Metadata parsed from the YAML frontmatter of a `SKILL.md` file.
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    /// Absolute path to the `SKILL.md` file.
    pub path: PathBuf,
    /// Directory containing the `SKILL.md` file (base for relative references).
    pub base_dir: PathBuf,
    /// For embedded skills: the skill body. When set, `read_skill` returns this
    /// instead of reading from `path`.
    pub embedded_body: Option<String>,
}

/// Scan all supported skill roots for subdirectories that contain a
/// `SKILL.md` with YAML frontmatter.
///
/// Skill roots:
/// - `~/.xi/skills`
/// - `~/.agents/skills`
/// - `%USERPROFILE%\\.agents\\skills` (Windows)
/// - `./.agents/skills`
/// - `./.xi/skills`
///
/// Also injects the embedded `edit_skill` that tells the model where skill
/// files live on this system and where to create new ones.
///
/// Returns an empty vec when no skill roots exist or are readable.
pub fn load_skills() -> Vec<SkillMeta> {
    let mut skills = load_skills_from_dirs(skill_dirs());
    skills.push(build_embedded_edit_skill(&skills));
    skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    skills
}

/// Build the embedded `edit_skill` with a body that lists:
/// - all loaded skill files with absolute paths
/// - the skill root directories where xi searches
/// - guidance for modifying existing skills and creating new ones
fn build_embedded_edit_skill(loaded: &[SkillMeta]) -> SkillMeta {
    let dirs = skill_dirs();

    // Split dirs into global (home-relative) and project (cwd-relative).
    let cwd = env::current_dir().ok();
    let cwd_canon = cwd
        .as_ref()
        .and_then(|p| p.canonicalize().ok())
        .or_else(|| cwd.clone());
    let is_project = |d: &PathBuf| -> bool {
        if let Some(ref cwd) = cwd_canon {
            let d_canon = d.canonicalize().unwrap_or_else(|_| d.clone());
            d_canon.starts_with(cwd)
        } else {
            false
        }
    };

    let (project_dirs, global_dirs): (Vec<&PathBuf>, Vec<&PathBuf>) =
        dirs.iter().partition(|d| is_project(d));

    // Classify each search directory by whether it contributed any skills.
    let classify_skill_root = |d: &PathBuf| -> bool {
        let d_canon = d.canonicalize().unwrap_or_else(|_| d.clone());
        loaded.iter().any(|s| {
            let base = s
                .base_dir
                .canonicalize()
                .unwrap_or_else(|_| s.base_dir.clone());
            base.starts_with(&d_canon)
        })
    };

    let in_use: Vec<&PathBuf> = dirs.iter().filter(|d| classify_skill_root(d)).collect();

    let dirs_section = {
        let mut lines: Vec<String> = Vec::new();
        if !global_dirs.is_empty() {
            lines.push("Global (home directory):".to_string());
            for d in &global_dirs {
                let marker = if in_use.contains(d) {
                    " ← in use"
                } else {
                    ""
                };
                lines.push(format!("  - `{}`{marker}", d.display()));
            }
        }
        if !project_dirs.is_empty() {
            lines.push("Project-local (current working directory):".to_string());
            for d in &project_dirs {
                let marker = if in_use.contains(d) {
                    " ← in use"
                } else {
                    ""
                };
                lines.push(format!("  - `{}`{marker}", d.display()));
            }
        }
        lines.join("\n")
    };

    let skills_section = if loaded.is_empty() {
        "(none loaded)\n".to_string()
    } else {
        loaded
            .iter()
            .map(|s| {
                // Skip the embedded edit_skill itself.
                if s.name == "edit_skill" {
                    return String::new();
                }
                let scope = if project_dirs.iter().any(|d| classify_skill_root(d)) {
                    let base = s
                        .base_dir
                        .canonicalize()
                        .unwrap_or_else(|_| s.base_dir.clone());
                    let is_project_skill = project_dirs.iter().any(|d| {
                        let d_canon = d.canonicalize().unwrap_or_else(|_| (*d).clone());
                        base.starts_with(&d_canon)
                    });
                    if is_project_skill {
                        " [project]"
                    } else {
                        " [global]"
                    }
                } else {
                    ""
                };
                format!("- `{}`{} → {}", s.name, scope, s.path.display())
            })
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let create_section = if !in_use.is_empty() {
        let preferred: Vec<_> = in_use
            .iter()
            .map(|d| format!("`{}`", d.display()))
            .collect();
        let preferred_list = preferred.join(" or ");
        format!(
            "Put new skills in an already-active directory: {preferred_list}.\n\
             Create a new subdirectory there and write a `SKILL.md` inside it\n\
             with the frontmatter shown above."
        )
    } else {
        "No skill directories are currently in use. Pick one of the search\n\
         directories above, create a new subdirectory, and write a `SKILL.md`\n\
         inside it with the frontmatter shown above."
            .to_string()
    };

    let body = format!(
        "\
# Edit Skill

## Skill search directories

xi searches these directories recursively for subdirectories containing
`SKILL.md` files. Directories that don't exist are silently skipped.
Directories marked \"← in use\" currently contain skill files.

{dirs_section}

## Currently loaded skills

These are the skill files xi found at startup (absolute paths):

{skills_section}

## How skills are structured

Each skill lives in its own subdirectory under one of the search directories
above. The directory name does not have to match the skill name — only the
`name` field in the YAML frontmatter of `SKILL.md` determines the skill
identity. A minimal `SKILL.md` looks like:

```markdown
---
name: my-skill
description: when to use this skill
---

# Skill body here
```

## Modifying an existing skill

Find the skill's absolute path in the list above and edit its `SKILL.md`.

Scope indicators:
- `[global]` — lives under a home-directory skill root, shared across all projects.
- `[project]` — lives under a project-local skill root, specific to the current repo.

## Creating a new skill

{create_section}
",
    );

    SkillMeta {
        name: "edit_skill".to_string(),
        description:
            "use when the user wants to edit, modify, create, or delete a skill. catch phrases: 'change the skill', 'update SKILL.md', 'add a new skill', 'where are skill files', 'create a skill'."
                .to_string(),
        // Dummy path — never read from disk; read_skill uses embedded_body.
        path: PathBuf::from("__embedded__/edit_skill/SKILL.md"),
        base_dir: PathBuf::from("__embedded__/edit_skill"),
        embedded_body: Some(body),
    }
}

fn skill_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = env::var_os("HOME").filter(|s| !s.is_empty()) {
        let home = PathBuf::from(home);
        dirs.push(home.join(".xi").join("skills"));
        dirs.push(home.join(".agents").join("skills"));
    }

    if cfg!(windows)
        && let Some(user_profile) = env::var_os("USERPROFILE").filter(|s| !s.is_empty())
    {
        dirs.push(PathBuf::from(user_profile).join(".agents").join("skills"));
    }

    if let Ok(cwd) = env::current_dir() {
        dirs.push(cwd.join(".agents").join("skills"));
        dirs.push(cwd.join(".xi").join("skills"));
    }

    dirs
}

fn load_skills_from_dirs(dirs: Vec<PathBuf>) -> Vec<SkillMeta> {
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    let mut skills: Vec<SkillMeta> = Vec::new();

    for dir in dirs {
        skills.extend(load_skills_from_dir(
            &dir,
            &mut seen_files,
            &mut visited_dirs,
        ));
    }

    // Deterministic order.
    skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    skills
}

fn load_skills_from_dir(
    dir: &Path,
    seen_files: &mut HashSet<PathBuf>,
    visited_dirs: &mut HashSet<PathBuf>,
) -> Vec<SkillMeta> {
    if !dir.exists() {
        return vec![];
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };

    let mut skills = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            load_skills_recursive(&path, seen_files, visited_dirs, &mut skills);
        }
    }

    skills
}

fn load_skills_recursive(
    dir: &Path,
    seen_files: &mut HashSet<PathBuf>,
    visited_dirs: &mut HashSet<PathBuf>,
    skills: &mut Vec<SkillMeta>,
) {
    let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !visited_dirs.insert(canonical_dir) {
        return;
    }

    let skill_file = dir.join("SKILL.md");
    if skill_file.is_file() {
        let canonical = skill_file.canonicalize().unwrap_or(skill_file.clone());
        if seen_files.insert(canonical)
            && let Ok(content) = fs::read_to_string(&skill_file)
            && let Some(meta) = parse_skill_meta(&content, skill_file)
        {
            skills.push(meta);
        }
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            load_skills_recursive(&path, seen_files, visited_dirs, skills);
        }
    }
}

/// Parse `name` and `description` from the YAML frontmatter block (`---` … `---`)
/// at the start of a `SKILL.md` file.
fn parse_skill_meta(content: &str, path: PathBuf) -> Option<SkillMeta> {
    let mut lines = content.lines();

    // First line must be `---`
    if lines.next()?.trim() != "---" {
        return None;
    }

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;

    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        }
    }

    let base_dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();

    Some(SkillMeta {
        name: name?,
        description: description?,
        path,
        base_dir,
        embedded_body: None,
    })
}

/// Expand a skill invocation into the `<skill>` XML block that is submitted to
/// the model, following the Agent Skills specification format used by pi.
///
/// Format:
/// ```text
/// <skill name="{name}" location="{path}">
/// References are relative to {base_dir}.
///
/// {body — SKILL.md with frontmatter stripped}
/// </skill>
///
/// {optional args}
/// ```
pub fn expand_skill(skill: &SkillMeta, args: &str) -> anyhow::Result<String> {
    let content = fs::read_to_string(&skill.path)?;
    let body = strip_frontmatter(&content).trim();

    let skill_block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.path.display(),
        skill.base_dir.display(),
        body,
    );

    if args.is_empty() {
        Ok(skill_block)
    } else {
        Ok(format!("{skill_block}\n\n{args}"))
    }
}

/// Strip YAML frontmatter (`---` … `---`) from the start of a file and return
/// the body text.  Returns the original string unchanged if no frontmatter is
/// found.
fn strip_frontmatter(content: &str) -> &str {
    let mut pos: usize = 0;
    let mut fence_seen = false;

    for line in content.split('\n') {
        // Handle CRLF gracefully.
        let trimmed = line.trim_end_matches('\r');
        // Byte length including the '\n' separator we split on.
        let advance = line.len() + 1;

        if !fence_seen {
            if trimmed == "---" {
                fence_seen = true;
                pos += advance;
                continue;
            } else {
                // Content does not start with a frontmatter fence.
                return content;
            }
        }

        pos += advance;

        if trimmed == "---" {
            // `pos` now points to the first byte after the closing `---\n`.
            return if pos <= content.len() {
                &content[pos..]
            } else {
                ""
            };
        }
    }

    content
}

#[cfg(test)]
mod tests {
    use super::{expand_skill, load_skills_from_dirs, parse_skill_meta, strip_frontmatter};
    use std::path::PathBuf;

    fn dummy_path() -> PathBuf {
        PathBuf::from("/tmp/skills/work-loop/SKILL.md")
    }

    #[test]
    fn parses_standard_frontmatter() {
        let content = "\
---
name: work-loop
description: guides most non-trivial coding work.
---

# Work loop
";
        let meta = parse_skill_meta(content, dummy_path()).expect("should parse");
        assert_eq!(meta.name, "work-loop");
        assert_eq!(meta.description, "guides most non-trivial coding work.");
        assert_eq!(meta.base_dir, PathBuf::from("/tmp/skills/work-loop"));
    }

    #[test]
    fn returns_none_without_opening_fence() {
        let content = "name: foo\ndescription: bar\n";
        assert!(parse_skill_meta(content, dummy_path()).is_none());
    }

    #[test]
    fn returns_none_when_name_missing() {
        let content = "---\ndescription: bar\n---\n";
        assert!(parse_skill_meta(content, dummy_path()).is_none());
    }

    #[test]
    fn returns_none_when_description_missing() {
        let content = "---\nname: foo\n---\n";
        assert!(parse_skill_meta(content, dummy_path()).is_none());
    }

    #[test]
    fn strip_frontmatter_removes_fence() {
        let content = "---\nname: foo\n---\n\n# Body\n";
        assert_eq!(strip_frontmatter(content), "\n# Body\n");
    }

    #[test]
    fn strip_frontmatter_no_fence_returns_original() {
        let content = "# Just a doc\nno frontmatter here\n";
        assert_eq!(strip_frontmatter(content), content);
    }

    #[test]
    fn expand_skill_wraps_body() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "---\nname: my-skill\ndescription: test.\n---\n\n# My skill\nDo the thing."
        )
        .unwrap();

        let meta =
            parse_skill_meta(&std::fs::read_to_string(&path).unwrap(), path.clone()).unwrap();

        let expanded = expand_skill(&meta, "").unwrap();
        assert!(expanded.contains("<skill name=\"my-skill\""));
        assert!(expanded.contains("References are relative to"));
        assert!(expanded.contains("# My skill"));
        assert!(expanded.contains("</skill>"));
    }

    #[test]
    fn expand_skill_appends_args() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: my-skill\ndescription: test.\n---\n\n# Body").unwrap();

        let meta =
            parse_skill_meta(&std::fs::read_to_string(&path).unwrap(), path.clone()).unwrap();

        let expanded = expand_skill(&meta, "implement the feature").unwrap();
        assert!(expanded.ends_with("\n\nimplement the feature"));
    }

    #[test]
    fn load_skills_merges_all_roots() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let root_a = dir.path().join("a");
        let root_b = dir.path().join("b");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();

        let skill_a_dir = root_a.join("skill-a");
        std::fs::create_dir_all(&skill_a_dir).unwrap();
        let skill_a = skill_a_dir.join("SKILL.md");
        let mut file_a = std::fs::File::create(&skill_a).unwrap();
        writeln!(
            file_a,
            "---\nname: skill-a\ndescription: from root a\n---\n"
        )
        .unwrap();

        let skill_b_dir = root_b.join("skill-b");
        std::fs::create_dir_all(&skill_b_dir).unwrap();
        let skill_b = skill_b_dir.join("SKILL.md");
        let mut file_b = std::fs::File::create(&skill_b).unwrap();
        writeln!(
            file_b,
            "---\nname: skill-b\ndescription: from root b\n---\n"
        )
        .unwrap();

        let skills = load_skills_from_dirs(vec![root_a.clone(), root_b.clone()]);
        assert_eq!(skills.len(), 2);
        assert!(skills.iter().any(|s| s.name == "skill-a"));
        assert!(skills.iter().any(|s| s.name == "skill-b"));
    }

    #[test]
    fn load_skills_discovers_nested_skill_dirs() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let nested = root.join("group").join("skill-nested");
        std::fs::create_dir_all(&nested).unwrap();

        let skill = nested.join("SKILL.md");
        let mut file = std::fs::File::create(&skill).unwrap();
        writeln!(file, "---\nname: nested\ndescription: nested skill\n---\n").unwrap();

        let skills = load_skills_from_dirs(vec![root]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "nested");
    }

    #[test]
    fn load_skills_ignores_root_markdown_files() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        let mut root_md = std::fs::File::create(root.join("foo.md")).unwrap();
        writeln!(
            root_md,
            "---\nname: bad\ndescription: should be ignored\n---"
        )
        .unwrap();

        let skills = load_skills_from_dirs(vec![root]);
        assert!(skills.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn load_skills_handles_symlink_loops() {
        use std::io::Write;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let skill_dir = root.join("skill-loop");
        std::fs::create_dir_all(&skill_dir).unwrap();

        let mut skill_file = std::fs::File::create(skill_dir.join("SKILL.md")).unwrap();
        writeln!(
            skill_file,
            "---\nname: loop-safe\ndescription: loop-safe discovery\n---\n"
        )
        .unwrap();

        symlink(&root, skill_dir.join("back-to-root")).unwrap();

        let skills = load_skills_from_dirs(vec![root]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "loop-safe");
    }
}
