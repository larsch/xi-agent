use std::path::Path;

use crate::commands::{COMMANDS, SlashCommand};
use crate::skills::SkillMeta;
use crate::thinking::ThinkingLevel;

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
    /// Byte range within `label` that matches the user's typed query, used for
    /// visual highlighting. `None` means no highlight (e.g. prefix match where
    /// the match is implied, or non-interactive rows).
    pub match_range: Option<(usize, usize)>,
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
            match_range: None,
        }
    }

    pub(crate) fn from_model(name: &str) -> Self {
        Self {
            label: name.to_string(),
            detail: String::new(),
            complete_to: format!("/model {}", name),
            loading: false,
            error: false,
            match_range: None,
        }
    }

    pub(crate) fn from_provider(name: &str, label: &str) -> Self {
        Self {
            label: name.to_string(),
            detail: label.to_string(),
            complete_to: format!("/provider {}", name),
            loading: false,
            error: false,
            match_range: None,
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
            match_range: None,
        }
    }

    pub(crate) fn loading_indicator() -> Self {
        Self {
            label: "fetching models…".to_string(),
            detail: String::new(),
            complete_to: String::new(),
            loading: true,
            error: false,
            match_range: None,
        }
    }

    pub(crate) fn error_indicator(msg: &str) -> Self {
        Self {
            label: format!("error: {msg}"),
            detail: String::new(),
            complete_to: String::new(),
            loading: true,
            error: true,
            match_range: None,
        }
    }
}

// ── @<file> completions ──────────────────────────────────────────────────────

/// Build file/directory completions for the last `@<path>` token in `input`.
///
/// Scans the input for the last `@` token (preceded by whitespace or at string
/// start) and returns matching file/directory entries from the filesystem.
/// Returns an empty vec if no valid `@` token is found.
pub fn at_completions(input: &str, cwd: &Path) -> Vec<CompletionItem> {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut path_start = None;
    let mut path_end = 0;

    while i < len {
        if bytes[i] == b'@' {
            let preceded_by_space = i == 0 || bytes[i - 1].is_ascii_whitespace();
            if preceded_by_space {
                if i + 1 >= len {
                    // @ is at end of input — empty partial path.
                    path_start = Some(i + 1);
                    path_end = i + 1;
                } else if bytes[i + 1].is_ascii_whitespace() {
                    // @ followed by whitespace — not a file token, skip.
                    i += 1;
                    continue;
                } else {
                    let start = i + 1;
                    let mut end = start;
                    while end < len && !bytes[end].is_ascii_whitespace() && bytes[end] != b'"' {
                        end += 1;
                    }
                    path_start = Some(start);
                    path_end = end;
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }

    let start = match path_start {
        Some(s) => s,
        None => return vec![],
    };
    let partial = &input[start..path_end];
    file_completions_for(partial, cwd)
}

/// Generate file completion items for a partial path relative to `cwd`.
fn file_completions_for(partial: &str, cwd: &Path) -> Vec<CompletionItem> {
    if partial.is_empty() {
        return list_dir_entries(cwd, "", "");
    }

    // A trailing `/` means the user has finished typing a directory name and
    // wants to list its contents — treat the whole thing as the parent dir.
    let ends_with_slash = partial.ends_with('/');

    let p = std::path::Path::new(partial);
    let parent = if ends_with_slash {
        p
    } else {
        p.parent().unwrap_or(std::path::Path::new(""))
    };
    let file_name = if ends_with_slash {
        ""
    } else {
        p.file_name().and_then(|s| s.to_str()).unwrap_or("")
    };

    let search_dir = expand_path(parent, cwd);

    let parent_prefix = if parent.as_os_str().is_empty() {
        String::new()
    } else {
        let parent_str = parent.to_string_lossy();
        if parent_str.ends_with('/') {
            parent_str.to_string()
        } else {
            format!("{}/", parent_str)
        }
    };

    list_dir_entries(&search_dir, file_name, &parent_prefix)
}

/// Expand a path that may use `~` or be relative to `cwd`.
fn expand_path(path: &std::path::Path, cwd: &Path) -> std::path::PathBuf {
    let path_str = path.to_string_lossy();
    if path_str.is_empty() || path_str == "." {
        return cwd.to_path_buf();
    }
    if let Some(rest) = path_str.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return std::path::PathBuf::from(home).join(rest);
    }
    if path_str.starts_with('/') {
        return std::path::PathBuf::from(path_str.as_ref());
    }
    cwd.join(path)
}

/// List entries in `dir` whose name starts with `prefix`, returning
/// `CompletionItem`s whose `complete_to` is `parent_prefix + name`.
fn list_dir_entries(
    dir: &std::path::Path,
    prefix: &str,
    parent_prefix: &str,
) -> Vec<CompletionItem> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };

    let mut items: Vec<CompletionItem> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let path = format!("{}{}", parent_prefix, name);
            let label = if is_dir {
                format!("{path}/")
            } else {
                path.clone()
            };
            CompletionItem {
                label,
                detail: if is_dir {
                    "dir".to_string()
                } else {
                    String::new()
                },
                complete_to: path,
                loading: false,
                error: false,
                match_range: None,
            }
        })
        .collect();

    // Sort: directories first, then files, both alphabetically.
    items.sort_by(|a, b| {
        let a_is_dir = a.detail == "dir";
        let b_is_dir = b.detail == "dir";
        b_is_dir
            .cmp(&a_is_dir)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });

    items
}

