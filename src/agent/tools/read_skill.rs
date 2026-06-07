use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::agent::types::{Tool, ToolCallContext, ToolResult};
use crate::skills::SkillMeta;

/// A built-in tool that loads a skill's body by name.
///
/// Takes the skill name (as listed in the available-skills block) and returns
/// the SKILL.md body with frontmatter stripped. Errors if the name is not
/// found in the loaded skill registry.
pub struct ReadSkillTool {
    skills: Arc<Vec<SkillMeta>>,
}

impl ReadSkillTool {
    pub fn new(skills: Arc<Vec<SkillMeta>>) -> Self {
        Self { skills }
    }
}

#[derive(serde::Deserialize)]
struct ReadSkillArgs {
    name: String,
}

impl Tool for ReadSkillTool {
    fn name(&self) -> &str {
        "read_skill"
    }

    fn description(&self) -> &str {
        "Load a skill's instructions by name. Use the skill name exactly as listed in the available skills."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill name to load (e.g. \"workflow\", \"build\")"
                }
            },
            "required": ["name"]
        })
    }

    fn streaming_field(&self) -> Option<&'static str> {
        Some("name")
    }

    fn run(
        &self,
        args: Value,
        _ctx: ToolCallContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + '_>> {
        Box::pin(async move {
            let ReadSkillArgs { name } = match super::parse_args(args) {
                Ok(a) => a,
                Err(e) => return *e,
            };

            let skill = self.skills.iter().find(|s| s.name == name);

            let skill = match skill {
                Some(s) => s,
                None => {
                    let available: Vec<&str> =
                        self.skills.iter().map(|s| s.name.as_str()).collect();
                    return ToolResult::err(format!(
                        "skill '{name}' not found. Available skills: {}",
                        available.join(", ")
                    ));
                }
            };

            let content = match std::fs::read_to_string(&skill.path) {
                Ok(c) => c,
                Err(e) => {
                    return ToolResult::err(format!(
                        "Failed to read skill '{}' from {}: {e}",
                        name,
                        skill.path.display()
                    ));
                }
            };

            let body = strip_frontmatter(&content).trim().to_string();
            ToolResult::ok_str(body)
        })
    }
}

/// Strip YAML frontmatter (`---` … `---`) from the start of a skill file.
/// Returns the body text. Returns the original string unchanged if no
/// frontmatter fence is found.
fn strip_frontmatter(content: &str) -> &str {
    let mut pos: usize = 0;
    let mut fence_seen = false;

    for line in content.split('\n') {
        let trimmed = line.trim_end_matches('\r');
        let advance = line.len() + 1;

        if !fence_seen {
            if trimmed == "---" {
                fence_seen = true;
                pos += advance;
                continue;
            } else {
                return content;
            }
        }

        pos += advance;

        if trimmed == "---" {
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
    use super::*;
    use crate::agent::types::Tool;
    use std::io::Write;
    use std::path::PathBuf;

    fn make_tool(skills: Vec<SkillMeta>) -> ReadSkillTool {
        ReadSkillTool::new(Arc::new(skills))
    }

    fn write_skill_file(dir: &std::path::Path, name: &str, body: &str) -> SkillMeta {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "---\nname: {name}\ndescription: test skill\n---\n\n{body}"
        )
        .unwrap();
        SkillMeta {
            name: name.to_string(),
            description: "test skill".to_string(),
            path: path.clone(),
            base_dir: skill_dir,
        }
    }

    #[tokio::test]
    async fn loads_skill_body_strips_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let skill = write_skill_file(dir.path(), "brainstorm", "# Brainstorm\nDo the thing.");
        let tool = make_tool(vec![skill]);

        let result = tool.execute(serde_json::json!({"name": "brainstorm"})).await;
        assert!(!result.is_error, "unexpected error: {}", result.content.as_text());
        let text = result.content.as_text();
        assert!(text.contains("# Brainstorm"), "missing body: {text}");
        assert!(text.contains("Do the thing."), "missing body: {text}");
        assert!(!text.contains("---"), "frontmatter not stripped: {text}");
        assert!(!text.contains("description:"), "frontmatter not stripped: {text}");
    }

    #[tokio::test]
    async fn unknown_skill_name_returns_error_with_available_list() {
        let dir = tempfile::tempdir().unwrap();
        let skill = write_skill_file(dir.path(), "build", "# Build");
        let tool = make_tool(vec![skill]);

        let result = tool.execute(serde_json::json!({"name": "nonexistent"})).await;
        assert!(result.is_error);
        let text = result.content.as_text();
        assert!(text.contains("nonexistent"), "missing queried name: {text}");
        assert!(text.contains("build"), "missing available list: {text}");
    }

    #[tokio::test]
    async fn empty_skill_list_reports_none_available() {
        let tool = make_tool(vec![]);
        let result = tool.execute(serde_json::json!({"name": "anything"})).await;
        assert!(result.is_error);
        assert!(result.content.as_text().contains("not found"));
    }

    #[tokio::test]
    async fn missing_name_arg_is_error() {
        let tool = make_tool(vec![]);
        let result = tool.execute(serde_json::json!({})).await;
        assert!(result.is_error);
        assert!(result.content.as_text().contains("Invalid arguments"));
    }

    #[tokio::test]
    async fn missing_file_returns_error() {
        let tool = make_tool(vec![SkillMeta {
            name: "ghost".to_string(),
            description: "missing file".to_string(),
            path: PathBuf::from("/nonexistent/path/SKILL.md"),
            base_dir: PathBuf::from("/nonexistent/path"),
        }]);
        let result = tool.execute(serde_json::json!({"name": "ghost"})).await;
        assert!(result.is_error);
        assert!(result.content.as_text().contains("Failed to read skill"));
    }

    #[test]
    fn strip_frontmatter_removes_fence() {
        let content = "---\nname: foo\n---\n\n# Body\n";
        assert_eq!(strip_frontmatter(content), "\n# Body\n");
    }

    #[test]
    fn strip_frontmatter_no_fence_returns_original() {
        let content = "# Just a doc\nno frontmatter\n";
        assert_eq!(strip_frontmatter(content), content);
    }
}
