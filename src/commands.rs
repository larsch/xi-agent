use crate::skills::SkillMeta;
use crate::thinking::ThinkingLevel;

/// A slash command supported by the application.
pub struct SlashCommand {
    pub name: &'static str,
    /// Full usage string shown in the completion popup (e.g. `/model <name>`).
    pub usage: &'static str,
    pub description: &'static str,
    /// Whether this command accepts a required argument after its name.
    pub takes_arg: bool,
}

/// All supported slash commands, in display order.
pub static COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "new",
        usage: "/new",
        description: "Start a new conversation",
        takes_arg: false,
    },
    SlashCommand {
        name: "model",
        usage: "/model <name>",
        description: "Switch to a different model",
        takes_arg: true,
    },
    SlashCommand {
        name: "provider",
        usage: "/provider <name>",
        description: "Switch the LLM provider (copilot / openai / codex / gemini / ollama)",
        takes_arg: true,
    },
    SlashCommand {
        name: "thinking",
        usage: "/thinking <off|minimal|low|medium|high|xhigh>",
        description: "Set reasoning effort for supported models/providers",
        takes_arg: true,
    },
    SlashCommand {
        name: "login",
        usage: "/login <provider>",
        description: "Authenticate provider (copilot / codex / gemini)",
        takes_arg: true,
    },
    SlashCommand {
        name: "resume",
        usage: "/resume",
        description: "Open session picker and resume a saved conversation",
        takes_arg: false,
    },
    SlashCommand {
        name: "quit",
        usage: "/quit",
        description: "Quit the application",
        takes_arg: false,
    },
];

// ── Completion items ──────────────────────────────────────────────────────────

/// A single entry in the completion popup.
#[derive(Clone)]
pub struct CompletionItem {
    /// Primary text shown in the left column (command usage or model name).
    pub label: String,
    /// Secondary text shown in the right column (description or empty).
    pub detail: String,
    /// Text to place in the textarea when this item is selected via Tab/Enter.
    /// Empty for non-interactive items (e.g. the loading indicator).
    pub complete_to: String,
    /// When true the item is a non-interactive status row (e.g. "fetching…").
    pub loading: bool,
    /// When true the item represents a fetch error and should be rendered in red.
    pub error: bool,
}

impl CompletionItem {
    fn from_command(cmd: &SlashCommand) -> Self {
        Self {
            label: cmd.usage.to_string(),
            detail: cmd.description.to_string(),
            complete_to: if cmd.takes_arg {
                format!("/{} ", cmd.name)
            } else {
                format!("/{}", cmd.name)
            },
            loading: false,
            error: false,
        }
    }

    pub(crate) fn from_model(name: &str) -> Self {
        Self {
            label: name.to_string(),
            detail: String::new(),
            complete_to: format!("/model {}", name),
            loading: false,
            error: false,
        }
    }

    pub(crate) fn from_provider(name: &str, label: &str) -> Self {
        Self {
            label: format!("{name}  —  {label}"),
            detail: String::new(),
            complete_to: format!("/provider {}", name),
            loading: false,
            error: false,
        }
    }

    fn from_skill(skill: &SkillMeta) -> Self {
        Self {
            label: format!("/skill:{}", skill.name),
            detail: skill.description.clone(),
            // Trailing space lets the user optionally append args.
            complete_to: format!("/skill:{} ", skill.name),
            loading: false,
            error: false,
        }
    }

    pub(crate) fn loading_indicator() -> Self {
        Self {
            label: "fetching models…".to_string(),
            detail: String::new(),
            complete_to: String::new(),
            loading: true,
            error: false,
        }
    }

    pub(crate) fn error_indicator(msg: &str) -> Self {
        Self {
            label: format!("error: {msg}"),
            detail: String::new(),
            complete_to: String::new(),
            loading: true,
            error: true,
        }
    }
}

// ── Completion matching ───────────────────────────────────────────────────────

