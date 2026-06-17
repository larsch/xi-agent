use chrono::Local;
use directories::BaseDirs;
use std::{fs, path::Path};

use crate::agent::types::ToolRegistry;
use crate::skills::SkillMeta;

/// Helper to read and combine AGENTS.md content.
pub fn read_agents_md(cwd: &str, test_home: Option<&Path>) -> String {
    let mut content = String::new();

    // Check for ~/.xi/AGENTS.md
    let home_dir_buf = test_home
        .map(|p| p.to_path_buf())
        .or_else(|| BaseDirs::new().map(|bd| bd.home_dir().to_path_buf()));
    if let Some(home_dir) = home_dir_buf.as_deref() {
        let global_agents_md = home_dir.join(".xi/AGENTS.md");
        if global_agents_md.exists()
            && let Ok(file_content) = fs::read_to_string(&global_agents_md)
        {
            content.push_str(&file_content);
            content.push('\n');
        }
    }

    // Check cwd and its parent directories for AGENTS.md
    let mut current_dir = Path::new(cwd);
    loop {
        let agents_md_path = current_dir.join("AGENTS.md");
        if agents_md_path.exists()
            && let Ok(file_content) = fs::read_to_string(&agents_md_path)
        {
            content.push_str(&file_content);
            content.push('\n');
        }

        match current_dir.parent() {
            Some(parent) if parent != current_dir => current_dir = parent,
            _ => break,
        }
    }

    content
}

/// Build the default system prompt for the agent loop.
///
/// Structure mirrors pi-mono's `buildSystemPrompt`: identity, tool list,
/// tool-aware guidelines, project context (AGENTS.md), skills, then date/cwd.
pub fn build_system_prompt(tools: &ToolRegistry, cwd: &str, skills: &[SkillMeta]) -> String {
    let date = Local::now().format("%Y-%m-%d").to_string();

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
        guidelines.push("Use relative paths for files within the current working directory hierarchy.".to_string());
    }
    if has("edit_file") || has("write_file") {
        guidelines.push("When summarizing your actions, output plain text directly — do NOT use bash or cat to display what you did.".to_string());
    }
    if has("ask_user") {
        guidelines.push("Use ask_user only when the task requires a user decision or information you cannot infer.".to_string());
        guidelines.push("Before calling ask_user, gather relevant context with your other tools and include a short summary in the context field.".to_string());
    }
    guidelines.push("Never describe a change as done or claim to have implemented something unless you have called the appropriate tools in this response to make that change. If you intend to make edits, call the tools now.".to_string());
    guidelines.push("Be concise in your responses.".to_string());
    guidelines.push("Show file paths clearly when working with files.".to_string());

    let guidelines_text = guidelines
        .iter()
        .map(|g| format!("- {g}"))
        .collect::<Vec<_>>()
        .join("\n");

    // AGENTS.md content rendered as a labelled project context section.
    let agents_md_content = read_agents_md(cwd, None);
    let project_context_section = if agents_md_content.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\n# Project Context\n\nProject-specific instructions and guidelines:\n\n{agents_md_content}"
        )
    };

    let skills_section = render_skills_block(skills);

    format!(
        "You are an assistant that helps the user perform applied interactive computational work \
in real systems. This includes building software, as well as running commands and \
code, inspecting and modifying files, debugging issues, and performing data and system \
manipulation workflows.\n\
\n\
Available tools:\n\
{tool_list}\n\
\n\
In addition to the tools above, you may have access to other custom tools depending on the project.\n\
\n\
File paths are relative to the current working directory.\n\
\n\
Guidelines:\n\
{guidelines_text}{project_context_section}{skills_section}\n\
Current date: {date}\n\
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

        let prompt = build_system_prompt(&tools, "/tmp", &[]);

        assert!(prompt.contains("Prefer find_files over bash for filesystem exploration."));
        assert!(prompt.contains("Use read_file to examine files before editing."));
        assert!(prompt.contains("Use edit_file for precise changes"));
        assert!(prompt.contains("Use write_file only for new files or complete rewrites."));
        assert!(prompt.contains("Use ask_user only when the task requires a user decision"));
        assert!(prompt.contains("- ask_user: Ask a question."));
        assert!(prompt.contains("- bash: Run shell commands."));
        assert!(prompt.contains("Prefer exec over bash when arguments contain spaces"));
    }

    #[test]
    fn build_system_prompt_switches_bash_guideline_when_find_is_missing() {
        let tools = registry(&[("bash", "Run shell commands.")]);
        let prompt = build_system_prompt(&tools, "/tmp", &[]);

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

        let prompt = build_system_prompt(&tools, "/tmp", &skills);

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

        let prompt = build_system_prompt(&tools, "/tmp", &skills);

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
}
