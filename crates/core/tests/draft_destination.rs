use hfdx_core::{
    task::{AppSnapshot, UiPhase, draft_destination},
    *,
};
use std::{path::Path, sync::Arc};

fn snapshot() -> AppSnapshot {
    let settings: Settings = serde_json::from_value(serde_json::json!({
        "endpoint":{"url":"https://huggingface.co/","allow_http":false},
        "options":{"connections_per_file":1,"concurrent_files":2},
        "proxy_address":null,"recent_directories":["/models"],"recent_tasks":[]
    }))
    .unwrap();
    let mut selection = Selection::new();
    let request = selection
        .begin(RepositorySpec {
            request: RepositoryRequest {
                endpoint: settings.endpoint.clone(),
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/old-model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            resolved_commit: CommitId::parse(&"a".repeat(40)).unwrap(),
        })
        .unwrap();
    selection
        .append(CatalogPage {
            catalog: request.id,
            entries: vec![],
            complete: true,
        })
        .unwrap();
    AppSnapshot {
        revision: 0,
        phase: UiPhase::Browsing,
        selection: selection.view(),
        task: None,
        settings: Arc::new(settings),
        recent: Arc::from([]),
        error: None,
        diagnostics: Default::default(),
        queue: None,
    }
}
#[test]
fn switching_repository_replaces_the_previous_default_but_keeps_explicit_folder() {
    let snapshot = snapshot();
    let mut repository = snapshot
        .selection
        .catalog
        .as_ref()
        .unwrap()
        .source
        .request
        .clone();
    assert_eq!(
        draft_destination(&snapshot, &repository, Path::new("/models/old-model")).unwrap(),
        Path::new("/models/old-model")
    );
    repository.repo_id = RepoId::parse("unsloth/Qwen3.8-27B-GGUF").unwrap();
    assert_eq!(
        draft_destination(&snapshot, &repository, Path::new("/models/old-model")).unwrap(),
        Path::new("/models/Qwen3.8-27B-GGUF")
    );
    assert_eq!(
        draft_destination(&snapshot, &repository, Path::new("/explicit/custom")).unwrap(),
        Path::new("/explicit/custom")
    );
    assert_eq!(
        draft_destination(&snapshot, &repository, Path::new("")).unwrap(),
        Path::new("/models/Qwen3.8-27B-GGUF")
    );
}
#[test]
fn directory_errors_name_owner_and_distinguish_other_version_in_all_languages() {
    let snapshot = snapshot();
    let owner = &snapshot.selection.catalog.as_ref().unwrap().source;
    for same_repository in [false, true] {
        let error = task::CoreError::DestinationInUse {
            path: "/models/old-model".into(),
            owner: Box::new(owner.clone()),
            same_repository,
        }
        .ui_error();
        for language in [Language::English, Language::ZhHans, Language::ZhHant] {
            let message = locale::error(language, &error);
            assert!(message.contains("org/old-model"));
            assert!(message.contains("/models/old-model"));
            assert!(message.contains(owner.resolved_commit.as_str()));
            assert!(message.contains(if same_repository {
                locale::text(language, locale::Message::DirectoryOtherVersion)
            } else {
                locale::text(language, locale::Message::DirectoryOtherRepository)
            }));
        }
    }
}
