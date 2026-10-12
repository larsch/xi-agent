use super::*;

#[test]
fn errors_preserve_codes_and_protocol_is_checked() {
    let response = serde_json::to_value(error_response(
        serde_json::json!(1),
        "control_owned",
        "another controller",
    ))
    .unwrap();
    assert_eq!(
        response_result(&response).unwrap_err(),
        "control_owned: another controller"
    );
    assert!(
        response_result(&serde_json::json!({"ok":true}))
            .unwrap_err()
            .starts_with("protocol_error:")
    );
}

#[test]
fn display_paths_do_not_change_nonstandard_paths() {
    assert_eq!(display_path(r"\\?\C:\repo"), r"C:\repo");
    assert_eq!(
        display_path(r"\\?\UNC\host\share\repo"),
        r"\\host\share\repo"
    );
    assert_eq!(display_path("/repo"), "/repo");
    assert_eq!(
        display_path(r"\\?\Volume{abc}\repo"),
        r"\\?\Volume{abc}\repo"
    );
}

#[test]
fn completion_has_correlation_error_and_monotonic_sequence() {
    let first: serde_json::Value = serde_json::from_str(&completion_event(
        "session",
        "turn-1",
        "error",
        Path::new("log"),
        None,
        Some("provider failed"),
    ))
    .unwrap();
    let second: serde_json::Value =
        serde_json::from_str(&control_revoked_event("session", Some("turn-1"))).unwrap();
    assert_eq!(first["payload"]["turn_id"], "turn-1");
    assert_eq!(first["payload"]["error"], "provider failed");
    assert!(second["seq"].as_u64().unwrap() > first["seq"].as_u64().unwrap());
}