// ── Completion matching ───────────────────────────────────────────────────────

/// Build the completion list for the current textarea `input`.
///
/// **Phase 1 — command name** (no space yet): filter `COMMANDS` + available
/// skills by prefix.  Skills are listed as `/skill:<name>` entries.
/// **Phase 2 — argument** (space present after `/model` or `/provider`): filter
/// available model / provider names by the typed prefix, or show a loading
/// indicator while the model list is being fetched.
#[allow(clippy::too_many_arguments)]
pub fn completions_for(
    input: &str,
    available_models: Option<&[String]>,
    models_loading: bool,
    model_fetch_error: Option<&str>,
    skills: &[SkillMeta],
    thinking_enabled: bool,
    provider_instances: &[crate::provider_instance::ProviderInstance],
    cwd: &Path,
) -> Vec<CompletionItem> {
    // Handle @<file> completions — check for any @ token in the input.
    if input.contains('@') {
        let at_items = at_completions(input, cwd);
        if !at_items.is_empty() {
            return at_items;
        }
    }

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
                "provider" => provider_instances
                    .iter()
                    .filter(|p| p.id.starts_with(arg))
                    .map(|p| CompletionItem::from_provider(&p.id, &p.label()))
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
                        match_range: None,
                    })
                    .collect(),
                "thinking" if thinking_enabled => ThinkingLevel::all()
                    .iter()
                    .map(|lvl| lvl.as_str())
                    .filter(|lvl| lvl.starts_with(arg))
                    .map(|lvl| CompletionItem {
                        label: lvl.to_string(),
                        detail: String::new(),
                        complete_to: format!("/thinking {lvl}"),
                        loading: false,
                        error: false,
                        match_range: None,
                    })
                    .collect(),
                "thinking" => vec![],
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
                // Phase 1a — prefix matches (no highlight needed).
                // Phase 1b — mid-string matches (highlight the matched portion).
                // Prefix matches are listed first; within each tier the original
                // declaration order is preserved.

                let mut prefix_items: Vec<CompletionItem> = Vec::new();
                let mut substr_items: Vec<CompletionItem> = Vec::new();

                for cmd in COMMANDS {
                    if !thinking_enabled && cmd.name == "thinking" {
                        continue;
                    }
                    if cmd.name.starts_with(rest) {
                        // Prefix match: the typed text lands right after the
                        // leading slash in the label, so match_range covers
                        // the leading "/" plus the typed text.
                        let start = 1; // skip the leading "/"
                        let end = start + rest.len();
                        let mut item = CompletionItem::from_command(cmd);
                        if !rest.is_empty() {
                            item.match_range = Some((start, end));
                        }
                        prefix_items.push(item);
                    } else if let Some(pos) = cmd.name.find(rest) {
                        // Mid-string match: offset by 1 for the leading "/".
                        let start = 1 + pos;
                        let end = start + rest.len();
                        let mut item = CompletionItem::from_command(cmd);
                        item.match_range = Some((start, end));
                        substr_items.push(item);
                    }
                }

                let mut items: Vec<CompletionItem> =
                    prefix_items.into_iter().chain(substr_items).collect();

                // Include skills whose `/skill:<name>` form matches (prefix or
                // substring). Skills are also split into prefix-first / substr-second
                // tiers and appended after built-in command matches.
                //
                // The label for a skill is "/skill:<name>", so offsets into the
                // label are: '/' = 0, 's' = 1 … ':' = 6, name starts at 7.
                const SKILL_NAME_OFFSET: usize = "/skill:".len(); // 7

                let mut skill_prefix_items: Vec<CompletionItem> = Vec::new();
                let mut skill_substr_items: Vec<CompletionItem> = Vec::new();

                if "skill:".starts_with(rest) || rest.starts_with("skill:") {
                    // User is typing the "skill:" prefix itself — show all skills,
                    // no per-name highlight needed.
                    for skill in skills {
                        skill_prefix_items.push(CompletionItem::from_skill(skill));
                    }
                } else {
                    for skill in skills {
                        let full = format!("skill:{}", skill.name); // no leading "/"
                        if full.starts_with(rest) {
                            // Prefix match on "skill:<name>" — highlight from "/".
                            let start = 1; // after the "/"
                            let end = start + rest.len();
                            let mut item = CompletionItem::from_skill(skill);
                            if !rest.is_empty() {
                                item.match_range = Some((start, end));
                            }
                            skill_prefix_items.push(item);
                        } else if let Some(pos) = skill.name.find(rest) {
                            // Substring match inside the skill name.
                            let start = SKILL_NAME_OFFSET + pos;
                            let end = start + rest.len();
                            let mut item = CompletionItem::from_skill(skill);
                            item.match_range = Some((start, end));
                            skill_substr_items.push(item);
                        }
                    }
                }

                items.extend(skill_prefix_items);
                items.extend(skill_substr_items);
                items
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::completions_for;
    use crate::skills::SkillMeta;

    fn skill(name: &str, description: &str) -> SkillMeta {
        SkillMeta {
            name: name.to_string(),
            description: description.to_string(),
            path: PathBuf::from(format!("/tmp/{name}/SKILL.md")),
            base_dir: PathBuf::from(format!("/tmp/{name}")),
            embedded_body: None,
        }
    }

    fn cwd() -> &'static Path {
        Path::new(".")
    }

    #[test]
    fn completions_non_slash_input_returns_empty() {
        let items = completions_for("hello", None, false, None, &[], true, &[], cwd());
        assert!(items.is_empty());
    }

    #[test]
    fn model_completions_show_loading_or_error_or_matching_models() {
        let loading = completions_for("/model ", None, true, None, &[], true, &[], cwd());
        assert_eq!(loading.len(), 1);
        assert!(loading[0].loading);
        assert!(loading[0].label.contains("fetching"));

        let errored = completions_for(
            "/model ",
            None,
            false,
            Some("no auth"),
            &[],
            true,
            &[],
            cwd(),
        );
        assert_eq!(errored.len(), 1);
        assert!(errored[0].error);
        assert!(errored[0].label.contains("no auth"));

        let models = vec![
            "gpt-4o".to_string(),
            "gpt-5".to_string(),
            "claude".to_string(),
        ];
        let items = completions_for(
            "/model gpt",
            Some(&models),
            false,
            None,
            &[],
            true,
            &[],
            cwd(),
        );
        let complete_to: Vec<String> = items.into_iter().map(|i| i.complete_to).collect();
        assert_eq!(complete_to, vec!["/model gpt-4o", "/model gpt-5"]);
    }

    #[test]
    fn provider_and_thinking_completions_are_filtered() {
        let providers = vec![
            crate::provider_instance::ProviderInstance::new(
                "gemini",
                crate::provider_instance::BackendPreset::Gemini,
            ),
            crate::provider_instance::ProviderInstance::new(
                "gpu-box",
                crate::provider_instance::BackendPreset::Ollama,
            ),
        ];
        let provider_items = completions_for(
            "/provider ge",
            None,
            false,
            None,
            &[],
            true,
            &providers,
            cwd(),
        );
        assert!(
            provider_items
                .iter()
                .any(|i| i.complete_to == "/provider gemini")
        );

        let thinking_items =
            completions_for("/thinking m", None, false, None, &[], true, &[], cwd());
        let complete_to: Vec<String> = thinking_items.into_iter().map(|i| i.complete_to).collect();
        assert!(complete_to.contains(&"/thinking minimal".to_string()));
        assert!(complete_to.contains(&"/thinking medium".to_string()));
        assert!(!complete_to.contains(&"/thinking high".to_string()));
    }

    #[test]
    fn thinking_completions_hidden_when_thinking_disabled() {
        let thinking_items =
            completions_for("/thinking m", None, false, None, &[], false, &[], cwd());
        assert!(thinking_items.is_empty());

        let cmd_items = completions_for("/t", None, false, None, &[], false, &[], cwd());
        assert!(
            cmd_items
                .iter()
                .all(|i| !i.complete_to.starts_with("/thinking"))
        );
    }

    #[test]
    fn thinking_completions_visible_when_thinking_enabled() {
        let cmd_items = completions_for("/t", None, false, None, &[], true, &[], cwd());
        assert!(
            cmd_items
                .iter()
                .any(|i| i.complete_to.starts_with("/thinking"))
        );
    }

    #[test]
    fn command_name_completion_includes_matching_commands_and_skills() {
        let skills = vec![skill("plan", "Planning"), skill("build", "Build things")];

        let items = completions_for("/s", None, false, None, &skills, true, &[], cwd());
        let complete_to: Vec<String> = items.into_iter().map(|i| i.complete_to).collect();
        assert!(complete_to.iter().any(|c| c == "/skill:plan "));
        assert!(complete_to.iter().any(|c| c == "/skill:build "));

        let skill_only = completions_for("/skill:pl", None, false, None, &skills, true, &[], cwd());
        assert_eq!(skill_only.len(), 1);
        assert_eq!(skill_only[0].complete_to, "/skill:plan ");

        let no_arg_completion = completions_for(
            "/skill:plan anything",
            None,
            false,
            None,
            &skills,
            true,
            &[],
            cwd(),
        );
        assert!(no_arg_completion.is_empty());
    }

    #[test]
    fn skill_substring_match_finds_skill_by_name_fragment() {
        let skills = vec![
            skill("brainstorm", "Brainstorm ideas"),
            skill("plan", "Planning"),
        ];

        // "/brainstorm" should match "/skill:brainstorm" as a substring of the skill name.
        let items = completions_for("/brainstorm", None, false, None, &skills, true, &[], cwd());
        let complete_to: Vec<String> = items.iter().map(|i| i.complete_to.clone()).collect();
        assert!(
            complete_to.contains(&"/skill:brainstorm ".to_string()),
            "expected /skill:brainstorm  in results, got: {complete_to:?}"
        );

        // The match range should point to "brainstorm" within "/skill:brainstorm"
        // "/skill:" is 7 bytes, so "brainstorm" starts at offset 7.
        let item = items
            .iter()
            .find(|i| i.complete_to == "/skill:brainstorm ")
            .unwrap();
        assert_eq!(item.match_range, Some((7, 17))); // "brainstorm" = 10 chars

        // "/plan" should match "/skill:plan" similarly (offset 7, len 4).
        let items2 = completions_for("/plan", None, false, None, &skills, true, &[], cwd());
        let plan_item = items2
            .iter()
            .find(|i| i.complete_to == "/skill:plan ")
            .unwrap();
        assert_eq!(plan_item.match_range, Some((7, 11)));
    }

    #[test]
    fn substring_match_includes_mid_string_commands() {
        // "/load" should match "/reload" (substring) in addition to prefix matches.
        let items = completions_for("/load", None, false, None, &[], true, &[], cwd());
        let complete_to: Vec<String> = items.iter().map(|i| i.complete_to.clone()).collect();
        assert!(
            complete_to.contains(&"/reload".to_string()),
            "expected /reload in results, got: {complete_to:?}"
        );
    }

    #[test]
    fn prefix_matches_come_before_substring_matches() {
        // "/re" is a prefix of "/reload" and "/resume" but a substring of nothing else
        // with that prefix. Verify ordering: prefix matches first.
        let items = completions_for("/load", None, false, None, &[], true, &[], cwd());
        // There are no commands whose name *starts with* "load", so all results
        // are substring matches. "/reload" should be present.
        let names: Vec<&str> = items.iter().map(|i| i.complete_to.as_str()).collect();
        assert!(names.contains(&"/reload"), "expected /reload: {names:?}");

        let re_items = completions_for("/re", None, false, None, &[], true, &[], cwd());
        let re_names: Vec<&str> = re_items.iter().map(|i| i.complete_to.as_str()).collect();
        assert!(re_names.contains(&"/reload"), "{re_names:?}");
        assert!(re_names.contains(&"/resume"), "{re_names:?}");
        let reload_pos = re_names.iter().position(|&n| n == "/reload").unwrap();
        let resume_pos = re_names.iter().position(|&n| n == "/resume").unwrap();
        for (pos, name) in re_names.iter().enumerate() {
            let cmd_name = name.trim_start_matches('/');
            if !cmd_name.starts_with("re") {
                assert!(
                    pos > reload_pos && pos > resume_pos,
                    "substring match {name} should come after prefix matches"
                );
            }
        }
    }

    #[test]
    fn match_range_is_set_for_highlighted_commands() {
        let items = completions_for("/re", None, false, None, &[], true, &[], cwd());
        let reload = items.iter().find(|i| i.complete_to == "/reload").unwrap();
        assert_eq!(reload.match_range, Some((1, 3)));

        let items2 = completions_for("/load", None, false, None, &[], true, &[], cwd());
        let reload2 = items2.iter().find(|i| i.complete_to == "/reload").unwrap();
        assert_eq!(reload2.match_range, Some((3, 7)));
    }

    // ── @<file> completion tests ──────────────────────────────────────────────

    #[test]
    fn at_completions_empty_input_no_at_returns_empty() {
        let items = completions_for("no at sign", None, false, None, &[], true, &[], cwd());
        assert!(items.is_empty());
    }

    #[test]
    fn at_completions_shows_current_dir_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("alpha.txt"), "").unwrap();
        std::fs::write(dir.path().join("beta.rs"), "").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let items = completions_for("@", None, false, None, &[], true, &[], dir.path());
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"subdir/"));
        assert!(labels.contains(&"alpha.txt"));
        assert!(labels.contains(&"beta.rs"));
    }

    #[test]
    fn at_completions_filters_by_prefix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("alpha.txt"), "").unwrap();
        std::fs::write(dir.path().join("beta.rs"), "").unwrap();
        std::fs::write(dir.path().join("gamma.py"), "").unwrap();

        let items = completions_for("@al", None, false, None, &[], true, &[], dir.path());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "alpha.txt");
        assert_eq!(items[0].complete_to, "alpha.txt");
    }

    #[test]
    fn at_completions_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("main.rs"), "").unwrap();
        std::fs::write(sub.join("lib.rs"), "").unwrap();

        let items = completions_for("@src/", None, false, None, &[], true, &[], dir.path());
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"src/main.rs"));
        assert!(labels.contains(&"src/lib.rs"));
    }

    #[test]
    fn at_completions_subdirectory_prefix_filter() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("main.rs"), "").unwrap();
        std::fs::write(sub.join("lib.rs"), "").unwrap();

        let items = completions_for("@src/ma", None, false, None, &[], true, &[], dir.path());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "src/main.rs");
        assert_eq!(items[0].complete_to, "src/main.rs");
    }

    #[test]
    fn at_completions_directories_listed_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("apples.txt"), "").unwrap();
        std::fs::create_dir(dir.path().join("artifacts")).unwrap();

        let items = completions_for("@a", None, false, None, &[], true, &[], dir.path());
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "artifacts/");
        assert_eq!(items[0].detail, "dir");
        assert_eq!(items[1].label, "apples.txt");
    }

    #[test]
    fn at_completions_no_match_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("alpha.txt"), "").unwrap();

        let items = completions_for("@zzz", None, false, None, &[], true, &[], dir.path());
        assert!(items.is_empty());
    }

    #[test]
    fn at_completions_nonexistent_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let items = completions_for(
            "@nonexistent/",
            None,
            false,
            None,
            &[],
            true,
            &[],
            dir.path(),
        );
        assert!(items.is_empty());
    }

    #[test]
    fn at_completions_ignores_mid_word_at() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "").unwrap();
        // "user@host" — not preceded by whitespace, so no completions.
        let items = completions_for("user@", None, false, None, &[], true, &[], dir.path());
        assert!(items.is_empty());
    }

    #[test]
    fn at_completions_lone_at_with_nothing_after() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "").unwrap();
        let items = completions_for("@ ", None, false, None, &[], true, &[], dir.path());
        assert!(items.is_empty());
    }
}
