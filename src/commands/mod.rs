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
        name: "export",
        usage: "/export [path]",
        description: "Export this session to a self-contained HTML file",
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
        description: "Switch the active configured provider instance",
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
        name: "compact",
        usage: "/compact [instructions]",
        description: "Compact session context now, optionally with summary instructions",
        takes_arg: false,
    },
    SlashCommand {
        name: "agent",
        usage: "/agent [name]",
        description: "Switch to a named agent or show the agent picker",
        takes_arg: true,
    },
    SlashCommand {
        name: "reload",
        usage: "/reload",
        description: "Reload AGENTS.md context, available skills, and agents",
        takes_arg: false,
    },
    SlashCommand {
        name: "quit",
        usage: "/quit",
        description: "Quit the application",
        takes_arg: false,
    },
];

// ── Command parsing ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum CommandAction {
    New,
    Quit,
    Reload,
    /// Export current session transcript to HTML.
    Export(Option<String>),
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
    /// Trigger immediate context compaction with optional user instructions.
    Compact(Option<String>),
    /// Switch to a named agent.
    Agent(String),
    /// `/agent` with no argument — show agent picker.
    AgentNoArg,
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
        "reload" => Some(CommandAction::Reload),
        "export" if !arg.is_empty() => Some(CommandAction::Export(Some(arg.to_string()))),
        "export" => Some(CommandAction::Export(None)),
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
        "compact" if !arg.is_empty() => Some(CommandAction::Compact(Some(arg.to_string()))),
        "compact" => Some(CommandAction::Compact(None)),
        "agent" if !arg.is_empty() => Some(CommandAction::Agent(arg.to_string())),
        "agent" => Some(CommandAction::AgentNoArg),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandAction, parse};

    #[test]
    fn parse_recognizes_builtins_and_args() {
        assert!(matches!(parse("/new"), Some(CommandAction::New)));
        assert!(matches!(parse("/quit"), Some(CommandAction::Quit)));
        assert!(matches!(parse("/reload"), Some(CommandAction::Reload)));
        assert!(matches!(
            parse("/export"),
            Some(CommandAction::Export(None))
        ));
        assert!(matches!(
            parse("/export transcript.html"),
            Some(CommandAction::Export(Some(path))) if path == "transcript.html"
        ));
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
        assert!(matches!(
            parse("/compact"),
            Some(CommandAction::Compact(None))
        ));
        assert!(matches!(
            parse("/compact include file paths only"),
            Some(CommandAction::Compact(Some(text))) if text == "include file paths only"
        ));
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
}
