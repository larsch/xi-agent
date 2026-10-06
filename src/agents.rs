//! User-definable agent profiles — filesystem-based agent definitions that
//! customise system prompt, tool availability, and skill availability.
//!
//! Each agent lives in a subdirectory under an agent root (e.g.
//! `~/.xi/agents/{name}/`). Two files are recognised:
//!
//! - `SYSTEM.md` (required) — YAML frontmatter with metadata + body that
//!   replaces the default system-prompt identity. Falls back to `AGENT.md`
//!   for backwards compatibility.
//! - `AGENTS.md` (optional) — replaces the global `~/.xi/AGENTS.md`
//!   instructions. Project-local AGENTS.md files (cwd→root chain) are still
//!   appended after the agent's AGENTS.md.

use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
};

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::agent::types::ToolRegistry;
use crate::skills::{SkillMeta, SkillScope};

// ── AgentMode ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    /// User-selectable agent shown in picker and switchable via `/agent`.
    Primary,
    /// Only invokable as a subagent by the orchestrator (Phase 2 — parsed but not wired).
    Subagent,
}

// ── AgentMeta ─────────────────────────────────────────────────────────────────

/// Parsed metadata from an agent definition directory.
#[derive(Debug, Clone)]
pub struct AgentMeta {
    pub name: String,
    pub description: String,
    pub mode: AgentMode,
    pub include_tools: Vec<String>,
    pub exclude_tools: Vec<String>,
    pub include_skills: Vec<String>,
    pub exclude_skills: Vec<String>,
    /// The system prompt body (markdown content after YAML frontmatter of
    /// `SYSTEM.md`, or `AGENT.md` for backwards compatibility).
    pub system_prompt: String,
    /// Content of the agent's `AGENTS.md`, if present. When set, this replaces
    /// the global `~/.xi/AGENTS.md` entry in the system prompt.
    pub agents_md: Option<String>,
    /// Absolute path to the metadata file (`SYSTEM.md` or `AGENT.md`).
    #[allow(dead_code)] // Reserved for future use (subagent file references, etc.)
    pub path: PathBuf,
    /// Directory containing the agent definition files.
    #[allow(dead_code)] // Reserved for future use (relative path resolution in subagents)
    pub base_dir: PathBuf,
}

// ── Always-present tools ──────────────────────────────────────────────────────

/// Tools that are always available to every agent regardless of filter settings.
pub const ALWAYS_PRESENT_TOOLS: &[&str] = &["ask_user", "read_skill", "agent_session"];

// ── Agent discovery ───────────────────────────────────────────────────────────

/// Directory roots searched for agent definitions.
fn agent_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = env::var_os("HOME").filter(|s| !s.is_empty()) {
        let home = PathBuf::from(home);
        dirs.push(home.join(".xi").join("agents"));
        dirs.push(home.join(".agents").join("agents"));
    }

    if cfg!(windows)
        && let Some(user_profile) = env::var_os("USERPROFILE").filter(|s| !s.is_empty())
    {
        dirs.push(PathBuf::from(user_profile).join(".agents").join("agents"));
    }

    if let Ok(cwd) = env::current_dir() {
        dirs.push(cwd.join(".agents").join("agents"));
        dirs.push(cwd.join(".xi").join("agents"));
    }

    dirs
}

/// Load agents from all discovery directories.
///
/// Returns a sorted (by name) list.  Agents from project-local directories
/// shadow global agents with the same name.
pub fn load_agents() -> Vec<AgentMeta> {
    let dirs = agent_dirs();
    let mut agents: Vec<AgentMeta> = Vec::new();
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();

    for dir in dirs {
        agents.extend(load_agents_from_dir(&dir, &mut seen_dirs));
    }

    // Deduplicate by name: later entries (project-local) shadow earlier (global).
    let mut deduped: Vec<AgentMeta> = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    for agent in agents.into_iter().rev() {
        if seen_names.insert(agent.name.clone()) {
            deduped.push(agent);
        }
    }
    deduped.reverse();
    deduped.sort_by(|a, b| a.name.cmp(&b.name));
    deduped
}

