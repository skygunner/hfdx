use super::fixture::Fixture;
use crate::repository::*;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) async fn collect(
    fixture: &Fixture,
    source: RepositorySpec,
) -> (Result<(), RepositoryError>, Vec<CatalogPage>) {
    let client = fixture.client();
    let request = hfdx_core::Selection::new().begin(source).unwrap();
    let (tx, mut rx) = mpsc::channel(2);
    let cancel = CancellationToken::new();
    let (result, pages) = tokio::join!(client.catalog(&request, tx, &cancel), async {
        let mut pages = Vec::new();
        while let Some(page) = rx.recv().await {
            pages.push(page);
        }
        pages
    });
    (result, pages)
}

async fn logged_load(
    fixture: &Fixture,
    logger: std::sync::Arc<hfdx_core::logging::SessionLogger>,
    root: &std::path::Path,
) -> hfdx_core::task::CoreSession {
    use hfdx_core::{
        storage::SettingsPaths,
        task::{self, CoreCommand, CoreConfig, LoadRequest},
    };
    let mut session = task::spawn_with_services(
        CoreConfig {
            paths: SettingsPaths {
                settings_file: root.join("settings.json"),
            },
            aria2_executable: "aria2c".into(),
        },
        std::sync::Arc::new(|_| {}),
        Some(logger),
    )
    .await
    .unwrap();
    let mut settings = (*session.snapshots.borrow().settings).clone();
    settings.debug_logging = true;
    session
        .handle
        .submit(CoreCommand::SaveSettings(settings))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: fixture.request(),
            access: NetworkAccess {
                token: Some(Token::parse("hf_fake_private_token").unwrap()),
                proxy: None,
            },
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        session.snapshots.wait_for(|s| {
            matches!(
                s.selection.state,
                hfdx_core::selection::CatalogState::Ready
                    | hfdx_core::selection::CatalogState::Failed
            )
        }),
    )
    .await
    .unwrap()
    .unwrap();
    session
}

