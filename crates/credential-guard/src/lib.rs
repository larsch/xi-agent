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
        let normalized = normalize_command(&text);
        let lower = normalized.as_str();
        if is_environment_dump(lower) {
            return CommandDecision::Block {
                category: ViolationCategory::EnvironmentDump,
            };
        }
        if contains_extraction_tool(&lower) || contains_credential_provider_command(&lower) {
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
        for prefix in ["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
            replace_prefixed(
                &mut text,
                prefix,
                MatchCategory::GithubToken,
                config,
                &mut matches,
            );
        }
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
        replace_sensitive_structured_fields(&mut text, config, &mut matches);
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

fn normalize_command(command: &str) -> String {
    let mut normalized = String::with_capacity(command.len());
    let mut whitespace = false;
    let mut chars = command.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\' && chars.peek().is_some_and(|next| *next == '\n') {
            chars.next();
            whitespace = true;
        } else if character.is_whitespace() {
            whitespace = true;
        } else {
            if whitespace && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character.to_ascii_lowercase());
            whitespace = false;
        }
    }
    normalized
}

fn command_tokens(s: &str) -> Vec<&str> {
    s.split(|character: char| character.is_whitespace() || matches!(character, ';' | '&' | '|'))
        .filter(|token| !token.is_empty())
        .map(|token| token.trim_matches(|character: char| "'\"()".contains(character)))
        .filter(|token| !token.is_empty())
        .collect()
}

fn has_command_sequence(tokens: &[&str], sequence: &[&str]) -> bool {
    let mut next = 0;
    for token in tokens {
        if *token == sequence[next] {
            next += 1;
            if next == sequence.len() {
                return true;
            }
        }
    }
    false
}