fn load_agents_from_dir(dir: &Path, visited_dirs: &mut HashSet<PathBuf>) -> Vec<AgentMeta> {
    if !dir.exists() {
        return vec![];
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };

    let mut agents = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            load_agents_recursive(&path, visited_dirs, &mut agents);
        }
    }

    agents
}

fn load_agents_recursive(
    dir: &Path,
    visited_dirs: &mut HashSet<PathBuf>,
    agents: &mut Vec<AgentMeta>,
) {
    let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !visited_dirs.insert(canonical_dir) {
        return;
    }

    // Prefer SYSTEM.md; fall back to AGENT.md for backwards compatibility.
    let system_file = dir.join("SYSTEM.md");
    let agent_file = dir.join("AGENT.md");

    let meta_path: Option<PathBuf> = if system_file.is_file() {
        Some(system_file)
    } else if agent_file.is_file() {
        Some(agent_file)
    } else {
        None
    };

    let agents_md_path = dir.join("AGENTS.md");
    let agents_md = if agents_md_path.is_file() {
        fs::read_to_string(&agents_md_path).ok()
    } else {
        None
    };

    if let Some(ref path) = meta_path
        && let Ok(content) = fs::read_to_string(path)
        && let Some(mut meta) = parse_agent_meta(&content, path.clone())
    {
        meta.agents_md = agents_md;
        agents.push(meta);
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            load_agents_recursive(&path, visited_dirs, agents);
        }
    }
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parse YAML frontmatter + body from a SYSTEM.md (or legacy AGENT.md) file.
///
/// Uses manual line-by-line parsing (matching `skills.rs`) to avoid an extra
/// serde_yaml dependency.  Only `name` and `description` are required; all
/// other fields fall back to sensible defaults.
fn parse_agent_meta(content: &str, path: PathBuf) -> Option<AgentMeta> {
    let mut lines = content.lines();

    // First line must be `---`
    if lines.next()?.trim() != "---" {
        return None;
    }

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut mode: Option<AgentMode> = None;
    let mut include_tools: Vec<String> = Vec::new();
    let mut exclude_tools: Vec<String> = Vec::new();
    let mut include_skills: Vec<String> = Vec::new();
    let mut exclude_skills: Vec<String> = Vec::new();
    let mut tools_include_seen = false;
    let mut _tools_exclude_seen = false;
    let mut skills_include_seen = false;
    let mut _skills_exclude_seen = false;
    let mut list_target: Option<u8> = None;

    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            let item = item.trim().trim_matches('"').trim_matches('\'');
            match list_target {
                Some(0) => include_tools.push(item.to_string()),
                Some(1) => exclude_tools.push(item.to_string()),
                Some(2) => include_skills.push(item.to_string()),
                Some(3) => exclude_skills.push(item.to_string()),
                _ => {}
            }
            continue;
        }
        if !trimmed.is_empty() {
            list_target = None;
        }

        if let Some(v) = trimmed.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = trimmed.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        } else if let Some(v) = trimmed.strip_prefix("mode:") {
            mode = Some(match v.trim() {
                "subagent" => AgentMode::Subagent,
                _ => AgentMode::Primary,
            });
        } else if let Some(v) = trimmed.strip_prefix("include_tools:") {
            include_tools = parse_yaml_string_list(v);
            tools_include_seen = true;
            list_target = v.trim().is_empty().then_some(0);
        } else if let Some(v) = trimmed.strip_prefix("exclude_tools:") {
            exclude_tools = parse_yaml_string_list(v);
            _tools_exclude_seen = true;
            list_target = v.trim().is_empty().then_some(1);
        } else if let Some(v) = trimmed.strip_prefix("include_skills:") {
            include_skills = parse_yaml_string_list(v);
            skills_include_seen = true;
            list_target = v.trim().is_empty().then_some(2);
        } else if let Some(v) = trimmed.strip_prefix("exclude_skills:") {
            exclude_skills = parse_yaml_string_list(v);
            _skills_exclude_seen = true;
            list_target = v.trim().is_empty().then_some(3);
        }
    }

    let name = name?;
    let description = description?;

    if name.is_empty() || description.is_empty() {
        return None;
    }

    // Defaults: when a filter field was not explicitly set, default to ["*"] (all).
    // When explicitly set (even to empty list), respect the user's choice.
    if !tools_include_seen {
        include_tools = vec!["*".to_string()];
    }
    if !skills_include_seen {
        include_skills = vec!["*".to_string()];
    }

    let mode = mode.unwrap_or(AgentMode::Primary);

    // Remainder of lines after frontmatter is the system prompt body
    let body: String = lines
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let body = body.trim().to_string();

    let base_dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();

    Some(AgentMeta {
        name,
        description,
        mode,
        include_tools,
        exclude_tools,
        include_skills,
        exclude_skills,
        system_prompt: body,
        agents_md: None, // Set by caller (load_agents_recursive)
        path,
        base_dir,
    })
}

