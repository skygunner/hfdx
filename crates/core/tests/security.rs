use hfdx_core::storage::{FailureKind, FileFailure};

#[test]
fn serialized_failure_does_not_contain_sensitive_message() {
    let failure = FileFailure {
        kind: FailureKind::Authentication,
        message: "fake-secret".into(),
        aria2_code: Some(22),
        http_status: Some(429),
    };
    let encoded = serde_json::to_string(&failure).unwrap();
    assert!(!encoded.contains("fake-secret"));
    let decoded: FileFailure = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.kind, FailureKind::Authentication);
    assert_eq!(decoded.aria2_code, Some(22));
    assert_eq!(decoded.http_status, Some(429));
    assert!(!decoded.message.is_empty());
}
