//! UTF-8 interchange helpers for agent tools.
//!
//! Rich or structured payloads must use these file/stdin helpers rather than
//! native command-line arguments. Files written here are UTF-8 without a BOM.

use std::path::Path;

/// Known strings commonly produced when UTF-8 has crossed a legacy code-page
/// boundary. The replacement character catches a separate class of loss.
#[cfg(test)]
pub const MOJIBAKE_MARKERS: &[&str] = &["╬ô├ç├┤", "╬ô├½├æ", "Γò¼├┤Γö£├ºΓö£Γöñ", "\u{fffd}"];

/// Write text as UTF-8 without a BOM, creating its parent directories.
pub fn write_payload_file(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text.as_bytes())
}

/// Read a UTF-8 payload strictly. Invalid byte sequences are never replaced.
pub fn read_utf8_payload_file(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    String::from_utf8(bytes)
        .map_err(|error| anyhow::anyhow!("{} is not valid UTF-8: {error}", path.display()))
}

/// Check a retrieved value for expected text and signs of Unicode corruption.
#[cfg(test)]
pub fn assert_unicode_integrity(text: &str, expected: &[&str]) -> anyhow::Result<()> {
    let found: Vec<_> = MOJIBAKE_MARKERS
        .iter()
        .filter(|marker| text.contains(**marker))
        .copied()
        .collect();
    if !found.is_empty() {
        anyhow::bail!("Possible Unicode corruption: {found:?}");
    }

    let missing: Vec<_> = expected
        .iter()
        .filter(|value| !text.contains(**value))
        .copied()
        .collect();
    if !missing.is_empty() {
        anyhow::bail!("Expected text missing after write: {missing:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUES: &[&str] = &[
        "SYS_ALERT_02_20 – Duplicate PMID",
        "LAPC ≤6.0, SH2.0 AP <6.0",
        "Compatibility – Advanced Mode",
        "München, naïve, 日本語, emoji: 😀",
    ];

    #[test]
    fn payload_file_is_utf8_without_bom_and_round_trips_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.json");
        let mut payload =
            serde_json::to_string_pretty(&serde_json::json!({"values": VALUES})).unwrap();
        payload.push('\n');
        write_payload_file(&path, &payload).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.starts_with(&[0xef, 0xbb, 0xbf]));
        let text = read_utf8_payload_file(&path).unwrap();
        let returned: serde_json::Value = serde_json::from_str(&text).unwrap();
        for value in VALUES {
            assert!(returned.to_string().contains(value));
        }
        assert_unicode_integrity(&text, VALUES).unwrap();
    }

    #[test]
    fn invalid_utf8_fails_strictly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.json");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(read_utf8_payload_file(&path).is_err());
    }

    #[test]
    fn verifier_rejects_mojibake_and_missing_text() {
        assert!(assert_unicode_integrity("bad ╬ô├ç├┤", &[]).is_err());
        assert!(assert_unicode_integrity("good", &["missing"]).is_err());
    }
}