fn logged_records(root: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .flat_map(|p| {
            std::fs::read_dir(p)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "log"))
                .flat_map(|p| {
                    std::fs::read_to_string(p)
                        .unwrap()
                        .lines()
                        .map(|line| serde_json::from_str(line).unwrap())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
async fn http_retry_and_pagination_events_are_measured_and_sanitized() {
    use hfdx_core::logging::{LogConfig, SessionLogger};
    let fixture = Fixture::start();
    fixture
        .control(json!({"faults":[{"request":1,"action":"429","retry_after":0}]}))
        .await;
    let root = tempfile::tempdir().unwrap();
    let logger =
        std::sync::Arc::new(SessionLogger::new(LogConfig::new(root.path().join("logs"))).unwrap());
    let session = logged_load(&fixture, logger.clone(), root.path()).await;
    assert!(session.snapshots.borrow().error.is_none());
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    logger.flush().unwrap();
    let values = logged_records(&root.path().join("logs"));
    let pages: Vec<_> = values
        .iter()
        .filter(|v| v["event"]["event"] == "pagination")
        .collect();
    assert_eq!(
        pages
            .iter()
            .map(|v| v["event"]["page"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2]
    );
    let retry: Vec<_> = values
        .iter()
        .filter(|v| v["event"]["event"] == "retry" && v["event"]["retry"].is_number())
        .collect();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0]["event"]["retry"], 1);
    assert_eq!(retry[0]["event"]["outcome"], "rate_limited");
    for record in pages.into_iter().chain(retry) {
        assert!(record["event"]["elapsed_ms"].is_number());
        assert!(record["event"].get("bytes").is_none());
        let serialized = record.to_string();
        assert!(!serialized.contains("hf_fake_private_token"));
        assert!(!serialized.contains(fixture.origin()));
    }
    fixture.stop().await;
}

#[tokio::test]
async fn logging_failure_is_visible_without_failing_catalog() {
    use hfdx_core::logging::{LogConfig, LogError, LogStatus, SessionLogger};
    let fixture = Fixture::start();
    fixture
        .control(json!({"faults":[{"request":1,"action":"429","retry_after":0}]}))
        .await;
    let root = tempfile::tempdir().unwrap();
    let logger = std::sync::Arc::new(
        SessionLogger::new(LogConfig::new(root.path().join("logs")).with_limits(
            std::num::NonZeroU64::new(1024).unwrap(),
            std::num::NonZeroU64::new(100_000).unwrap(),
        ))
        .unwrap(),
    );
    let log_dir = std::fs::read_dir(root.path().join("logs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    std::fs::create_dir(log_dir.join("events_r00000.log")).unwrap();
    let session = logged_load(&fixture, logger.clone(), root.path()).await;
    assert!(session.snapshots.borrow().error.is_none());
    assert_eq!(logger.status(), LogStatus::Failed(LogError::Rotation));
    assert_eq!(
        session.snapshots.borrow().diagnostics.logging,
        Some(LogStatus::Failed(LogError::Rotation))
    );
    assert_eq!(fixture.state().await["page2_requests"], 2);
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn catalog_is_complete_and_pinned_when_branch_changes() {
    let fixture = Fixture::start();
    let source = fixture.source().await;
    assert_eq!(source.resolved_commit.as_str(), "a".repeat(40));
    fixture.control(json!({"branch":"b"})).await;
    let (result, pages) = collect(&fixture, source).await;
    result.unwrap();
    assert_eq!(
        pages
            .iter()
            .map(|p| (p.entries.len(), p.complete))
            .collect::<Vec<_>>(),
        [(2, false), (2, true)]
    );
    assert_eq!(
        pages
            .iter()
            .flat_map(|p| &p.entries)
            .map(|e| (e.path.as_str(), e.size.get()))
            .collect::<Vec<_>>(),
        [
            ("small-complete.bin", 262144),
            ("large-partial.bin", 33554432),
            ("empty.bin", 0),
            ("special/space # 中文.bin", 1024)
        ]
    );
    let state = fixture.state().await;
    assert_eq!(state["page2_requests"], 1);
    assert!(
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["path"].as_str().unwrap().contains("/tree/"))
            .all(|e| e["path"].as_str().unwrap().ends_with(&"a".repeat(40)))
    );
    fixture.stop().await;
}

#[tokio::test]
async fn catalog_retries_429_500_and_connection_close_without_losing_files() {
    for fault in ["429", "500", "close"] {
        let fixture = Fixture::start();
        fixture
            .control(json!({"faults":[{"request":1,"action":fault,"retry_after":0}]}))
            .await;
        let (result, pages) = collect(&fixture, fixture.source().await).await;
        result.unwrap();
        assert_eq!(pages.iter().map(|p| p.entries.len()).sum::<usize>(), 4);
        assert!(pages.last().unwrap().complete);
        assert_eq!(fixture.state().await["page2_requests"], 2, "{fault}");
        fixture.stop().await;
    }
}

#[tokio::test]
async fn exhausted_retries_do_not_publish_a_complete_catalog() {
    let fixture = Fixture::start();
    fixture
        .control(
            json!({"faults":[{"request":1,"action":"500"},{"request":2,"action":"500"},
        {"request":3,"action":"500"},{"request":4,"action":"500"}]}),
        )
        .await;
    let (result, pages) = collect(&fixture, fixture.source().await).await;
    assert!(matches!(
        result,
        Err(RepositoryError::Http { status: 500, .. })
    ));
    assert_eq!(pages.len(), 1);
    assert!(!pages[0].complete);
    assert_eq!(fixture.state().await["page2_requests"], 4);
    fixture.stop().await;
}

#[tokio::test]
async fn cancellation_interrupts_retry_after_and_never_finishes_catalog() {
    let fixture = Fixture::start();
    fixture
        .control(json!({"faults":[{"request":1,"action":"429","retry_after":3600}]}))
        .await;
    let source = fixture.source().await;
    let path = format!(
        "/api/models/org/model/tree/{}",
        source.resolved_commit.as_str()
    );
    let client = fixture.client();
    let request = hfdx_core::Selection::new().begin(source).unwrap();
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel::<CatalogPage>(2);
    let watch = async {
        let first = rx.recv().await.unwrap();
        assert!(!first.complete);
        let event = fixture.event_status(&path, 429).await;
        assert_eq!(event["status"], 429);
        cancel.cancel();
        assert!(rx.recv().await.is_none());
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        tokio::join!(client.catalog(&request, tx, &cancel), watch)
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(RepositoryError::Cancelled)));
    fixture.stop().await;
}

#[tokio::test]
async fn client_rejects_a_different_source_before_sending_its_token() {
    let fixture = Fixture::start();
    let mut request = fixture.request();
    request.endpoint = Endpoint::parse(fixture.ready["capture"].as_str().unwrap(), true).unwrap();
    let client = RepositoryClient::new(
        &fixture.request().endpoint,
        NetworkAccess {
            token: Some(Token::parse("fake-token").unwrap()),
            proxy: None,
        },
    )
    .unwrap();
    assert!(matches!(
        client.resolve(&request, &CancellationToken::new()).await,
        Err(RepositoryError::UnsupportedEndpoint)
    ));
    assert_eq!(fixture.state().await["events"], json!([]));
    fixture.stop().await;
}
