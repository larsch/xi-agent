# credential-guard

`credential-guard` is a small, dependency-free Rust crate for conservative detection of credential-access commands and redaction of recognizable credential material in text output.

It is intended to be used by an application or agent harness as a safety layer. The crate contains detection and redaction logic only; it does not define UI messages, tool results, configuration loading, or agent policy wording.

## Features

- Detects high-confidence credential-disclosure commands, including:
  - environment dumps such as `env`, `printenv`, `export`, and `set`;
  - reads and searches of common credential files and paths;
  - plaintext extraction through tools such as `ansible-vault`, `sops`, `gpg`, `age`, `op`, `bw`, and `vault`;
  - both shell command strings and direct executable/argument vectors.
- Redacts recognizable output patterns for:
  - AWS access keys;
  - GitHub tokens;
  - Slack tokens;
  - Google API keys;
  - PEM/private-key material;
  - sensitive key/value assignments.
- Supports caller-provided default replacements and category-specific replacement overrides.
- Returns neutral categories and match metadata without returning matched secret values.
- Has no runtime dependencies outside the Rust standard library.

## Example

```rust
use credential_guard::{
    CommandDecision, CommandRequest, CredentialGuard, RedactionConfig,
};

let guard = CredentialGuard;
let command = CommandRequest {
    program: "cat",
    args: &["~/.aws/credentials".to_owned()],
    shell_command: None,
};

assert!(matches!(
    guard.inspect_command(command),
    CommandDecision::Block { .. }
));

let result = guard.redact_output(
    "token=ghp_abcdefghijklmnopqrstuvwxyz1234567890",
    &RedactionConfig::default(),
);
assert!(!result.text.contains("ghp_abcdefghijklmnopqrstuvwxyz1234567890"));
```

## Design principles

- **Conservative decisions:** block high-confidence credential-disclosure patterns while allowing ordinary development commands.
- **Neutral API:** applications translate categories into their own user-facing behavior.
- **No secret logging:** detection metadata identifies categories and counts, not matched values.
- **Configurable output:** replacement markers are supplied by the caller rather than imposed by the crate API.
- **Heuristic scope:** this is not a complete shell parser or a guarantee against obfuscation. Callers should retain defense-in-depth controls and treat the final accumulated output as the safety boundary.

## Testing

Run the crate tests with:

```sh
cargo test
```

The test suite covers command variants, direct argv requests, benign false-positive cases, credential-file families, extraction tools, token formats, structured assignments, PEM material, placeholders, multiple matches, and configurable replacements.
