use super::{catalog::collect, fixture::Fixture};
use crate::repository::*;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn same_origin_metadata_alias_resolves_commit_and_complete_tree() {
    let fixture = Fixture::start();
    fixture
        .control(json!({"metadata_redirect":"same-origin"}))
        .await;
    let mut request = fixture.request();
    request.repo_id = RepoId::parse("org/alias").unwrap();
    let client = RepositoryClient::new(
        &request.endpoint,
        NetworkAccess {
            token: Some(Token::parse("ephemeral_fake_marker").unwrap()),
            proxy: None,
        },
    )
    .unwrap();
    let source = client
        .resolve(&request, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(source.resolved_commit.as_str(), "a".repeat(40));
    assert_eq!(source.request, request);
    let (result, pages) = collect(&fixture, source).await;
    result.unwrap();
    assert_eq!(
        pages.iter().map(|page| page.entries.len()).sum::<usize>(),
        4
    );
    assert!(pages.last().unwrap().complete);
    let state = fixture.state().await;
    let starts = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "start")
        .collect::<Vec<_>>();
    assert!(starts.iter().all(|event| event["role"] == "origin"));
    let revisions = starts
        .iter()
        .filter(|event| event["path"].as_str().unwrap().contains("/revision/"))
        .collect::<Vec<_>>();
    assert_eq!(revisions.len(), 2);
    assert!(revisions.iter().all(|event| event["auth_present"] == true));
    assert_ne!(revisions[0]["path"], revisions[1]["path"]);
    println!(
        "metadata_same_origin_events={}",
        serde_json::to_string(&starts).unwrap()
    );
    fixture.stop().await;
}

#[tokio::test]
async fn metadata_redirects_cannot_cross_origin_or_loop_forever() {
    for mode in ["cross-origin", "loop", "missing-location"] {
        let fixture = Fixture::start();
        fixture.control(json!({"metadata_redirect":mode})).await;
        let mut request = fixture.request();
        request.repo_id = RepoId::parse("org/alias").unwrap();
        let client = RepositoryClient::new(
            &request.endpoint,
            NetworkAccess {
                token: Some(Token::parse("ephemeral_fake_marker").unwrap()),
                proxy: None,
            },
        )
        .unwrap();
        let result = client.resolve(&request, &CancellationToken::new()).await;
        if mode == "missing-location" {
            assert!(matches!(result, Err(RepositoryError::InvalidResponse)));
        } else {
            assert!(matches!(result, Err(RepositoryError::UnsupportedEndpoint)));
        }
        let state = fixture.state().await;
        let events = state["events"].as_array().unwrap();
        assert!(events.iter().all(|event| event["role"] != "capture"));
        let starts = events
            .iter()
            .filter(|event| event["kind"] == "start")
            .count();
        assert_eq!(starts, if mode == "loop" { 6 } else { 1 });
        println!("metadata_rejected mode={mode} requests={starts} capture_requests=0");
        fixture.stop().await;
    }
}