fn is_environment_dump(s: &str) -> bool {
    let tokens = command_tokens(s);

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

fn contains_credential_provider_command(s: &str) -> bool {
    let tokens = command_tokens(s);
    let has = |sequence: &[&str]| has_command_sequence(&tokens, sequence);
    has(&["gh", "auth", "token"])
        || has(&["gcloud", "auth", "print-access-token"])
        || has(&["az", "account", "get-access-token"])
        || has(&["git", "credential", "fill"])
        || has(&["pass", "show"])
        || has(&["keyring", "get"])
        || has(&["keyring", "get_password"])
        || has(&["secret-tool", "lookup"])
        || has(&["npm", "config", "get"])
        || has(&["pip", "config", "get"])
        || has(&["kubectl", "config", "view", "--raw"])
        || has(&["helm", "registry", "login"])
        || (has(&["aws", "configure", "get"])
            && ["access_key", "secret", "token", "credential", "password"]
                .iter()
                .any(|key| tokens.iter().any(|token| token.contains(key))))
        || (tokens
            .iter()
            .any(|token| token.starts_with("docker-credential-"))
            && tokens.contains(&"get"))
}

fn contains_extraction_tool(s: &str) -> bool {
    [
        ("ansible-vault", "view"),
        ("ansible-vault", "decrypt"),
        ("sops", "decrypt"),
        ("sops", "-d"),
        ("gpg", "--decrypt"),
        ("gpg", "-d"),
        ("gpg", "--decrypt-files"),
        ("age", "-d"),
        ("age", "--decrypt"),
        ("op", "read"),
        ("op", "inject"),
        ("bw", "get"),
        ("bw", "unlock --raw"),
        ("vault", "kv get"),
        ("vault", "read"),
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
        ".config/gh/hosts.yml",
        ".config/gcloud/application_default_credentials.json",
        ".config/gcloud/credentials.db",
        ".azure/accesstokens.json",
        ".config/containers/auth.json",
        ".config/helm/registry/config.json",
        ".local/share/keyrings",
        ".config/keyring",
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

fn replace_sensitive_structured_fields(
    text: &mut String,
    config: &RedactionConfig,
    matches: &mut Vec<MatchCategory>,
) {
    let keys = [
        "secret",
        "password",
        "token",
        "access_token",
        "refresh_token",
        "client_secret",
        "clientsecret",
        "api_key",
        "apikey",
        "private_key",
        "private-key",
        "privatekey",
        "authtoken",
        "_authtoken",
        "auth_token",
        "auth-token",
        "access-token",
        "refresh-token",
    ];
    let mut cursor = 0;
    while cursor < text.len() {
        let lower = text[cursor..].to_ascii_lowercase();
        let Some((relative, key)) = keys
            .iter()
            .filter_map(|key| {
                let quoted = format!("\"{key}\"");
                lower.find(&quoted).map(|position| (position, *key))
            })
            .min_by_key(|(position, _)| *position)
        else {
            break;
        };
        let key_start = cursor + relative;
        let key_end = key_start + key.len() + 2;
        let Some(colon_relative) = text[key_end..].find(':') else {
            break;
        };
        if colon_relative > 8 {
            cursor = key_end;
            continue;
        }
        let value_start = key_end + colon_relative + 1;
        let Some(value_offset) =
            text[value_start..].find(|character: char| !character.is_whitespace())
        else {
            break;
        };
        let value_begin = value_start + value_offset;
        let quoted = text.as_bytes().get(value_begin) == Some(&b'"');
        let content_begin = if quoted { value_begin + 1 } else { value_begin };
        let value_end = if quoted {
            text[content_begin..]
                .find('"')
                .map(|offset| content_begin + offset)
                .unwrap_or(text.len())
        } else {
            text[value_begin..]
                .find(|character: char| matches!(character, ',' | '}' | '\n' | '\r'))
                .map(|offset| value_begin + offset)
                .unwrap_or(text.len())
        };
        let value = &text[content_begin..value_end];
        if !is_placeholder(value) && !value.is_empty() {
            let replacement = replacement(MatchCategory::SensitiveAssignment, config);
            text.replace_range(content_begin..value_end, replacement);
            matches.push(MatchCategory::SensitiveAssignment);
            cursor = content_begin + replacement.len();
        } else {
            cursor = value_end.max(key_end);
        }
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
        "refresh_token",
        "authtoken",
        "auth_token",
        "private_key",
    ];
    let mut output = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let (body, ending) = body
            .strip_suffix('\r')
            .map_or((body, ""), |body| (body, "\r"));
        let lower = body.to_ascii_lowercase();
        if !body.contains(['{', '}'])
            && keys.iter().any(|key| lower.contains(key))
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
    fn blocks_credential_provider_commands() {
        let guard = CredentialGuard;
        for command in [
            "gh auth token",
            "gcloud auth print-access-token",
            "az account get-access-token",
            "git credential fill",
            "pass show production/db",
            "keyring get service account",
            "secret-tool lookup service github",
            "docker-credential-secretservice get",
            "aws configure get aws_secret_access_key",
            "aws configure get aws_session_token",
            "npm config get //registry.example.com/:_authToken",
            "pip config get global.index-url",
            "kubectl config view --raw",
            "helm registry login registry.example.com",
        ] {
            assert!(
                matches!(
                    guard.inspect_command(shell(command)),
                    CommandDecision::Block {
                        category: ViolationCategory::SecretExtraction
                    }
                ),
                "must block credential provider command: {command}"
            );
        }
        assert!(matches!(
            guard.inspect_command(shell("aws configure get region")),
            CommandDecision::Allow
        ));
    }

    #[test]
    fn blocks_whitespace_and_option_variants() {
        let guard = CredentialGuard;
        for command in [
            "gh\tauth\ttoken",
            "gcloud  --account user@example.com auth print-access-token",
            "git -c credential.helper=store credential fill",
            "aws --profile prod configure get aws_secret_access_key",
            "secret-tool --unlock lookup service github",
            "sops -d secrets.yaml",
            "gpg -d --batch secret.gpg",
            "vault   read   secret/app",
            "printenv\tAWS_SECRET_ACCESS_KEY",
        ] {
            assert!(
                matches!(
                    guard.inspect_command(shell(command)),
                    CommandDecision::Block { .. }
                ),
                "must block command variant: {command:?}"
            );
        }
        assert!(matches!(
            guard.inspect_command(shell("gh auth status")),
            CommandDecision::Allow
        ));
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
            "cat ~/.config/gh/hosts.yml",
            "cat ~/.config/gcloud/application_default_credentials.json",
            "cat ~/.config/gcloud/credentials.db",
            "cat ~/.azure/accessTokens.json",
            "cat ~/.config/containers/auth.json",
            "cat ~/.config/helm/registry/config.json",
            "cat ~/.local/share/keyrings/login.keyring",
            "cat ~/.config/keyring/secrets.json",
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
    fn redacts_github_token_families_and_structured_fields() {
        let input = concat!(
            "gho_abcdefghijklmnopqrstuvwxyz1234567890 ",
            "ghu_abcdefghijklmnopqrstuvwxyz1234567890 ",
            "ghs_abcdefghijklmnopqrstuvwxyz1234567890 ",
            "ghr_abcdefghijklmnopqrstuvwxyz1234567890 ",
            r#"{"credentials":{"access_token":"real-token","refresh_token":"real-refresh","_authToken":"real-auth"}}"#,
        );
        let result = CredentialGuard.redact_output(input, &RedactionConfig::default());
        assert!(!result.text.contains("gho_"));
        assert!(!result.text.contains("ghu_"));
        assert!(!result.text.contains("ghs_"));
        assert!(!result.text.contains("ghr_"));
        assert!(!result.text.contains("real-token"));
        assert!(!result.text.contains("real-refresh"));
        assert!(!result.text.contains("real-auth"));
        assert!(result.text.contains(r#""access_token":"[REDACTED]""#));
        assert!(result.text.contains(r#""refresh_token":"[REDACTED]""#));
    }

    #[test]
    fn redacts_structured_spacing_and_key_variants() {
        let input = r#"{"apiKey" : "one", "private-key": "two", "clientSecret":"three", "access-token": "four"}"#;
        let result = CredentialGuard.redact_output(input, &RedactionConfig::default());
        assert!(!result.text.contains("one"));
        assert!(!result.text.contains("two"));
        assert!(!result.text.contains("three"));
        assert!(!result.text.contains("four"));
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