/// Parse a YAML inline string list like `["alpha", "beta"]`.
///
/// Handles both `["a", "b"]` and `[a, b]` forms.  Returns an empty vec on
/// parse failure; the caller applies the `["*"]` default.
fn parse_yaml_string_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let inner = if let Some(s) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        s
    } else {
        return Vec::new();
    };

    let mut items = Vec::new();
    for part in inner.split(',') {
        let cleaned = part.trim().trim_matches('"').trim_matches('\'');
        if !cleaned.is_empty() {
            items.push(cleaned.to_string());
        }
    }
    items
}

// ── Glob helpers ──────────────────────────────────────────────────────────────

fn build_glob_set(patterns: &[String]) -> Option<GlobSet> {
    if patterns.is_empty() || (patterns.len() == 1 && patterns[0] == "*") {
        return None;
    }

    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        if let Ok(glob) = Glob::new(pat) {
            builder.add(glob);
        }
    }
    builder.build().ok()
}

/// Check if `name` matches any pattern in `patterns`.
/// `*` is treated as "match all".
fn matches_any(name: &str, glob_set: &Option<GlobSet>, patterns: &[String]) -> bool {
    // If patterns contain "*", everything matches
    if patterns.iter().any(|p| p == "*") {
        return true;
    }
    match glob_set {
        Some(set) => set.is_match(name),
        None => true, // no patterns = match all (same as ["*"])
    }
}

// ── Filtering ─────────────────────────────────────────────────────────────────

/// Filter a tool registry according to agent include/exclude rules.
///
/// Always-present tools are re-added regardless of filter settings.
pub fn filter_tools(tools: &ToolRegistry, include: &[String], exclude: &[String]) -> ToolRegistry {
    let include_globs = build_glob_set(include);
    let exclude_globs = build_glob_set(exclude);

    let mut filtered = ToolRegistry::new();

    for (name, tool) in tools {
        // Skip tools not matching include patterns
        if !matches_any(name, &include_globs, include) {
            continue;
        }
        // Skip tools matching exclude patterns (empty exclude = exclude nothing)
        if !exclude.is_empty() && matches_any(name, &exclude_globs, exclude) {
            continue;
        }
        filtered.insert(name.clone(), tool.clone());
    }

    // Re-add always-present tools
    for name in ALWAYS_PRESENT_TOOLS {
        if !filtered.contains_key(*name)
            && let Some(tool) = tools.get(*name)
        {
            filtered.insert((*name).to_string(), tool.clone());
        }
    }

    filtered
}

