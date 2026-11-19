use hfdx_core::{
    Endpoint, RepositoryError,
    locale::{self, Message},
    storage::{FailureKind, Language, TaskState},
    task::{CompletionOutcome, CoreError, EngineFailure, ErrorCode, ErrorDetail, UiError, UiPhase},
};
use std::collections::HashSet;

const LANGUAGES: [Language; 3] = [Language::English, Language::ZhHans, Language::ZhHant];

#[test]
fn every_catalog_entry_has_text_in_each_language() {
    for &message in Message::ALL {
        for language in LANGUAGES {
            assert!(
                !locale::text(language, message).trim().is_empty(),
                "{message:?} {language:?}"
            );
        }
    }
}

#[test]
fn verification_labels_are_distinct_in_each_language() {
    let messages = [
        Message::DisabledCount,
        Message::PendingCount,
        Message::NoSupportedDigest,
    ];
    for language in LANGUAGES {
        let labels: HashSet<_> = messages
            .into_iter()
            .map(|message| locale::text(language, message))
            .collect();
        assert_eq!(labels.len(), messages.len());
        assert!(labels.iter().all(|label| !label.trim().is_empty()));
    }
}

#[test]
fn phases_states_and_failure_reasons_remain_distinct_in_each_language() {
    let phases = [
        UiPhase::Idle,
        UiPhase::Loading,
        UiPhase::Browsing,
        UiPhase::Preparing,
        UiPhase::Downloading,
        UiPhase::Verifying,
        UiPhase::Paused,
        UiPhase::Stopping,
        UiPhase::Closed,
    ];
    let states = [
        TaskState::Paused,
        TaskState::Running,
        TaskState::Completed,
        TaskState::Failed,
    ];
    let failures = [
        FailureKind::Network,
        FailureKind::Authentication,
        FailureKind::Disk,
        FailureKind::Engine,
        FailureKind::Integrity,
        FailureKind::Interrupted,
    ];
    for language in LANGUAGES {
        assert_eq!(
            phases
                .iter()
                .map(|&v| locale::phase(language, v))
                .collect::<HashSet<_>>()
                .len(),
            phases.len()
        );
        assert_eq!(
            states
                .iter()
                .map(|&v| locale::state(language, v))
                .collect::<HashSet<_>>()
                .len(),
            states.len()
        );
        assert_eq!(
            failures
                .iter()
                .map(|&v| locale::failure(language, v))
                .collect::<HashSet<_>>()
                .len(),
            failures.len()
        );
    }
}

#[test]
fn every_error_uses_its_code_without_app_generated_english() {
    let codes = [
        ErrorCode::Input,
        ErrorCode::Authentication,
        ErrorCode::NotFound,
        ErrorCode::Network,
        ErrorCode::RateLimited,
        ErrorCode::UnsupportedEndpoint,
        ErrorCode::UnsafePath,
        ErrorCode::DestinationConflict,
        ErrorCode::Locked,
        ErrorCode::CorruptRecord,
        ErrorCode::UnsupportedSchema,
        ErrorCode::Disk,
        ErrorCode::Engine,
        ErrorCode::InvalidState,
        ErrorCode::Cancelled,
        ErrorCode::Internal,
    ];
    for language in LANGUAGES {
        let mut categories = HashSet::new();
        for code in codes {
            let input = UiError {
                code,
                message: "server-detail-π".into(),
                hint: "hint-detail-λ".into(),
                detail: None,
            };
            let rendered = locale::error(language, &input);
            assert!(rendered.contains(&format!("{code:?}")));
            assert!(!rendered.contains(&input.message));
            assert!(!rendered.contains(&input.hint));
            categories.insert(rendered);
        }
        assert_eq!(categories.len(), codes.len());
    }
}

#[test]
fn http_detail_survives_core_error_conversion() {
    let endpoint = Endpoint::parse("https://example.org:8443", false).unwrap();
    let error = CoreError::Repository(RepositoryError::Http {
        endpoint: endpoint.clone(),
        status: 429,
    })
    .ui_error();
    for language in LANGUAGES {
        let rendered = locale::error(language, &error);
        assert!(rendered.contains("429"));
        assert!(rendered.contains(endpoint.url().as_str()));
    }
}

#[test]
fn engine_details_keep_stage_reason_and_rpc_code_in_each_language() {
    let reasons = [
        EngineFailure::UnsupportedProxy,
        EngineFailure::Io,
        EngineFailure::Protocol,
        EngineFailure::Connection,
        EngineFailure::Response("totalLength"),
        EngineFailure::Exited(Some(127)),
        EngineFailure::Rpc(-32603),
        EngineFailure::Timeout,
        EngineFailure::Shutdown,
    ];
    let operations = ["launch", "configure", "add", "status", "forget", "shutdown"];
    for language in LANGUAGES {
        let mut output = HashSet::new();
        for reason in &reasons {
            let error = UiError {
                code: ErrorCode::Engine,
                message: "application-message".into(),
                hint: "application-hint".into(),
                detail: Some(ErrorDetail::Engine {
                    operation: "launch",
                    reason: *reason,
                }),
            };
            let rendered = locale::error(language, &error);
            assert!(!rendered.contains(&error.message));
            assert!(!rendered.contains(&error.hint));
            if matches!(reason, EngineFailure::Rpc(_)) {
                assert!(rendered.contains("-32603"));
            }
            output.insert(rendered);
        }
        assert_eq!(output.len(), reasons.len());
        let stages: HashSet<_> = operations
            .into_iter()
            .map(|operation| {
                locale::error(
                    language,
                    &UiError {
                        code: ErrorCode::Engine,
                        message: String::new(),
                        hint: String::new(),
                        detail: Some(ErrorDetail::Engine {
                            operation,
                            reason: EngineFailure::Io,
                        }),
                    },
                )
            })
            .collect();
        assert_eq!(stages.len(), operations.len());
        let unknown = locale::error(
            language,
            &UiError {
                code: ErrorCode::Engine,
                message: String::new(),
                hint: String::new(),
                detail: Some(ErrorDetail::Engine {
                    operation: "new-stage",
                    reason: EngineFailure::Io,
                }),
            },
        );
        assert!(unknown.contains("new-stage"));
    }
}

#[test]
fn completion_outcomes_and_counts_are_preserved() {
    let outcomes = [
        CompletionOutcome::Verified,
        CompletionOutcome::VerificationDisabled,
        CompletionOutcome::Unverified,
        CompletionOutcome::IntegrityFailed,
    ];
    for language in LANGUAGES {
        let rendered: HashSet<_> = outcomes
            .into_iter()
            .map(|outcome| locale::completion(language, outcome, 17, 23, 41))
            .collect();
        assert_eq!(rendered.len(), outcomes.len());
        for value in rendered {
            for count in ["17", "23", "41"] {
                assert!(value.contains(count));
            }
        }
    }
}