/// Build the completion list for the current textarea `input`.
///
/// **Phase 1 — command name** (no space yet): filter `COMMANDS` + available
/// skills by prefix.  Skills are listed as `/skill:<name>` entries.
/// **Phase 2 — argument** (space present after `/model` or `/provider`): filter
/// available model / provider names by the typed prefix, or show a loading
/// indicator while the model list is being fetched.
pub fn completions_for(
    input: &str,
    available_models: Option<&[String]>,
    models_loading: bool,
    model_fetch_error: Option<&str>,
    skills: &[SkillMeta],
) -> Vec<CompletionItem> {
    use crate::provider::ProviderKind;

    let Some(rest) = input.strip_prefix('/') else {
        return vec![];
    };

    match rest.find(' ') {
        // ── Phase 2: argument ─────────────────────────────────────────────────
        Some(space_pos) => {
            let cmd = &rest[..space_pos];
            let arg = rest[space_pos + 1..].trim_start();

            if cmd.starts_with("skill:") {
                // `/skill:<name> <args>` — no completions for free-form args.
                return vec![];
            }

            match cmd {
                "model" => {
                    if models_loading {
                        vec![CompletionItem::loading_indicator()]
                    } else if let Some(msg) = model_fetch_error {
                        vec![CompletionItem::error_indicator(msg)]
                    } else if let Some(models) = available_models {
                        models
                            .iter()
                            .filter(|m| m.starts_with(arg))
                            .map(|m| CompletionItem::from_model(m))
                            .collect()
                    } else {
                        // Models not fetched yet — will trigger a fetch via
                        // App::should_fetch_models(); show nothing for now.
                        vec![]
                    }
                }
                "provider" => ProviderKind::all()
                    .iter()
                    .filter(|p| p.name().starts_with(arg))
                    .map(|p| CompletionItem::from_provider(p.name(), p.label()))
                    .collect(),
                "login" => ["copilot", "codex", "gemini"]
                    .iter()
                    .filter(|p| p.starts_with(arg))
                    .map(|p| CompletionItem {
                        label: (*p).to_string(),
                        detail: String::new(),
                        complete_to: format!("/login {p}"),
                        loading: false,
                        error: false,
                    })
                    .collect(),
                "thinking" => ThinkingLevel::all()
                    .iter()
                    .map(|lvl| lvl.as_str())
                    .filter(|lvl| lvl.starts_with(arg))
                    .map(|lvl| CompletionItem {
                        label: lvl.to_string(),
                        detail: String::new(),
                        complete_to: format!("/thinking {lvl}"),
                        loading: false,
                        error: false,
                    })
                    .collect(),
                _ => vec![],
            }
        }
        // ── Phase 1: command name ─────────────────────────────────────────────
        None => {
            if let Some(skill_prefix) = rest.strip_prefix("skill:") {
                // User is typing `/skill:<name>` — only show skill completions.
                skills
                    .iter()
                    .filter(|s| s.name.starts_with(skill_prefix))
                    .map(CompletionItem::from_skill)
                    .collect()
            } else {
                // General command name filtering: built-in commands + skills.
                let mut items: Vec<CompletionItem> = COMMANDS
                    .iter()
                    .filter(|c| c.name.starts_with(rest))
                    .map(CompletionItem::from_command)
                    .collect();

                // Include skills whose `/skill:<name>` form matches the prefix.
                // e.g. typing `/s` or `/sk` shows skill entries alongside commands.
                let full_prefix = format!("skill:{}", rest);
                // Alternatively check if `skill:` starts with `rest` (partial match).
                if "skill:".starts_with(rest) || rest.starts_with("skill:") {
                    for skill in skills {
                        items.push(CompletionItem::from_skill(skill));
                    }
                } else {
                    // Check each skill name with the full prefix
                    for skill in skills {
                        if format!("skill:{}", skill.name).starts_with(rest) {
                            items.push(CompletionItem::from_skill(skill));
                        }
                    }
                    let _ = full_prefix; // already handled above
                }

                items
            }
        }
    }
}

// ── Command parsing ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum CommandAction {
    New,
    Quit,
    /// Switch model to the given name.
    Model(String),
    /// `/model` typed with no argument — show interactive selection menu.
    ModelNoArg,
    /// Switch provider to the given name (e.g. `"copilot"`, `"openai"`).
    Provider(String),
    /// `/provider` typed with no argument — show interactive selection menu.
    ProviderNoArg,
    /// Authenticate with provider by name (`copilot`, `codex`, `gemini`).
    Login(String),
    /// Set thinking/reasoning level.
    Thinking(String),
    /// `/login` with no argument — show login provider picker.
    LoginNoArg,
    /// `/thinking` with no argument.
    ThinkingNoArg,
    /// Resume a specific session by id (internal command form).
    Resume(String),
    /// `/resume` with no argument — show session picker.
    ResumeNoArg,
    /// Invoke a skill by name, with optional free-form args.
    Skill {
        name: String,
        args: String,
    },
}