/// Select the active skill set, then resolve name collisions by the narrowest
/// active scope. Names are opaque; scope comes only from the source directory.
pub fn select_skills(skills: &[SkillMeta], agent: Option<&AgentMeta>) -> Vec<SkillMeta> {
    let selected_name = agent.map(|a| a.name.as_str()).unwrap_or("default");
    let is_specialized = selected_name != "default";
    let include = agent.map(|a| a.include_skills.as_slice()).unwrap_or(&[]);
    let exclude = agent.map(|a| a.exclude_skills.as_slice()).unwrap_or(&[]);
    let include_globs = build_glob_set(include);
    let exclude_globs = build_glob_set(exclude);
    let include_scoped_skill = |name: &str| {
        !is_specialized || (!include.is_empty() && matches_any(name, &include_globs, include))
    };

    let mut active: Vec<&SkillMeta> = skills
        .iter()
        .filter(|skill| match &skill.scope {
            SkillScope::Standard => {
                !is_specialized
                    || exclude.is_empty()
                    || !matches_any(&skill.name, &exclude_globs, exclude)
            }
            SkillScope::Agent(scope_name) if scope_name == "default" && !is_specialized => true,
            SkillScope::Agent(scope_name) if scope_name == selected_name => {
                include_scoped_skill(&skill.name)
                    && (exclude.is_empty() || !matches_any(&skill.name, &exclude_globs, exclude))
            }
            SkillScope::Agent(scope_name) if scope_name == "default" => {
                include_scoped_skill(&skill.name)
                    && (exclude.is_empty() || !matches_any(&skill.name, &exclude_globs, exclude))
            }
            SkillScope::Agent(_) => false,
        })
        .collect();

    // The narrower active source wins: selected agent, then default agent,
    // then standard roots. Filtering precedes this collapse so inactive skills
    // never shadow an available broader-scope definition.
    active.sort_by(|a, b| {
        let rank = |skill: &SkillMeta| match &skill.scope {
            SkillScope::Standard => 0,
            SkillScope::Agent(name) if name == selected_name => 2,
            SkillScope::Agent(_) => 1,
        };
        a.name.cmp(&b.name).then_with(|| rank(b).cmp(&rank(a)))
    });
    let mut names = HashSet::new();
    active
        .into_iter()
        .filter(|skill| names.insert(skill.name.clone()))
        .cloned()
        .collect()
}

// ── Resolve ───────────────────────────────────────────────────────────────────

/// Find an agent by name in a list. Returns `None` when `name` is empty
/// (representing "default / no agent").
#[allow(dead_code)] // Used by planned subagent orchestration.
pub fn resolve_agent<'a>(agents: &'a [AgentMeta], name: &str) -> Option<&'a AgentMeta> {
    if name.is_empty() {
        return None;
    }
    agents.iter().find(|agent| agent.name == name)
}

