use directories::BaseDirs;
use std::{fs, path::Path};

use crate::agent::types::ToolRegistry;
use crate::agents::AgentMeta;
use crate::skills::SkillMeta;

/// A sourced AGENTS.md entry with its file path and origin.
pub struct AgentsEntry {
    /// Category: user-home or working-directory chain.
    pub kind: AgentsKind,
    /// Path to the file that was read.
    pub path: std::path::PathBuf,
    /// File contents.
    pub content: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentsKind {
    /// From an agent's AGENTS.md — replaces the global entry.
    Agent,
    /// From the home directory — user-level, applies everywhere.
    Global,
    /// From the cwd-to-root chain — local to this working directory.
    Local,
}

/// Read the first existing AGENTS.md candidate from a base directory.
///
/// Priority: `.xi/AGENTS.md` → `.agents/AGENTS.md` → `AGENTS.md`.
/// Returns `Some((path, content))` for the first match, or `None` if none exist.
fn read_directory_agents(base: &Path) -> Option<(std::path::PathBuf, String)> {
    let candidates = [
        base.join(".xi/AGENTS.md"),
        base.join(".agents/AGENTS.md"),
        base.join("AGENTS.md"),
    ];
    for candidate in &candidates {
        if candidate.exists()
            && let Ok(content) = fs::read_to_string(candidate)
        {
            return Some((candidate.clone(), content));
        }
    }
    None
}

/// Collect AGENTS.md entries from home and the cwd→root chain.
///
/// When `agent_agents_md` is provided (content of an agent's `AGENTS.md`),
/// it replaces the global home-directory entry.  The local chain (cwd→root)
/// is always included.
///
/// Returns an ordered list: agent entry first (if any), then global entry
/// (if any and no agent override), then local entries from cwd up to root.
pub fn read_agents_md(
    cwd: &str,
    test_home: Option<&Path>,
    agent_agents_md: Option<&str>,
) -> Vec<AgentsEntry> {
    let mut entries: Vec<AgentsEntry> = Vec::new();

    if let Some(content) = agent_agents_md {
        entries.push(AgentsEntry {
            kind: AgentsKind::Agent,
            path: std::path::PathBuf::from("(agent AGENTS.md)"),
            content: content.to_string(),
        });
    } else {
        // Global: one file from home directory (only when no agent override).
        let home_dir_buf = test_home
            .map(|p| p.to_path_buf())
            .or_else(|| BaseDirs::new().map(|bd| bd.home_dir().to_path_buf()));
        if let Some(home_dir) = home_dir_buf.as_deref()
            && let Some((path, content)) = read_directory_agents(home_dir)
        {
            entries.push(AgentsEntry {
                kind: AgentsKind::Global,
                path,
                content,
            });
        }
    }

    // Walk cwd → root, one file per directory level.
    let mut current_dir = Path::new(cwd).to_path_buf();
    loop {
        if let Some((path, content)) = read_directory_agents(&current_dir) {
            entries.push(AgentsEntry {
                kind: AgentsKind::Local,
                path,
                content,
            });
        }

        match current_dir.parent() {
            Some(parent) if parent != current_dir => current_dir = parent.to_path_buf(),
            _ => break,
        }
    }

    entries
}

/// Render collected AGENTS.md entries into a system prompt section.
fn render_agents_section(entries: &[AgentsEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut section = String::from("\n\n# AGENTS.md\n\n");
    section.push_str("The following was read from AGENTS.md files and is already available to you — you do not need to read them again.\n\n");

    for entry in entries {
        let label = match entry.kind {
            AgentsKind::Agent => "Agent Instructions",
            AgentsKind::Global => "Global Instructions",
            AgentsKind::Local => "Local Instructions",
        };
        let path_display = entry.path.display();
        section.push_str(&format!("## {label} ({path_display})\n\n"));
        section.push_str(&entry.content);
        section.push('\n');
    }

    section
}

/// Build the default system prompt for the agent loop.
///
/// When `agent` is `Some`, the agent's body replaces the default identity
/// paragraph and the caller is expected to pass pre-filtered `tools` and
/// `skills`.  When `None`, the default identity is used.
///
/// Structure: identity, tool list, tool-aware guidelines, project context
/// (AGENTS.md), skills, then cwd.
pub fn build_system_prompt(
    tools: &ToolRegistry,
    cwd: &str,
    skills: &[SkillMeta],
    agent: Option<&AgentMeta>,
) -> String {
    // Build tool list sorted by name for deterministic output.
    let mut tool_names: Vec<&str> = tools.keys().map(String::as_str).collect();
    tool_names.sort_unstable();

    let tool_list = if tool_names.is_empty() {
        "(none)".to_string()
    } else {
        tool_names
            .iter()
            .map(|name| {
                let desc = tools.get(*name).map(|t| t.description()).unwrap_or(*name);
                // Trim the description to its first sentence for brevity.
                let short = first_sentence(desc);
                format!("- {name}: {short}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Build guidelines conditioned on which tools are present.
    let has = |name: &str| tool_names.contains(&name);
    let mut guidelines: Vec<String> = Vec::new();

    // File exploration: prefer dedicated tools over bash when both are available.
    if has("bash") && !has("find_files") {
        guidelines.push("Use bash for file operations like ls, find, grep.".to_string());
    } else if has("bash") && has("find_files") {
        guidelines.push("Prefer find_files over bash for filesystem exploration.".to_string());
    }

    // argv-style execution: prefer exec over bash when args contain fragile characters.
    if has("exec") && has("bash") {
        guidelines.push(
            "Prefer exec over bash when arguments contain spaces, backticks, quotes, \
             dollar signs, newlines, or other characters that are fragile under shell parsing."
                .to_string(),
        );
    }

    if has("read_file") && has("edit_file") {
        guidelines.push("Use read_file to examine files before editing. You must use this tool instead of cat or sed.".to_string());
    }
    if has("edit_file") {
        guidelines
            .push("Use edit_file for precise changes — old_text must match exactly.".to_string());
    }
    if has("write_file") {
        guidelines.push("Use write_file only for new files or complete rewrites.".to_string());
    }
    if has("read_file") || has("write_file") || has("edit_file") {
        guidelines.push(
            "Use relative paths for files within the current working directory hierarchy."
                .to_string(),
        );
    }
    if has("edit_file") || has("write_file") {
        guidelines.push("When summarizing your actions, output plain text directly — do NOT use bash or cat to display what you did.".to_string());
    }
    if has("ask_user") {
        guidelines.push("Use ask_user only when the task requires a user decision or information you cannot infer.".to_string());
        guidelines.push("Before calling ask_user, gather relevant context with your other tools and include a short summary in the context field.".to_string());
    }
    guidelines.push("For rich or structured writes, create a UTF-8 no-BOM payload file and pass it through a file/stdin option rather than embedding the payload in PowerShell or cmd command arguments. Retrieve and verify the stored result after every such write.".to_string());
    guidelines.push("Never describe a change as done or claim to have implemented something unless you have called the appropriate tools in this response to make that change. If you intend to make edits, call the tools now.".to_string());
    guidelines.push("Be concise in your responses.".to_string());
    guidelines.push("Show file paths clearly when working with files.".to_string());

    let guidelines_text = guidelines
        .iter()
        .map(|g| format!("- {g}"))
        .collect::<Vec<_>>()
        .join("\n");

    // AGENTS.md content, with global and local entries labelled by source path.
    let agent_agents_md = agent.and_then(|a| a.agents_md.as_deref());
    let agents_entries = read_agents_md(cwd, None, agent_agents_md);
    let agents_section = render_agents_section(&agents_entries);

    let skills_section = render_skills_block(skills);

    let identity = if let Some(agent) = agent
        && !agent.system_prompt.is_empty()
    {
        agent.system_prompt.clone()
    } else {
        "You are a multi-purpose assistant for computational systems, spanning data and code analysis, data processing, software development, and interaction with system environments. You interpret user intent and respond through explanation, analysis, review, suggestions, or actions."
            .to_string()
    };

    format!(
        "{identity}\n\
\n\
Available tools:\n\
{tool_list}\n\
\n\
In addition to the tools above, you may have access to other custom tools depending on the project.\n\
\n\
File paths are relative to the current working directory.\n\
\n\
Guidelines:\n\
{guidelines_text}{agents_section}{skills_section}
Current working directory: {cwd}"
    )
}

/// Render the available skills block from a slice of skill metadata.
/// Returns an empty string when `skills` is empty.
fn render_skills_block(skills: &[SkillMeta]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let entries: String = skills
        .iter()
        .map(|s| format!("- `{}`: {}", s.name, s.description))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "\n\
\nThe following skills provide specialized instructions for specific tasks.\n\
Each skill's description names the task type, problem domain, or situation it handles. \
Load any skill whose description overlaps with what the user is asking about. \
When multiple descriptions are relevant, load all of them.\n\
Use the read_skill tool to load a skill's instructions by name.\n\
When a skill file references a relative path, resolve it against the skill directory \
(parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.\n\
\n<available_skills>\n{entries}\n</available_skills>"
    )
}

/// Return the text up to and including the first `.`, `!`, or `?`,
/// or the whole string if none is found. Strips leading/trailing whitespace.
fn first_sentence(s: &str) -> &str {
    let end = s
        .char_indices()
        .find(|(_, c)| matches!(c, '.' | '!' | '?'))
        .map(|(i, _)| i + 1)
        .unwrap_or(s.len());
    s[..end].trim()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use super::{build_system_prompt, first_sentence};
    use crate::{
        agent::types::{Tool, ToolRegistry},
        agents::AgentMeta,
        skills::SkillMeta,
    };

    struct TestTool {
        name: &'static str,
        desc: &'static str,
    }

    impl Tool for TestTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            self.desc
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

    fn registry(tool_defs: &[(&'static str, &'static str)]) -> ToolRegistry {
        let mut tools: ToolRegistry = HashMap::new();
        for (name, desc) in tool_defs {
            tools.insert(
                (*name).to_string(),
                Arc::new(TestTool { name, desc }) as Arc<dyn Tool>,
            );
        }
        tools
    }

    #[test]
    fn first_sentence_handles_punctuation_and_trim() {
        assert_eq!(first_sentence("  Hello world. More text"), "Hello world.");
        assert_eq!(first_sentence("What now? Later"), "What now?");
        assert_eq!(first_sentence("No punctuation"), "No punctuation");
    }

    #[test]
    fn build_system_prompt_includes_tool_specific_guidelines() {
        let tools = registry(&[
            ("bash", "Run shell commands. With side effects."),
            (
                "exec",
                "Execute a program directly with an argv-style argument list.",
            ),
            ("find_files", "Find files recursively."),
            ("read_file", "Read files."),
            ("edit_file", "Edit files."),
            ("write_file", "Write files."),
            ("ask_user", "Ask a question."),
        ]);

        let prompt = build_system_prompt(&tools, "/tmp", &[], None);

        assert!(prompt.contains("Prefer find_files over bash for filesystem exploration."));
        assert!(prompt.contains("Use read_file to examine files before editing."));
        assert!(prompt.contains("Use edit_file for precise changes"));
        assert!(prompt.contains("Use write_file only for new files or complete rewrites."));
        assert!(prompt.contains("Use ask_user only when the task requires a user decision"));
        assert!(prompt.contains("- ask_user: Ask a question."));
        assert!(prompt.contains("- bash: Run shell commands."));
        assert!(prompt.contains("Prefer exec over bash when arguments contain spaces"));
        assert!(prompt.contains("create a UTF-8 no-BOM payload file"));
    }

    #[test]
    fn build_system_prompt_switches_bash_guideline_when_find_is_missing() {
        let tools = registry(&[("bash", "Run shell commands.")]);
        let prompt = build_system_prompt(&tools, "/tmp", &[], None);

        assert!(prompt.contains("Use bash for file operations like ls, find, grep."));
        assert!(!prompt.contains("Prefer find_files over bash for filesystem exploration."));
    }

    #[test]
    fn build_system_prompt_renders_skills_block() {
        let tools = registry(&[]);
        let skills = vec![SkillMeta {
            name: "plan".to_string(),
            description: "Create an implementation plan".to_string(),
            path: Path::new("/tmp/skills/plan/SKILL.md").to_path_buf(),
            base_dir: PathBuf::from("/tmp/skills/plan"),
            embedded_body: None,
        }];

        let prompt = build_system_prompt(&tools, "/tmp", &skills, None);

        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("- `plan`: Create an implementation plan"));
        assert!(
            !prompt.contains("<name>plan</name>"),
            "should not contain XML tags"
        );
        assert!(
            !prompt.contains("/tmp/skills/plan/SKILL.md"),
            "location should not appear in listing"
        );
        assert!(prompt.contains(
            "Each skill's description names the task type, problem domain, or situation it handles."
        ));
        assert!(prompt.contains(
            "Load any skill whose description overlaps with what the user is asking about."
        ));
        assert!(prompt.contains("When multiple descriptions are relevant, load all of them."));
        assert!(prompt.contains("Use the read_skill tool to load a skill's instructions by name."));
    }

    #[test]
    fn build_system_prompt_skill_special_chars_not_escaped() {
        let tools = registry(&[]);
        let skills = vec![SkillMeta {
            name: "a-skill".to_string(),
            description: "handles x < y and \"quoted\" values".to_string(),
            path: Path::new("/tmp/skills/a-skill/SKILL.md").to_path_buf(),
            base_dir: PathBuf::from("/tmp/skills/a-skill"),
            embedded_body: None,
        }];

        let prompt = build_system_prompt(&tools, "/tmp", &skills, None);

        // Prose format — no XML escaping applied
        assert!(
            prompt.contains("x < y"),
            "angle bracket should not be escaped"
        );
        assert!(
            prompt.contains("\"quoted\""),
            "quotes should not be escaped"
        );
    }

    #[test]
    fn build_system_prompt_with_agent_uses_agent_body_as_identity() {
        let tools = registry(&[]);
        let skills = vec![SkillMeta {
            name: "workflow".to_string(),
            description: "structured workflow".to_string(),
            path: Path::new("/tmp/skills/workflow/SKILL.md").to_path_buf(),
            base_dir: PathBuf::from("/tmp/skills/workflow"),
            embedded_body: None,
        }];
        let agent = AgentMeta {
            name: "test-agent".into(),
            description: "test".into(),
            mode: crate::agents::AgentMode::Primary,
            include_tools: vec!["*".to_string()],
            exclude_tools: vec![],
            include_skills: vec!["*".to_string()],
            exclude_skills: vec![],
            system_prompt: "You are a test-only agent.".into(),
            agents_md: None,
            path: PathBuf::from("/tmp/agents/test/AGENT.md"),
            base_dir: PathBuf::from("/tmp/agents/test"),
        };

        let prompt = build_system_prompt(&tools, "/tmp", &skills, Some(&agent));

        assert!(
            prompt.starts_with("You are a test-only agent."),
            "should start with agent body: {prompt}"
        );
        assert!(
            !prompt.contains("multi-purpose assistant"),
            "should not contain default identity: {prompt}"
        );
        // Environmental context still present
        assert!(prompt.contains("Available tools"), "{prompt}");
        assert!(
            prompt.contains("Current working directory: /tmp"),
            "{prompt}"
        );
    }

    #[test]
    fn build_system_prompt_with_empty_agent_body_falls_back_to_default() {
        let tools = registry(&[]);
        let agent = AgentMeta {
            name: "minimal".into(),
            description: "min".into(),
            mode: crate::agents::AgentMode::Primary,
            include_tools: vec!["*".to_string()],
            exclude_tools: vec![],
            include_skills: vec!["*".to_string()],
            exclude_skills: vec![],
            system_prompt: String::new(),
            agents_md: None,
            path: PathBuf::from("/tmp/agents/min/AGENT.md"),
            base_dir: PathBuf::from("/tmp/agents/min"),
        };

        let prompt = build_system_prompt(&tools, "/tmp", &[], Some(&agent));

        assert!(
            prompt.starts_with("You are a multi-purpose assistant"),
            "should fall back to default identity: {prompt}"
        );
    }
}
