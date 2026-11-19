use super::{catalog::collect, fixture::Fixture};
use crate::repository::*;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn identical_duplicates_are_removed_but_conflicts_never_complete() {
    for mode in ["duplicate", "conflict", "empty", "empty-terminal"] {
        let fixture = Fixture::start();
        fixture.control(json!({"tree_mode":mode})).await;
        let (result, pages) = collect(&fixture, fixture.source().await).await;
        let count = pages.iter().map(|p| p.entries.len()).sum::<usize>();
        match mode {
            "duplicate" => {
                result.unwrap();
                assert_eq!(count, 4);
                assert!(pages.last().unwrap().complete);
            }
            "conflict" => {
                assert!(matches!(result, Err(RepositoryError::InvalidResponse)));
                assert_eq!(count, 2);
                assert!(pages.iter().all(|p| !p.complete));
            }
            "empty" => {
                result.unwrap();
                assert_eq!(count, 0);
                assert_eq!(pages.len(), 1);
                assert!(pages[0].complete);
            }
            "empty-terminal" => {
                result.unwrap();
                assert_eq!(count, 2);
                assert_eq!(pages.len(), 2);
                assert!(pages[1].entries.is_empty());
                assert!(pages[1].complete);
            }
            _ => unreachable!(),
        }
        fixture.stop().await;
    }
}

#[tokio::test]
async fn cross_origin_pagination_is_rejected_without_a_capture_request() {
    let fixture = Fixture::start();
    fixture.control(json!({"link_mode":"cross-origin"})).await;
    let (result, pages) = collect(&fixture, fixture.source().await).await;
    assert!(matches!(result, Err(RepositoryError::UnsupportedEndpoint)));
    assert!(pages.iter().all(|p| !p.complete));
    let state = fixture.state().await;
    assert!(
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["role"] != "capture")
    );
    assert_eq!(state["page2_requests"], 0);
    fixture.stop().await;
}

#[tokio::test]
async fn http_date_retry_after_can_be_cancelled_after_observed_429() {
    let fixture = Fixture::start();
    fixture.control(json!({"faults":[{"request":1,"action":"429","retry_after":"Wed, 21 Oct 2037 07:28:00 GMT"}]})).await;
    let source = fixture.source().await;
    let path = format!(
        "/api/models/org/model/tree/{}",
        source.resolved_commit.as_str()
    );
    let request = hfdx_core::Selection::new().begin(source).unwrap();
    let client = fixture.client();
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel::<CatalogPage>(2);
    let watch = async {
        assert!(!rx.recv().await.unwrap().complete);
        assert_eq!(fixture.event_status(&path, 429).await["status"], 429);
        cancel.cancel();
        assert!(rx.recv().await.is_none());
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        tokio::join!(client.catalog(&request, tx, &cancel), watch)
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(RepositoryError::Cancelled)));
    assert_eq!(fixture.state().await["page2_requests"], 1);
    fixture.stop().await;
}

#[tokio::test]
async fn authorization_and_not_found_errors_keep_source_status_without_retry() {
    for status in [401, 403, 404] {
        let fixture = Fixture::start();
        let mut request = fixture.request();
        if status == 404 {
            request.requested_revision = Revision::parse("missing").unwrap();
        } else {
            fixture.control(json!({"api_status":status})).await;
        }
        let result = fixture
            .client()
            .resolve(&request, &CancellationToken::new())
            .await;
        assert!(
            matches!(result, Err(RepositoryError::Http { status: found, endpoint }) if found == status && endpoint == request.endpoint)
        );
        assert_eq!(
            fixture.state().await["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "start")
                .count(),
            1
        );
        fixture.stop().await;
    }
}
