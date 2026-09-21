//! Conservative credential-access detection and output redaction.
//!
//! This crate deliberately has no agent, UI, or model-facing policy wording.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViolationCategory {
    EnvironmentDump,
    CredentialPath,
    SecretExtraction,
    CredentialSearch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchCategory {
    AwsAccessKey,
    GithubToken,
    SlackToken,
    GoogleApiKey,
    Jwt,
    PrivateKey,
    SensitiveAssignment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandDecision {
    Allow,
    Block { category: ViolationCategory },
}

#[derive(Debug, Clone, Copy)]
pub struct CommandRequest<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub shell_command: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionConfig {
    pub secret: String,
    pub private_key: String,
    pub overrides: HashMap<MatchCategory, String>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            secret: "[REDACTED]".into(),
            private_key: "[PRIVATE KEY REDACTED]".into(),
            overrides: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionResult {
    pub text: String,
    pub matches: Vec<MatchCategory>,
    pub redaction_count: usize,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CredentialGuard;

impl CredentialGuard {
    pub fn inspect_command(&self, request: CommandRequest<'_>) -> CommandDecision {
        let text = request.shell_command.map(str::to_owned).unwrap_or_else(|| {
            std::iter::once(request.program)
                .chain(request.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        });
        let lower = text.to_ascii_lowercase();
        if is_environment_dump(&lower) {
            return CommandDecision::Block {
                category: ViolationCategory::EnvironmentDump,
            };
        }
        if contains_extraction_tool(&lower) {
            return CommandDecision::Block {
                category: ViolationCategory::SecretExtraction,
            };
        }
        if contains_sensitive_path(&lower) {
            let search = ["grep", "rg", "find", "fd", "strings", "awk", "sed"]
                .iter()
                .any(|word| {
                    lower
                        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .any(|part| part == *word)
                });
            return CommandDecision::Block {
                category: if search {
                    ViolationCategory::CredentialSearch
                } else {
                    ViolationCategory::CredentialPath
                },
            };
        }
        CommandDecision::Allow
    }

    pub fn redact_output(&self, input: &str, config: &RedactionConfig) -> RedactionResult {
        let mut text = input.to_owned();
        let mut matches = Vec::new();
        replace_pattern(
            &mut text,
            "AKIA",
            MatchCategory::AwsAccessKey,
            config,
            &mut matches,
            |s| s.len() >= 16 && s.chars().all(|c| c.is_ascii_alphanumeric()),
        );
        replace_pattern(
            &mut text,
            "ASIA",
            MatchCategory::AwsAccessKey,
            config,
            &mut matches,
            |s| s.len() >= 16 && s.chars().all(|c| c.is_ascii_alphanumeric()),
        );
        replace_prefixed(
            &mut text,
            "ghp_",
            MatchCategory::GithubToken,
            config,
            &mut matches,
        );
        replace_prefixed(
            &mut text,
            "github_pat_",
            MatchCategory::GithubToken,
            config,
            &mut matches,
        );
        replace_prefixed(
            &mut text,
            "xoxb-",
            MatchCategory::SlackToken,
            config,
            &mut matches,
        );
        replace_prefixed(
            &mut text,
            "xoxp-",
            MatchCategory::SlackToken,
            config,
            &mut matches,
        );
        replace_prefixed(
            &mut text,
            "AIza",
            MatchCategory::GoogleApiKey,
            config,
            &mut matches,
        );
        replace_pem(&mut text, config, &mut matches);
        replace_sensitive_assignments(&mut text, config, &mut matches);
        matches.sort_unstable_by_key(|m| *m as u8);
        matches.dedup();
        RedactionResult {
            redaction_count: matches.len(),
            text,
            matches,
        }
    }
}

fn is_environment_dump(s: &str) -> bool {
    let tokens: Vec<_> = s
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| "'\"();".contains(c)))
        .filter(|token| !token.is_empty())
        .collect();

    for (index, token) in tokens.iter().enumerate() {
        let command = token.rsplit('/').next().unwrap_or(token);
        if command != "env" && command != "printenv" {
            continue;
        }
        let rest = &tokens[index + 1..];
        if rest.is_empty()
            || (command == "printenv" && rest.iter().any(|arg| !arg.starts_with('-')))
            || rest.iter().all(|arg| arg.starts_with('-'))
            || s.contains('|')
            || s.contains('>')
        {
            return true;
        }
    }

    tokens.len() == 1 && matches!(tokens[0], "export" | "set")
}

fn contains_extraction_tool(s: &str) -> bool {
    [
        ("ansible-vault", "view"),
        ("ansible-vault", "decrypt"),
        ("sops", "decrypt"),
        ("gpg", "--decrypt"),
        ("age", "-d"),
        ("op", "read"),
        ("op", "inject"),
        ("bw", "get"),
        ("vault", "kv get"),
    ]
    .iter()
    .any(|(tool, action)| s.contains(tool) && s.contains(action))
}

fn contains_sensitive_path(s: &str) -> bool {
    [
        ".env",
        ".pem",
        ".key",
        ".p12",
        ".pfx",
        "id_rsa",
        "id_ed25519",
        ".aws/credentials",
        ".docker/config.json",
        ".kube/config",
        ".git-credentials",
        ".netrc",
        ".npmrc",
        ".pypirc",
        ".cargo/credentials",
        "credentials.",
        "secrets.",
        ".tfvars",
    ]
    .iter()
    .any(|p| s.contains(p))
}

fn replacement<'a>(category: MatchCategory, config: &'a RedactionConfig) -> &'a str {
    config
        .overrides
        .get(&category)
        .map(String::as_str)
        .unwrap_or_else(|| {
            if category == MatchCategory::PrivateKey {
                &config.private_key
            } else {
                &config.secret
            }
        })
}

