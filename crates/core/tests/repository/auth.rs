use super::fixture::Fixture;
use crate::repository::*;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn entry() -> CatalogEntry {
    CatalogEntry {
        path: RepoPath::parse("small-complete.bin").unwrap(),
        size: Bytes::new(262144),
        content_hash: None,
    }
}
#[tokio::test]
async fn head_errors_fall_back_to_range_and_redirect_without_bearer_leak() {
    for (status, redirect) in [
        (405, false),
        (501, false),
        (200, true),
        (405, true),
        (501, true),
    ] {
        let fixture = Fixture::start();
        fixture.control(json!({"head_status":status,"download_mode":if redirect {"redirect"} else {"direct"}})).await;
        let source = fixture.source().await;
        let client = RepositoryClient::new(
            &source.request.endpoint,
            NetworkAccess {
                token: Some(Token::parse("ephemeral_fake_marker").unwrap()),
                proxy: None,
            },
        )
        .unwrap();
        client
            .download_url(&source, &entry(), &CancellationToken::new())
            .await
            .unwrap();
        let state = fixture.state().await;
        let events = state["events"].as_array().unwrap();
        if status != 200 {
            let gets = events
                .iter()
                .filter(|e| {
                    e["role"] == "origin"
                        && e["method"] == "GET"
                        && e["kind"] == "start"
                        && e["path"].as_str().unwrap().contains("/resolve/")
                })
                .collect::<Vec<_>>();
            assert!(!gets.is_empty());
            assert!(gets.iter().all(|e| e["range"] == "bytes=0-0"));
        }
        if redirect {
            let captured = events
                .iter()
                .filter(|e| e["role"] == "capture")
                .collect::<Vec<_>>();
            assert!(!captured.is_empty());
            assert!(captured.iter().all(|e| e["auth_present"] == false));
            let starts = captured
                .iter()
                .filter(|e| e["kind"] == "start")
                .collect::<Vec<_>>();
            assert_eq!(starts.len(), 2);
            assert!(starts.iter().any(|e| e["method"] == "HEAD"));
            if status != 200 {
                assert_eq!(starts[0]["method"], "GET");
                assert_eq!(starts[0]["range"], "bytes=0-0");
            }
            println!(
                "redirect head_status={status} capture_events={}",
                serde_json::to_string(&captured).unwrap()
            );
        }
        fixture.stop().await;
    }
}

#[tokio::test]
async fn bearer_only_download_is_explicitly_unsupported_after_authenticated_success() {
    let fixture = Fixture::start();
    let marker = format!("fake_{}", fixture.ready["port"]);
    fixture
        .control(json!({"download_mode":"bearer","bearer_marker":marker}))
        .await;
    let source = fixture.source().await;
    let client = RepositoryClient::new(
        &source.request.endpoint,
        NetworkAccess {
            token: Some(Token::parse(&marker).unwrap()),
            proxy: None,
        },
    )
    .unwrap();
    let result = client
        .download_url(&source, &entry(), &CancellationToken::new())
        .await;
    assert!(matches!(result, Err(RepositoryError::UnsupportedEndpoint)));
    let state = fixture.state().await;
    let completed = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["method"] == "HEAD" && e["kind"] == "complete")
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0]["status"], 200);
    assert_eq!(completed[1]["status"], 401);
    assert_eq!(completed[0]["auth_present"], true);
    assert_eq!(completed[1]["auth_present"], false);
    assert!(!state.to_string().contains(&marker));
    fixture.stop().await;
}

#[tokio::test]
async fn dataset_metadata_catalog_and_file_urls_use_dataset_routes() {
    let fixture = Fixture::start();
    let mut request = fixture.request();
    request.kind = RepoKind::Dataset;
    let client = fixture.client();
    let cancel = CancellationToken::new();
    let source = client.resolve(&request, &cancel).await.unwrap();
    let (_, pages) = super::catalog::collect(&fixture, source.clone()).await;
    assert_eq!(pages.iter().map(|p| p.entries.len()).sum::<usize>(), 4);
    client
        .download_url(&source, &entry(), &cancel)
        .await
        .unwrap();
    let state = fixture.state().await;
    assert_eq!(state["page2_requests"], 1);
    assert!(state["events"].as_array().unwrap().iter().any(|event| {
        event["method"] == "HEAD"
            && event["path"]
                .as_str()
                .unwrap()
                .starts_with("/datasets/org/model/resolve/")
    }));
    assert!(
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["path"].as_str().unwrap().starts_with("/api/"))
            .all(|e| e["path"].as_str().unwrap().starts_with("/api/datasets/"))
    );
    fixture.stop().await;
}
