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
/// Skill roots are searched in precedence order:
/// - `.xi/skills`, then `.agents/skills`, at every directory from the current
///   working directory through the filesystem root
/// - `~/.xi/skills`, then `~/.agents/skills`
/// - `%USERPROFILE%\\.agents\\skills` (Windows)
///
/// A skill name found in a higher-precedence root suppresses same-named skills
/// in lower-precedence roots.
///
/// Also injects the embedded `xi-skill-locations` with technical guidance about
/// skill discovery, file locations, and the supported format.
pub fn load_skills() -> Vec<SkillMeta> {
    let mut skills = load_skills_from_dirs(skill_dirs());
    skills.push(build_embedded_skill_locations(&skills));
    skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    skills
}

/// Build technical guidance with the startup inventory and skill search roots.
fn build_embedded_skill_locations(loaded: &[SkillMeta]) -> SkillMeta {
    let dirs = skill_dirs();

    // Split dirs into global (home-relative) and project (cwd-to-root) roots.
    let project_root_dirs = env::current_dir()
        .map(|cwd| project_skill_dirs(&cwd))
        .unwrap_or_default();
    let is_project = |d: &PathBuf| -> bool {
        let d_canon = d.canonicalize().unwrap_or_else(|_| d.clone());
        project_root_dirs.iter().any(|project_dir| {
            project_dir
                .canonicalize()
                .unwrap_or_else(|_| project_dir.clone())
                == d_canon
        })
    };

    let project_dirs: Vec<&PathBuf> = dirs.iter().filter(|d| is_project(d)).collect();

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

    let dirs_section = dirs
        .iter()
        .map(|d| {
            let scope = if is_project(d) { "project" } else { "global" };
            let marker = if in_use.contains(&d) {
                " ← in use"
            } else {
                ""
            };
            format!("- `{}` [{scope}]{marker}", d.display())
        })
        .collect::<Vec<_>>()
        .join("\n");

    let skills_section = if loaded.is_empty() {
        "(none loaded)\n".to_string()
    } else {
        loaded
            .iter()
            .map(|s| {
                let base = s
                    .base_dir
                    .canonicalize()
                    .unwrap_or_else(|_| s.base_dir.clone());
                let is_project_skill = project_dirs.iter().any(|d| {
                    let d_canon = d.canonicalize().unwrap_or_else(|_| (*d).clone());
                    base.starts_with(&d_canon)
                });
                let scope = if is_project_skill {
                    "project"
                } else {
                    "global"
                };
                format!("- `{}` [{scope}] → {}", s.name, s.path.display())
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let body = format!(
        "\
# xi skill locations

This built-in skill provides technical guidance about xi’s skill discovery,
file locations, and supported format. It does not define skill-authoring
policy or override user direction or applicable system, developer, or
project instructions.

For authoring workflows and quality guidance, use applicable authoring
skills when available.

## Where this guidance lives

xi generates this skill at startup. It has no `SKILL.md` file on disk.
Its implementation lives in `src/skills.rs` in the xi source repository.

## Search directories

xi recursively searches these directories for subdirectories containing
`SKILL.md`. Missing directories are skipped. Roots are listed in precedence
order:

{dirs_section}

When filesystem skills share a frontmatter name, the first search root
containing that name takes precedence.

Directories marked “in use” contributed skills to this startup’s discovery
results. That marker is informational, not a recommendation about scope.

## Discovered skill files

These paths identify the filesystem skills xi discovered at startup:

{skills_section}

- `[global]`: discovered under a home-directory skill root.
- `[project]`: discovered under a skill root in the working directory or
  an ancestor. An ancestor root may apply to multiple repositories.

This inventory is a startup snapshot, not a live filesystem listing.

## File format

Each filesystem skill lives in a subdirectory containing `SKILL.md`.
The YAML frontmatter determines its identity; the directory name need not
match.

```markdown
---
name: my-skill
description: Describes the capability and when to use it.
---

# Skill instructions
```

## Locating or modifying a skill

Use the listed absolute path to locate an existing skill. Read its current
contents and applicable instructions before editing.

## Choosing a location for a new skill

Choose scope according to user intent and applicable guidance:

- Use a project root for repository- or subtree-specific guidance.
- Use a global root for guidance intended to apply across projects.

An existing active directory may be convenient within the chosen scope,
but activity alone does not determine placement. A supported search
directory need not already exist.
",
    );

    SkillMeta {
        name: "xi-skill-locations".to_string(),
        description:
            "Explains where xi discovers skills, locates loaded skill files, and describes the supported SKILL.md format. Use when locating skills or choosing where to store one."
                .to_string(),
        // Dummy path — never read from disk; read_skill uses embedded_body.
        path: PathBuf::from("__embedded__/xi-skill-locations/SKILL.md"),
        base_dir: PathBuf::from("__embedded__/xi-skill-locations"),
        embedded_body: Some(body),
    }
}

fn skill_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    let home = env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    if let Ok(cwd) = env::current_dir() {
        dirs.extend(
            project_skill_dirs(&cwd)
                .into_iter()
                .filter(|dir| !home.as_ref().is_some_and(|home| dir.starts_with(home))),
        );
    }

    if let Some(home) = home {
        dirs.push(home.join(".xi").join("skills"));
        dirs.push(home.join(".agents").join("skills"));
    }

    if cfg!(windows)
        && let Some(user_profile) = env::var_os("USERPROFILE").filter(|s| !s.is_empty())
    {
        dirs.push(PathBuf::from(user_profile).join(".agents").join("skills"));
    }

    dirs
}

/// Return project skill roots from `cwd` to the filesystem root. At each
/// directory level, `.xi` precedes `.agents`.
fn project_skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current_dir = cwd.to_path_buf();

    loop {
        dirs.push(current_dir.join(".xi").join("skills"));
        dirs.push(current_dir.join(".agents").join("skills"));

        match current_dir.parent() {
            Some(parent) if parent != current_dir => current_dir = parent.to_path_buf(),
            _ => break,
        }
    }

    dirs
}

fn load_skills_from_dirs(dirs: Vec<PathBuf>) -> Vec<SkillMeta> {
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut skills: Vec<SkillMeta> = Vec::new();

    for dir in dirs {
        let mut root_skills = load_skills_from_dir(&dir, &mut seen_files, &mut visited_dirs);
        root_skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
        for skill in root_skills {
            if seen_names.insert(skill.name.clone()) {
                skills.push(skill);
            }
        }
    }

    // Deterministic presentation order. Discovery order establishes precedence.
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
    use super::{
        build_embedded_skill_locations, expand_skill, load_skills_from_dirs, parse_skill_meta,
        project_skill_dirs, strip_frontmatter,
    };
    use std::path::{Path, PathBuf};

    fn write_skill(root: &Path, directory: &str, name: &str, description: &str) -> PathBuf {
        let skill_dir = root.join(directory);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        std::fs::write(
            &path,
            format!("---\nname: {name}\ndescription: {description}\n---\n"),
        )
        .unwrap();
        path
    }

    fn dummy_path() -> PathBuf {
        PathBuf::from("/tmp/skills/work-loop/SKILL.md")
    }

    #[test]
    fn embedded_skill_locations_explains_origin_and_authority() {
        let skill = build_embedded_skill_locations(&[]);
        assert_eq!(skill.name, "xi-skill-locations");
        assert_eq!(
            skill.path,
            PathBuf::from("__embedded__/xi-skill-locations/SKILL.md")
        );
        let body = skill.embedded_body.unwrap();
        assert!(body.contains("It has no `SKILL.md` file on disk."));
        assert!(body.contains("policy or override user direction"));
        assert!(body.contains("activity alone does not determine placement"));
        assert!(body.contains("(none loaded)"));
        assert!(!body.contains("Put new skills in an already-active directory"));
    }

    #[test]
    fn embedded_skill_locations_lists_files_even_with_old_builtin_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_skill(dir.path(), "custom", "edit_skill", "User guidance");
        let loaded = load_skills_from_dirs(vec![dir.path().to_path_buf()]);
        let body = build_embedded_skill_locations(&loaded)
            .embedded_body
            .unwrap();
        assert!(body.contains("`edit_skill` [global]"));
        assert!(body.contains(&path.display().to_string()));
        assert!(body.contains("startup snapshot, not a live filesystem listing"));
    }

    #[test]
    fn embedded_skill_locations_preserves_search_precedence() {
        let body = build_embedded_skill_locations(&[]).embedded_body.unwrap();
        let mut previous_end = 0;
        for dir in super::skill_dirs() {
            let entry = format!("- `{}` [", dir.display());
            let offset = body[previous_end..].find(&entry).unwrap();
            previous_end += offset + entry.len();
        }
        assert!(body.contains("first search root"));
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
    fn project_skill_dirs_walks_ancestors_with_xi_before_agents() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project").join("nested");
        let dirs = project_skill_dirs(&cwd);

        assert_eq!(
            &dirs[..6],
            [
                cwd.join(".xi/skills"),
                cwd.join(".agents/skills"),
                cwd.parent().unwrap().join(".xi/skills"),
                cwd.parent().unwrap().join(".agents/skills"),
                cwd.parent().unwrap().parent().unwrap().join(".xi/skills"),
                cwd.parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join(".agents/skills"),
            ]
        );
    }

    #[test]
    fn load_skills_prefers_first_root_for_duplicate_names() {
        let dir = tempfile::tempdir().unwrap();
        let nearer = dir.path().join("nearer");
        let farther = dir.path().join("farther");
        let nearer_path = write_skill(&nearer, "same", "duplicate", "nearer skill");
        write_skill(&farther, "same", "duplicate", "farther skill");
        write_skill(&farther, "other", "other", "other skill");

        let skills = load_skills_from_dirs(vec![nearer, farther]);

        assert_eq!(skills.len(), 2);
        let duplicate = skills
            .iter()
            .find(|skill| skill.name == "duplicate")
            .unwrap();
        assert_eq!(duplicate.path, nearer_path);
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