fn replace_prefixed(
    text: &mut String,
    prefix: &str,
    category: MatchCategory,
    config: &RedactionConfig,
    matches: &mut Vec<MatchCategory>,
) {
    let mut start = 0;
    while let Some(relative) = text[start..].find(prefix) {
        let begin = start + relative;
        let end = text[begin..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '}'))
            .map(|n| begin + n)
            .unwrap_or(text.len());
        if end - begin > prefix.len() + 8 {
            text.replace_range(begin..end, replacement(category, config));
            matches.push(category);
            start = begin + replacement(category, config).len();
        } else {
            start = end;
        }
    }
}

fn replace_pattern<F: Fn(&str) -> bool>(
    text: &mut String,
    prefix: &str,
    category: MatchCategory,
    config: &RedactionConfig,
    matches: &mut Vec<MatchCategory>,
    valid: F,
) {
    let mut start = 0;
    while let Some(relative) = text[start..].find(prefix) {
        let begin = start + relative;
        let end = text[begin..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '}'))
            .map(|n| begin + n)
            .unwrap_or(text.len());
        if valid(&text[begin..end]) {
            text.replace_range(begin..end, replacement(category, config));
            matches.push(category);
            start = begin + replacement(category, config).len();
        } else {
            start = end;
        }
    }
}

fn replace_pem(text: &mut String, config: &RedactionConfig, matches: &mut Vec<MatchCategory>) {
    while let Some(begin) = text.find("-----BEGIN ") {
        let Some(end_rel) = text[begin..].find("-----END ") else {
            break;
        };
        let end = text[begin + end_rel..]
            .find("-----")
            .map(|n| begin + end_rel + n + 5)
            .unwrap_or(text.len());
        text.replace_range(begin..end, replacement(MatchCategory::PrivateKey, config));
        matches.push(MatchCategory::PrivateKey);
    }
}