pub fn select_startup_agent(
    agents: &[AgentMeta],
    cli_name: Option<&str>,
    configured_name: Option<&str>,
) -> io::Result<Option<AgentMeta>> {
    if let Some(name) = cli_name {
        return agents
            .iter()
            .find(|agent| agent.name == name)
            .cloned()
            .map(Some)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown agent '{name}'; use a discovered agent profile name"),
                )
            });
    }

    Ok(configured_name
        .and_then(|name| agents.iter().find(|agent| agent.name == name))
        .cloned())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::{io::Write, sync::Arc};

    use super::*;
    use crate::agent::types::Tool;

    // ── Test helpers ──────────────────────────────────────────────────────────

    struct StubTool {
        name: &'static str,
    }

    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.name
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn run(
            &self,
            _args: serde_json::Value,
            _ctx: crate::agent::types::ToolCallContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = crate::agent::types::ToolResult> + Send + '_>,
        > {
            Box::pin(async { crate::agent::types::ToolResult::ok_str("ok") })
        }
    }

    fn test_registry(names: &[&'static str]) -> ToolRegistry {
        let mut r = ToolRegistry::new();
        for name in names {
            r.insert((*name).to_string(), Arc::new(StubTool { name }));
        }
        r
    }

    fn test_skills(names: &[&str]) -> Vec<SkillMeta> {
        names
            .iter()
            .map(|n| SkillMeta {
                scope: SkillScope::Standard,
                name: n.to_string(),
                description: format!("desc: {n}"),
                path: PathBuf::from(format!("/tmp/skills/{n}/SKILL.md")),
                base_dir: PathBuf::from(format!("/tmp/skills/{n}")),
                embedded_body: None,
            })
            .collect()
    }

    // ── Startup selection tests ───────────────────────────────────────────────

    fn startup_agents() -> Vec<AgentMeta> {
        ["configured", "cli-choice"]
            .into_iter()
            .map(|name| {
                parse_agent_meta(
                    &format!("---\nname: {name}\ndescription: test\n---\nbody"),
                    PathBuf::from(format!("/tmp/agents/{name}/SYSTEM.md")),
                )
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn startup_agent_cli_name_overrides_configured_name() {
        let agents = startup_agents();
        let selected = select_startup_agent(&agents, Some("cli-choice"), Some("configured"))
            .unwrap()
            .unwrap();
        assert_eq!(selected.name, "cli-choice");
    }

    #[test]
    fn startup_agent_uses_configured_name_when_cli_is_absent() {
        let agents = startup_agents();
        let selected = select_startup_agent(&agents, None, Some("configured"))
            .unwrap()
            .unwrap();
        assert_eq!(selected.name, "configured");
    }

    #[test]
    fn startup_agent_rejects_unknown_cli_name() {
        let agents = startup_agents();
        let error = select_startup_agent(&agents, Some("missing"), Some("configured")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("missing"));
    }

    #[test]
    fn startup_agent_ignores_unknown_configured_name_for_compatibility() {
        let agents = startup_agents();
        assert!(
            select_startup_agent(&agents, None, Some("missing"))
                .unwrap()
                .is_none()
        );
    }

    // ── Parsing tests ─────────────────────────────────────────────────────────

    #[test]
    fn parse_minimal_agent() {
        let content = "\
---
name: test-agent
description: a test agent
---

You are a test agent.
";
        let meta = parse_agent_meta(content, PathBuf::from("/tmp/agents/test-agent/AGENT.md"))
            .expect("should parse");
        assert_eq!(meta.name, "test-agent");
        assert_eq!(meta.description, "a test agent");
        assert_eq!(meta.mode, AgentMode::Primary);
        assert_eq!(meta.include_tools, vec!["*"]);
        assert!(meta.exclude_tools.is_empty());
        assert_eq!(meta.include_skills, vec!["*"]);
        assert!(meta.exclude_skills.is_empty());
        assert_eq!(meta.system_prompt, "You are a test agent.");
    }

    #[test]
    fn parse_agent_with_filters() {
        let content = "\
---
name: restricted
description: restricted agent
include_tools: [\"read_file\", \"find_files\"]
exclude_tools: [\"bash\"]
include_skills: [\"workflow\"]
exclude_skills: []
---

Restricted.
";
        let meta = parse_agent_meta(content, PathBuf::from("/tmp/agents/restricted/AGENT.md"))
            .expect("should parse");
        assert_eq!(meta.include_tools, vec!["read_file", "find_files"]);
        assert_eq!(meta.exclude_tools, vec!["bash"]);
        assert_eq!(meta.include_skills, vec!["workflow"]);
        assert!(meta.exclude_skills.is_empty());
    }

    #[test]
    fn parse_agent_with_subagent_mode() {
        let content = "\
---
name: helper
description: a subagent
mode: subagent
---

I help.
";
        let meta = parse_agent_meta(content, PathBuf::from("/tmp/agents/helper/AGENT.md"))
            .expect("should parse");
        assert_eq!(meta.mode, AgentMode::Subagent);
    }

    #[test]
    fn parse_rejects_missing_name() {
        let content = "\
---
description: no name
---

body
";
        assert!(parse_agent_meta(content, PathBuf::from("/tmp/agents/bad/AGENT.md")).is_none());
    }

    #[test]
    fn parse_rejects_missing_description() {
        let content = "\
---
name: no-desc
---

body
";
        assert!(parse_agent_meta(content, PathBuf::from("/tmp/agents/bad/AGENT.md")).is_none());
    }

    #[test]
    fn parse_rejects_no_frontmatter() {
        let content = "Just some markdown, no frontmatter";
        assert!(parse_agent_meta(content, PathBuf::from("/tmp/agents/bad/AGENT.md")).is_none());
    }

    #[test]
    fn parse_respects_explicit_empty_include_skills() {
        let content = "\
---
name: no-skills
description: no skills agent
include_skills: []
---

No skills here.
";
        let meta = parse_agent_meta(content, PathBuf::from("/tmp/agents/no-skills/AGENT.md"))
            .expect("should parse");
        assert!(
            meta.include_skills.is_empty(),
            "expected empty, got {:?}",
            meta.include_skills
        );
        // include_tools was not set, so defaults to ["*"]
        assert_eq!(meta.include_tools, vec!["*"]);
    }

    // ── Tool filtering tests ──────────────────────────────────────────────────

    #[test]
    fn filter_tools_include_all_by_default() {
        let tools = test_registry(&["bash", "read_file", "write_file"]);
        let filtered = filter_tools(&tools, &["*".into()], &[]);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn filter_tools_include_subset() {
        let tools = test_registry(&["bash", "read_file", "write_file", "ask_user", "read_skill"]);
        let filtered = filter_tools(&tools, &["read_file".into(), "find_files".into()], &[]);
        // read_file matches, find_files not in registry, always-present not excluded
        assert!(filtered.contains_key("read_file"));
        assert!(!filtered.contains_key("bash"));
        assert!(!filtered.contains_key("write_file"));
        // Always-present tools survive
        assert!(filtered.contains_key("ask_user"));
        assert!(filtered.contains_key("read_skill"));
    }

    #[test]
    fn filter_tools_exclude_overrides_include() {
        let tools = test_registry(&["bash", "read_file"]);
        let filtered = filter_tools(&tools, &["*".into()], &["bash".into()]);
        assert!(filtered.contains_key("read_file"));
        assert!(!filtered.contains_key("bash"));
    }

    #[test]
    fn filter_tools_always_present_immune_to_exclude() {
        let tools = test_registry(&["ask_user", "read_skill", "bash"]);
        let filtered = filter_tools(
            &tools,
            &["*".into()],
            &["ask_user".into(), "read_skill".into()],
        );
        // Always-present tools survive even explicit exclusion
        assert!(filtered.contains_key("ask_user"));
        assert!(filtered.contains_key("read_skill"));
        assert!(filtered.contains_key("bash"));
    }

    #[test]
    fn filter_tools_empty_include_means_all() {
        let tools = test_registry(&["bash", "read_file"]);
        let filtered = filter_tools(&tools, &[], &[]);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_tools_glob_pattern() {
        let tools = test_registry(&["bash", "read_file", "write_file", "find_files"]);
        // Include only read_* and find_* tools
        let filtered = filter_tools(&tools, &["read_*".into(), "find_*".into()], &[]);
        assert!(filtered.contains_key("read_file"));
        assert!(filtered.contains_key("find_files"));
        assert!(!filtered.contains_key("bash"));
        assert!(!filtered.contains_key("write_file"));
    }

    // ── Scoped skill selection tests ────────────��─────────────────────────────

    fn scoped_skill(name: &str, scope: SkillScope) -> SkillMeta {
        let mut skill = test_skills(&[name]).remove(0);
        skill.scope = scope;
        skill
    }

    fn test_agent(name: &str, include: &[&str], exclude: &[&str]) -> AgentMeta {
        let mut agent = parse_agent_meta(
            &format!("---\nname: {name}\ndescription: test\n---\n"),
            PathBuf::from(format!("/tmp/agents/{name}/SYSTEM.md")),
        )
        .unwrap();
        agent.include_skills = include.iter().map(|s| s.to_string()).collect();
        agent.exclude_skills = exclude.iter().map(|s| s.to_string()).collect();
        agent
    }

    #[test]
    fn specialized_agent_keeps_standard_skills_unless_explicitly_excluded() {
        let skills = vec![
            scoped_skill("alpha", SkillScope::Standard),
            scoped_skill("beta", SkillScope::Standard),
        ];
        let agent = test_agent("specialist", &["only-this"], &["beta"]);
        let selected = select_skills(&skills, Some(&agent));
        let names: Vec<_> = selected.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["alpha"]);
    }

    #[test]
    fn specialized_whitelist_masks_default_skills_but_adds_selected_agent_skills() {
        let skills = vec![
            scoped_skill("core", SkillScope::Standard),
            scoped_skill("default-extra", SkillScope::Agent("default".into())),
            scoped_skill("special-extra", SkillScope::Agent("specialist".into())),
        ];
        let agent = test_agent("specialist", &["special-extra"], &[]);
        let selected = select_skills(&skills, Some(&agent));
        let names: Vec<_> = selected.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["core", "special-extra"]);
    }

    #[test]
    fn default_agent_gets_standard_and_default_scoped_skills_only() {
        let skills = vec![
            scoped_skill("core", SkillScope::Standard),
            scoped_skill("default-extra", SkillScope::Agent("default".into())),
            scoped_skill("special-extra", SkillScope::Agent("specialist".into())),
        ];
        let selected = select_skills(&skills, None);
        let names: Vec<_> = selected.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["core", "default-extra"]);
    }

    #[test]
    fn narrowest_active_skill_wins_collision_and_names_have_no_scope_meaning() {
        let skills = vec![
            scoped_skill("default", SkillScope::Standard),
            scoped_skill("default", SkillScope::Agent("default".into())),
            scoped_skill("default", SkillScope::Agent("specialist".into())),
        ];
        let agent = test_agent("specialist", &["default"], &[]);
        let selected = select_skills(&skills, Some(&agent));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].scope, SkillScope::Agent("specialist".into()));

        let no_whitelist_match = test_agent("specialist", &["other"], &[]);
        let selected = select_skills(&skills, Some(&no_whitelist_match));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].scope, SkillScope::Standard);
    }

    #[test]
    fn block_style_skill_lists_are_parsed() {
        let agent = parse_agent_meta(
            "---\nname: specialist\ndescription: test\ninclude_skills:\n  - alpha\nexclude_skills:\n  - beta\n---\n",
            PathBuf::from("/tmp/agents/specialist/SYSTEM.md"),
        )
        .unwrap();
        assert_eq!(agent.include_skills, ["alpha"]);
        assert_eq!(agent.exclude_skills, ["beta"]);
    }

    // ── Discovery tests ───────────────────────────────────────────────────────

    #[test]
    fn load_agents_from_dir_discovers_agent_files() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("my-agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let mut f = std::fs::File::create(agent_dir.join("AGENT.md")).unwrap();
        writeln!(
            f,
            "---\nname: my-agent\ndescription: test agent\n---\n\nSystem prompt."
        )
        .unwrap();

        let mut visited = HashSet::new();
        let agents = load_agents_from_dir(dir.path(), &mut visited);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "my-agent");
    }

    #[test]
    fn load_agents_skips_non_matching_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file at root — should be ignored
        std::fs::write(
            dir.path().join("not-an-agent.md"),
            "---\nname: x\ndescription: y\n---\n",
        )
        .unwrap();
        let mut visited = HashSet::new();
        let agents = load_agents_from_dir(dir.path(), &mut visited);
        assert!(agents.is_empty());
    }

    #[test]
    fn resolve_agent_finds_by_name() {
        let agents = vec![AgentMeta {
            name: "alpha".into(),
            description: "a".into(),
            mode: AgentMode::Primary,
            include_tools: vec![],
            exclude_tools: vec![],
            include_skills: vec![],
            exclude_skills: vec![],
            system_prompt: String::new(),
            agents_md: None,
            path: PathBuf::from("/x"),
            base_dir: PathBuf::from("/x"),
        }];
        assert!(resolve_agent(&agents, "alpha").is_some());
        assert!(resolve_agent(&agents, "beta").is_none());
        assert!(resolve_agent(&agents, "").is_none());
    }
}