/// Parse a complete slash command input string into an action.
/// Returns `None` if the input is not a recognised slash command.
pub fn parse(input: &str) -> Option<CommandAction> {
    let rest = input.strip_prefix('/')?;
    let (name, arg) = match rest.find(' ') {
        Some(pos) => (&rest[..pos], rest[pos + 1..].trim()),
        None => (rest, ""),
    };

    // `/skill:<name>` — name and args separated by a space.
    if let Some(skill_name) = name.strip_prefix("skill:") {
        if !skill_name.is_empty() {
            return Some(CommandAction::Skill {
                name: skill_name.to_string(),
                args: arg.to_string(),
            });
        }
        return None;
    }

    match name {
        "new" => Some(CommandAction::New),
        "quit" => Some(CommandAction::Quit),
        "model" if !arg.is_empty() => Some(CommandAction::Model(arg.to_string())),
        "model" => Some(CommandAction::ModelNoArg),
        "provider" if !arg.is_empty() => Some(CommandAction::Provider(arg.to_string())),
        "provider" => Some(CommandAction::ProviderNoArg),
        "thinking" if !arg.is_empty() => Some(CommandAction::Thinking(arg.to_string())),
        "thinking" => Some(CommandAction::ThinkingNoArg),
        "login" if !arg.is_empty() => Some(CommandAction::Login(arg.to_string())),
        "login" => Some(CommandAction::LoginNoArg),
        "resume" if !arg.is_empty() => Some(CommandAction::Resume(arg.to_string())),
        "resume" => Some(CommandAction::ResumeNoArg),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{CommandAction, completions_for, parse};
    use crate::skills::SkillMeta;

    fn skill(name: &str, description: &str) -> SkillMeta {
        SkillMeta {
            name: name.to_string(),
            description: description.to_string(),
            path: PathBuf::from(format!("/tmp/{name}/SKILL.md")),
            base_dir: PathBuf::from(format!("/tmp/{name}")),
        }
    }

    #[test]
    fn parse_recognizes_builtins_and_args() {
        assert!(matches!(parse("/new"), Some(CommandAction::New)));
        assert!(matches!(parse("/quit"), Some(CommandAction::Quit)));
        assert!(matches!(parse("/model"), Some(CommandAction::ModelNoArg)));
        assert!(matches!(
            parse("/model gpt-4o"),
            Some(CommandAction::Model(m)) if m == "gpt-4o"
        ));
        assert!(matches!(
            parse("/provider openai"),
            Some(CommandAction::Provider(p)) if p == "openai"
        ));
        assert!(matches!(
            parse("/thinking high"),
            Some(CommandAction::Thinking(l)) if l == "high"
        ));
        assert!(matches!(
            parse("/login codex"),
            Some(CommandAction::Login(p)) if p == "codex"
        ));
        assert!(matches!(
            parse("/login gemini"),
            Some(CommandAction::Login(p)) if p == "gemini"
        ));
        assert!(matches!(
            parse("/resume abc123"),
            Some(CommandAction::Resume(id)) if id == "abc123"
        ));
        assert!(matches!(parse("/resume"), Some(CommandAction::ResumeNoArg)));
    }

    #[test]
    fn parse_skill_command_handles_name_and_optional_args() {
        assert!(parse("/skill:").is_none());
        assert!(matches!(
            parse("/skill:plan"),
            Some(CommandAction::Skill { name, args }) if name == "plan" && args.is_empty()
        ));
        assert!(matches!(
            parse("/skill:build step-by-step"),
            Some(CommandAction::Skill { name, args }) if name == "build" && args == "step-by-step"
        ));
    }

    #[test]
    fn completions_non_slash_input_returns_empty() {
        let items = completions_for("hello", None, false, None, &[]);
        assert!(items.is_empty());
    }

    #[test]
    fn model_completions_show_loading_or_error_or_matching_models() {
        let loading = completions_for("/model ", None, true, None, &[]);
        assert_eq!(loading.len(), 1);
        assert!(loading[0].loading);
        assert!(loading[0].label.contains("fetching"));

        let errored = completions_for("/model ", None, false, Some("no auth"), &[]);
        assert_eq!(errored.len(), 1);
        assert!(errored[0].error);
        assert!(errored[0].label.contains("no auth"));

        let models = vec![
            "gpt-4o".to_string(),
            "gpt-5".to_string(),
            "claude".to_string(),
        ];
        let items = completions_for("/model gpt", Some(&models), false, None, &[]);
        let complete_to: Vec<String> = items.into_iter().map(|i| i.complete_to).collect();
        assert_eq!(complete_to, vec!["/model gpt-4o", "/model gpt-5"]);
    }

    #[test]
    fn provider_and_thinking_completions_are_filtered() {
        let provider_items = completions_for("/provider ge", None, false, None, &[]);
        assert!(
            provider_items
                .iter()
                .any(|i| i.complete_to == "/provider gemini")
        );

        let thinking_items = completions_for("/thinking m", None, false, None, &[]);
        let complete_to: Vec<String> = thinking_items.into_iter().map(|i| i.complete_to).collect();
        assert!(complete_to.contains(&"/thinking minimal".to_string()));
        assert!(complete_to.contains(&"/thinking medium".to_string()));
        assert!(!complete_to.contains(&"/thinking high".to_string()));
    }

    #[test]
    fn command_name_completion_includes_matching_commands_and_skills() {
        let skills = vec![skill("plan", "Planning"), skill("build", "Build things")];

        let items = completions_for("/s", None, false, None, &skills);
        let complete_to: Vec<String> = items.into_iter().map(|i| i.complete_to).collect();
        assert!(complete_to.iter().any(|c| c == "/skill:plan "));
        assert!(complete_to.iter().any(|c| c == "/skill:build "));

        let skill_only = completions_for("/skill:pl", None, false, None, &skills);
        assert_eq!(skill_only.len(), 1);
        assert_eq!(skill_only[0].complete_to, "/skill:plan ");

        let no_arg_completion = completions_for("/skill:plan anything", None, false, None, &skills);
        assert!(no_arg_completion.is_empty());
    }
}