fn replace_sensitive_assignments(
    text: &mut String,
    config: &RedactionConfig,
    matches: &mut Vec<MatchCategory>,
) {
    let keys = [
        "api_key",
        "secret",
        "token",
        "password",
        "client_secret",
        "access_token",
        "private_key",
    ];
    let mut output = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let (body, ending) = body
            .strip_suffix('\r')
            .map_or((body, ""), |body| (body, "\r"));
        let lower = body.to_ascii_lowercase();
        if keys.iter().any(|key| lower.contains(key))
            && let Some(pos) = body.find(['=', ':'])
        {
            let value_start = body[pos + 1..]
                .find(|c: char| !c.is_whitespace() && c != '"' && c != '\'')
                .map(|n| pos + 1 + n);
            if let Some(value_start) = value_start
                && !is_placeholder(&body[value_start..])
            {
                output.push_str(&body[..value_start]);
                output.push_str(replacement(MatchCategory::SensitiveAssignment, config));
                output.push_str(ending);
                if line.ends_with('\n') {
                    output.push('\n');
                }
                matches.push(MatchCategory::SensitiveAssignment);
                continue;
            }
        }
        output.push_str(line);
    }
    *text = output;
}

fn is_placeholder(value: &str) -> bool {
    let v = value
        .trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == '}' || c.is_whitespace())
        .to_ascii_lowercase();
    v.is_empty()
        || [
            "example",
            "changeme",
            "your_api_key_here",
            "your_token_here",
            "<token>",
            "placeholder",
            "null",
        ]
        .contains(&v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(command: &str) -> CommandRequest<'_> {
        CommandRequest {
            program: "sh",
            args: &[],
            shell_command: Some(command),
        }
    }

    #[test]
    fn blocks_common_reads_and_allows_normal_commands() {
        let guard = CredentialGuard;
        assert!(matches!(
            guard.inspect_command(shell("cat ~/.aws/credentials")),
            CommandDecision::Block {
                category: ViolationCategory::CredentialPath
            }
        ));
        assert!(matches!(
            guard.inspect_command(shell("printenv")),
            CommandDecision::Block {
                category: ViolationCategory::EnvironmentDump
            }
        ));
        assert!(matches!(
            guard.inspect_command(shell("git push")),
            CommandDecision::Allow
        ));
        assert!(matches!(
            guard.inspect_command(shell("rg password src/")),
            CommandDecision::Allow
        ));
    }

    #[test]
    fn blocks_extraction_and_sensitive_search() {
        let guard = CredentialGuard;
        assert!(matches!(
            guard.inspect_command(shell("ansible-vault view secrets.yml")),
            CommandDecision::Block {
                category: ViolationCategory::SecretExtraction
            }
        ));
        assert!(matches!(
            guard.inspect_command(shell("grep API_KEY .env")),
            CommandDecision::Block {
                category: ViolationCategory::CredentialSearch
            }
        ));
    }

    #[test]
    fn redacts_formats_assignments_and_placeholders() {
        let result = CredentialGuard.redact_output(
            "API_KEY=real-value\nTOKEN=your_token_here\nghp_abcdefghijklmnopqrstuvwxyz",
            &RedactionConfig::default(),
        );
        assert!(result.text.contains("API_KEY=[REDACTED]"));
        assert!(result.text.contains("your_token_here"));
        assert!(!result.text.contains("ghp_abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn redaction_is_configurable() {
        let mut config = RedactionConfig::default();
        config.secret = "<secret>".into();
        let result = CredentialGuard.redact_output("API_KEY=real", &config);
        assert!(result.text.contains("<secret>"));
    }

    #[test]
    fn detects_environment_dump_variants() {
        let guard = CredentialGuard;
        for command in [
            "env",
            "printenv",
            "export",
            "set",
            "/usr/bin/env",
            "command env",
            "env -0",
            "printenv -0",
            "printenv AWS_SECRET_ACCESS_KEY",
            "/usr/bin/printenv GITHUB_TOKEN",
            "env | sort",
            "printenv > /tmp/environment.txt",
            "sh -c 'env'",
            "bash -lc \"printenv\"",
        ] {
            assert!(
                matches!(
                    guard.inspect_command(shell(command)),
                    CommandDecision::Block {
                        category: ViolationCategory::EnvironmentDump
                    }
                ),
                "must block environment dump: {command}"
            );
        }
    }

    #[test]
    fn allows_non_dump_environment_usage() {
        let guard = CredentialGuard;
        for command in [
            "env FOO=bar ./app",
            "env VAR=value command",
            "echo $HOME",
            "printf '%s\\n' $PATH",
            "HOME=/tmp command",
            "git show --stat",
            "cargo test",
            "cat README.md",
        ] {
            assert!(
                matches!(
                    guard.inspect_command(shell(command)),
                    CommandDecision::Allow
                ),
                "must allow benign environment usage: {command}"
            );
        }
    }

    #[test]
    fn detects_sensitive_paths_and_readers() {
        let guard = CredentialGuard;
        for command in [
            "cat .env",
            "cat .env.local",
            "less ~/.aws/credentials",
            "head -20 ~/.ssh/id_rsa",
            "tail -20 ~/.ssh/id_ed25519",
            "cat ~/.docker/config.json",
            "cat ~/.kube/config",
            "cat ~/.git-credentials",
            "cat ~/.netrc",
            "cat ~/.npmrc",
            "cat ~/.pypirc",
            "cat ~/.cargo/credentials.toml",
            "cat server.key",
            "cat server.pem",
            "cat client.p12",
            "cat client.pfx",
            "cat credentials.yaml",
            "cat secrets.json",
            "cat terraform.tfvars",
        ] {
            assert!(
                matches!(
                    guard.inspect_command(shell(command)),
                    CommandDecision::Block { .. }
                ),
                "must block sensitive read: {command}"
            );
        }
    }

    #[test]
    fn detects_sensitive_search_variants() {
        let guard = CredentialGuard;
        for command in [
            "grep -R token .env",
            "grep -n password ~/.aws/credentials",
            "rg --hidden API_KEY .env.local",
            "find . -name credentials.json -print",
            "fd id_rsa ~",
            "strings secrets.bin",
            "awk '{print $0}' .pem",
            "sed -n '1,20p' .key",
            "python -c 'print(open(\".env\").read())'",
        ] {
            assert!(
                matches!(
                    guard.inspect_command(shell(command)),
                    CommandDecision::Block { .. }
                ),
                "must block sensitive search: {command}"
            );
        }
    }

    #[test]
    fn detects_secret_extraction_variants() {
        let guard = CredentialGuard;
        for command in [
            "ansible-vault view secrets.yml",
            "ansible-vault decrypt --output=- vault.yml",
            "sops --decrypt secrets.yaml",
            "gpg --decrypt secrets.gpg",
            "age -d encrypted.age",
            "age --decrypt encrypted.age",
            "op read op://prod/db/password",
            "op inject -i template.env",
            "bw get password item-id",
            "vault kv get secret/app",
        ] {
            assert!(
                matches!(
                    guard.inspect_command(shell(command)),
                    CommandDecision::Block {
                        category: ViolationCategory::SecretExtraction
                    }
                ),
                "must block secret extraction: {command}"
            );
        }
    }

    #[test]
    fn checks_direct_argv_commands() {
        let guard = CredentialGuard;
        let args = vec!["--decrypt".to_string(), "backup.gpg".to_string()];
        assert!(matches!(
            guard.inspect_command(CommandRequest {
                program: "gpg",
                args: &args,
                shell_command: None,
            }),
            CommandDecision::Block {
                category: ViolationCategory::SecretExtraction
            }
        ));

        let args = vec!["README.md".to_string()];
        assert!(matches!(
            guard.inspect_command(CommandRequest {
                program: "cat",
                args: &args,
                shell_command: None,
            }),
            CommandDecision::Allow
        ));
    }

    #[test]
    fn redacts_all_supported_token_families() {
        let input = concat!(
            "AKIAIOSFODNN7EXAMPLE ",
            "ASIAIOSFODNN7EXAMPLE ",
            "ghp_abcdefghijklmnopqrstuvwxyz1234567890 ",
            "github_pat_11AAAAAAAAAAAAAAAAAAAAAA_abcdefghijklmnopqrstuvwxyz1234567890 ",
            "xoxb-test-fixture ",
            "xoxp-test-fixture ",
            "AIzaSyDUMMYKEY1234567890"
        );
        let result = CredentialGuard.redact_output(input, &RedactionConfig::default());
        assert!(!result.text.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!result.text.contains("ASIAIOSFODNN7EXAMPLE"));
        assert!(
            !result
                .text
                .contains("ghp_abcdefghijklmnopqrstuvwxyz1234567890")
        );
        assert!(
            !result.text.contains(
                "github_pat_11AAAAAAAAAAAAAAAAAAAAAA_abcdefghijklmnopqrstuvwxyz1234567890"
            )
        );
        assert!(
            !result
                .text
                .contains("xoxb-test-fixture")
        );
        assert!(
            !result
                .text
                .contains("xoxp-test-fixture")
        );
        assert!(!result.text.contains("AIzaSyDUMMYKEY1234567890"));
        assert_eq!(result.redaction_count, result.matches.len());
        assert!(result.redaction_count >= 4);
    }

    #[test]
    fn redacts_pem_and_structured_assignments() {
        let input = concat!(
            "AWS_SECRET_ACCESS_KEY=real-secret\n",
            "password: \"real-password\"\n",
            "client_secret = 'real-client-secret'\n",
            "private_key: |\n",
            "  -----BEGIN PRIVATE KEY-----\n",
            "  base64-secret-material\n",
            "  -----END PRIVATE KEY-----\n"
        );
        let result = CredentialGuard.redact_output(input, &RedactionConfig::default());
        assert!(!result.text.contains("real-secret"));
        assert!(!result.text.contains("real-password"));
        assert!(!result.text.contains("real-client-secret"));
        assert!(!result.text.contains("base64-secret-material"));
        assert!(!result.text.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn preserves_placeholders_and_benign_text() {
        let input = concat!(
            "API_KEY=example\n",
            "TOKEN=changeme\n",
            "password: <token>\n",
            "Documentation mentions API_KEY and ghp_example only.\n",
            "ordinary output remains unchanged"
        );
        let result = CredentialGuard.redact_output(input, &RedactionConfig::default());
        assert!(result.text.contains("API_KEY=example"));
        assert!(result.text.contains("TOKEN=changeme"));
        assert!(result.text.contains("password: <token>"));
        assert!(result.text.contains("ordinary output remains unchanged"));
    }

    #[test]
    fn redacts_multiple_occurrences_and_keeps_unrelated_output() {
        let input = "before ghp_abcdefghijklmnopqrstuvwxyz1234567890 middle xoxb-test-fixture after";
        let result = CredentialGuard.redact_output(input, &RedactionConfig::default());
        assert!(
            !result
                .text
                .contains("ghp_abcdefghijklmnopqrstuvwxyz1234567890")
        );
        assert!(
            !result
                .text
                .contains("xoxb-test-fixture")
        );
        assert!(result.text.starts_with("before "));
        assert!(result.text.ends_with(" after"));
    }

    #[test]
    fn supports_category_specific_replacements() {
        let mut config = RedactionConfig::default();
        config
            .overrides
            .insert(MatchCategory::GithubToken, "<github-token>".into());
        config
            .overrides
            .insert(MatchCategory::PrivateKey, "<private-key>".into());
        let result = CredentialGuard.redact_output(
            "ghp_abcdefghijklmnopqrstuvwxyz1234567890 -----BEGIN PRIVATE KEY-----x-----END PRIVATE KEY-----",
            &config,
        );
        assert!(result.text.contains("<github-token>"));
        assert!(result.text.contains("<private-key>"));
    }
}
