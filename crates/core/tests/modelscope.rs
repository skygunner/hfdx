use hfdx_core::{
    repository::{
        Endpoint, NetworkAccess, RepoId, RepoKind, RepositoryRequest, Revision, parse_repository,
    },
    selection::{CatalogState, SelectionChange},
    storage::{DownloadOptions, SettingsPaths, TaskState, VerificationStatus},
    task::{self, CoreCommand, CoreConfig, LoadRequest, StartRequest},
};
use std::time::Duration;

#[test]
fn modelscope_urls_select_platform_and_default_revision_without_changing_hf() {
    let defaults = RepositoryRequest {
        endpoint: Endpoint::parse("https://huggingface.co", false).unwrap(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("placeholder").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    for suffix in ["", "/", "/summary", "/files"] {
        let request = parse_repository(
            &format!("https://modelscope.ai/models/Qwen/Qwen2.5-0.5B{suffix}"),
            &defaults,
        )
        .unwrap();
        assert!(request.endpoint.is_modelscope());
        assert_eq!(request.repo_id.as_str(), "Qwen/Qwen2.5-0.5B");
        assert_eq!(request.requested_revision.as_str(), "master");
    }
    let explicit = RepositoryRequest {
        requested_revision: Revision::parse("release/v1").unwrap(),
        ..defaults.clone()
    };
    assert_eq!(
        parse_repository("https://modelscope.ai/models/Qwen/model", &explicit)
            .unwrap()
            .requested_revision
            .as_str(),
        "release/v1"
    );
    assert_eq!(
        parse_repository("Qwen/model", &defaults)
            .unwrap()
            .requested_revision
            .as_str(),
        "main"
    );
    for url in [
        "https://modelscope.ai/models/Qwen/model/files/file.bin",
        "https://modelscope.ai/models/Qwen/model?token=secret",
        "https://modelscope.ai/models/Qwen/../other",
    ] {
        assert!(parse_repository(url, &defaults).is_err());
    }
}

#[tokio::test]
#[ignore = "explicit public ModelScope small-file smoke test; requires internet and aria2c"]
async fn public_modelscope_model_downloads_through_shared_task_core_and_verifies_sha256() {
    let root = tempfile::tempdir().unwrap();
    let mut session = task::spawn(CoreConfig {
        paths: SettingsPaths {
            settings_file: root.path().join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    })
    .await
    .unwrap();
    let handle = session.handle.clone();
    let run = async {
        let request = RepositoryRequest {
            endpoint: Endpoint::parse("https://modelscope.ai", false).unwrap(),
            kind: RepoKind::Model,
            repo_id: RepoId::parse("Qwen/Qwen2.5-0.5B").unwrap(),
            requested_revision: Revision::parse("master").unwrap(),
        };
        handle
            .submit(CoreCommand::Load(LoadRequest {
                repository: request,
                access: NetworkAccess::default(),
            }))
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        session
            .snapshots
            .wait_for(|s| s.selection.state == CatalogState::Ready || s.error.is_some())
            .await
            .unwrap();
        let snapshot = session.snapshots.borrow().clone();
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        let catalog = snapshot.selection.catalog.as_ref().unwrap().id;
        let entry_id = *snapshot
            .selection
            .visible
            .iter()
            .find(|id| {
                snapshot
                    .selection
                    .catalog
                    .as_ref()
                    .unwrap()
                    .entry(**id)
                    .unwrap()
                    .path
                    .as_str()
                    == "config.json"
            })
            .unwrap();
        handle
            .submit(CoreCommand::SetFilter("config.json".into()))
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        handle
            .submit(CoreCommand::ChangeSelection(SelectionChange::Set {
                id: entry_id,
                selected: true,
            }))
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        handle
            .submit(CoreCommand::Start(StartRequest {
                catalog,
                destination: root.path().join("model"),
                options: DownloadOptions::new(1, 1).unwrap(),
                verify_after_download: true,
            }))
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        session
            .snapshots
            .wait_for(|s| {
                s.task.as_ref().is_some_and(|t| {
                    matches!(t.record.state, TaskState::Completed | TaskState::Failed)
                }) || s.error.is_some()
            })
            .await
            .unwrap();
        let snapshot = session.snapshots.borrow().clone();
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        let task = snapshot.task.as_ref().unwrap();
        assert_eq!(
            task.record.state,
            TaskState::Completed,
            "{:?}",
            task.progress.failed
        );
        assert_eq!(task.record.files.len(), 1);
        assert_eq!(
            task.record.files[0].verification,
            VerificationStatus::Passed
        );
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("model/config.json")).unwrap())
                .unwrap();
        assert_eq!(config["model_type"], "qwen2");
        println!(
            "ModelScope commit={} file=config.json bytes={} SHA256=verified",
            task.record.source.resolved_commit.as_str(),
            task.record.files[0].entry.size.get()
        );
    };
    let result = tokio::time::timeout(Duration::from_secs(120), run).await;
    handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    result.unwrap();
}
