use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use credential_guard::{
    CommandDecision, CommandRequest, CredentialGuard, MatchCategory, ViolationCategory,
};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Default, Serialize)]
pub struct AuditReport {
    pub files_scanned: usize,
    pub events_scanned: usize,
    pub parse_errors: usize,
    pub file_errors: usize,
    pub hits: Vec<AuditHit>,
    pub by_category: BTreeMap<String, usize>,
    pub by_session: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditHit {
    pub file: String,
    pub session: String,
    pub event: usize,
    pub field: String,
    pub category: String,
    pub detail: String,
}

pub fn scan(sessions_dir: &Path) -> AuditReport {
    let mut report = AuditReport::default();
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        report.file_errors = 1;
        return report;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(path) else {
            report.file_errors += 1;
            continue;
        };
        for entry in files.flatten() {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            report.files_scanned += 1;
            scan_file(&path, &mut report);
        }
    }
    report
}

fn scan_file(path: &Path, report: &mut AuditReport) {
    let Ok(file) = fs::File::open(path) else {
        report.file_errors += 1;
        return;
    };
    let session = path
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("unknown")
        .to_owned();
    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else {
            report.file_errors += 1;
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            report.parse_errors += 1;
            continue;
        };
        report.events_scanned += 1;
        scan_event(&value, path, &session, line_no + 1, report);
    }
}

fn scan_event(value: &Value, path: &Path, session: &str, event: usize, report: &mut AuditReport) {
    let Some(map) = value.as_object() else { return };
    match map.get("type").and_then(Value::as_str) {
        Some("tool_call") => {
            if let Some(args) = map.get("args") {
                scan_command_args(
                    map.get("name").and_then(Value::as_str).unwrap_or(""),
                    args,
                    path,
                    session,
                    event,
                    report,
                );
            }
        }
        Some("tool_result") => {
            let name = map.get("name").and_then(Value::as_str).unwrap_or("");
            if let Some(content) = map.get("content").and_then(Value::as_str)
                && output_boundary(name)
            {
                scan_output(content, "event.content", path, session, event, report);
            }
        }
        _ => {}
    }
}

fn output_boundary(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "bash"
            | "cmd"
            | "exec"
            | "powershell"
            | "python"
            | "python_repl"
            | "read_file"
            | "read_skill"
    )
}

fn scan_output(
    text: &str,
    field: &str,
    path: &Path,
    session: &str,
    event: usize,
    report: &mut AuditReport,
) {
    let redacted = CredentialGuard.redact_output(text, &Default::default());
    for category in &redacted.matches {
        add_hit(
            report,
            path,
            session,
            event,
            field,
            format_match(*category),
            text,
        );
    }
}

fn scan_command_args(
    _tool_name: &str,
    args: &Value,
    path: &Path,
    session: &str,
    event: usize,
    report: &mut AuditReport,
) {
    let Some(map) = args.as_object() else { return };
    let program = map.get("program").and_then(Value::as_str);
    let command = map.get("command").and_then(Value::as_str);
    if program.is_none() && command.is_none() {
        return;
    }
    let program = program.unwrap_or("sh");
    let empty = Vec::new();
    let argv = map.get("args").and_then(Value::as_array).unwrap_or(&empty);
    let args = argv
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let command = map.get("command").and_then(Value::as_str);
    let request = CommandRequest {
        program,
        args: &args,
        shell_command: command,
    };
    if let CommandDecision::Block { category } = CredentialGuard.inspect_command(request) {
        add_hit(
            report,
            path,
            session,
            event,
            "event.args",
            format_violation(category),
            &format!("{program} {}", args.join(" ")),
        );
    }
}

fn add_hit(
    report: &mut AuditReport,
    path: &Path,
    session: &str,
    event: usize,
    field: &str,
    category: String,
    detail: &str,
) {
    *report.by_category.entry(category.clone()).or_default() += 1;
    *report.by_session.entry(session.to_owned()).or_default() += 1;
    report.hits.push(AuditHit {
        file: path.display().to_string(),
        session: session.to_owned(),
        event,
        field: field.to_owned(),
        category,
        detail: detail.to_owned(),
    });
}

fn format_match(category: MatchCategory) -> String {
    format!("match::{category:?}")
}
fn format_violation(category: ViolationCategory) -> String {
    format!("command::{category:?}")
}

pub fn default_sessions_dir() -> anyhow::Result<PathBuf> {
    directories::ProjectDirs::from("", "", "xi")
        .map(|dirs| dirs.data_dir().join("sessions"))
        .ok_or_else(|| anyhow::anyhow!("could not resolve xi data directory"))
}

pub fn render(report: &AuditReport, details: bool, json: bool) -> String {
    if json {
        if details {
            return serde_json::to_string_pretty(report).expect("report serializes");
        }
        let mut safe = report.clone_for_summary();
        for hit in &mut safe.hits {
            hit.detail.clear();
        }
        return serde_json::to_string_pretty(&safe).expect("report serializes");
    }
    let mut out = format!(
        "Scanned {} session files, {} events\nParse errors: {}, file errors: {}\nHits: {}\n",
        report.files_scanned,
        report.events_scanned,
        report.parse_errors,
        report.file_errors,
        report.hits.len()
    );
    for (category, count) in &report.by_category {
        out.push_str(&format!("  {category}: {count}\n"));
    }
    if details {
        out.push_str(
            "\nWARNING: --details may print raw session content, including credentials.\n",
        );
        for hit in &report.hits {
            out.push_str(&format!(
                "\n{} event {} {} {}\n{}\n",
                hit.file, hit.event, hit.field, hit.category, hit.detail
            ));
        }
    }
    out
}

impl AuditReport {
    fn clone_for_summary(&self) -> Self {
        Self {
            files_scanned: self.files_scanned,
            events_scanned: self.events_scanned,
            parse_errors: self.parse_errors,
            file_errors: self.file_errors,
            hits: self.hits.clone(),
            by_category: self.by_category.clone(),
            by_session: self.by_session.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_hits_and_continues_after_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            cwd.join("session.jsonl"),
            concat!(
                "{\"type\":\"tool_call\",\"args\":{\"program\":\"gh\",\"args\":[\"auth\",\"token\"]}}\n",
                "not json\n",
                "{\"type\":\"tool_result\",\"name\":\"exec\",\"content\":\"API_KEY=RealSecretValue_1234567890abcdef\"}\n",
                "{\"type\":\"user_message\",\"content\":\"Please explain what an API_KEY is\"}\n"
            ),
        )
        .unwrap();
        let report = scan(dir.path());
        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.events_scanned, 3);
        assert_eq!(report.parse_errors, 1);
        assert!(
            report
                .by_category
                .keys()
                .any(|key| key.contains("SecretExtraction"))
        );
        assert!(
            report
                .by_category
                .keys()
                .any(|key| key.contains("SensitiveAssignment"))
        );
    }

    #[test]
    fn safe_rendering_omits_raw_details() {
        let report = AuditReport {
            hits: vec![AuditHit {
                file: "session.jsonl".into(),
                session: "session".into(),
                event: 1,
                field: "event.content".into(),
                category: "match::SensitiveAssignment".into(),
                detail: "API_KEY=RealSecretValue_1234567890abcdef".into(),
            }],
            ..Default::default()
        };
        assert!(!render(&report, false, false).contains("RealSecretValue_"));
        assert!(!render(&report, false, true).contains("RealSecretValue_"));
        assert!(render(&report, true, false).contains("RealSecretValue_"));
    }
}
