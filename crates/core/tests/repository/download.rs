use super::fixture::Fixture;
use crate::repository::*;
use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn special_path_download_is_encoded_and_anonymously_accessible() {
    let fixture = Fixture::start();
    let source = fixture.source().await;
    let entry = CatalogEntry {
        path: RepoPath::parse("special/space # 中文.bin").unwrap(),
        size: Bytes::new(1024),
        content_hash: None,
    };
    let client = RepositoryClient::new(
        &source.request.endpoint,
        NetworkAccess {
            token: Some(Token::parse("fake-token").unwrap()),
            proxy: None,
        },
    )
    .unwrap();
    client
        .download_url(&source, &entry, &CancellationToken::new())
        .await
        .unwrap();
    let mut url = url::Url::parse(fixture.origin()).unwrap();
    url.path_segments_mut().unwrap().extend([
        "org",
        "model",
        "resolve",
        source.resolved_commit.as_str(),
        "special",
        "space # 中文.bin",
    ]);
    let bytes = fixture
        .http
        .get(url)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        bytes.as_ref(),
        (0..1024)
            .map(|n| u8::try_from(n % 256).unwrap())
            .collect::<Vec<_>>()
    );
    let mut digest = Command::new("python")
        .args([
            "-c",
            "import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    digest.stdin.take().unwrap().write_all(&bytes).unwrap();
    let output = digest.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        fixture.ready["fixtures"]["special/space # 中文.bin"]["sha256"]
            .as_str()
            .unwrap()
    );
    let state = fixture.state().await;
    let heads = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["method"] == "HEAD" && e["kind"] == "start")
        .collect::<Vec<_>>();
    assert_eq!(heads.len(), 2);
    assert_eq!(
        heads
            .iter()
            .map(|e| e["auth_present"].as_bool().unwrap())
            .collect::<Vec<_>>(),
        [true, false]
    );
    assert!(heads.iter().all(|e| {
        e["path"]
            .as_str()
            .unwrap()
            .ends_with("special/space # 中文.bin")
    }));
    fixture.stop().await;
}

#[tokio::test]
async fn explicit_authenticated_http_proxy_is_used_for_metadata_and_download_probe() {
    let fixture = Fixture::start();
    let proxy = fixture.ready["proxy"].as_str().unwrap().replacen(
        "http://",
        "http://fake:fake-password@",
        1,
    );
    let request = fixture.request();
    let client = RepositoryClient::new(
        &request.endpoint,
        NetworkAccess {
            token: Some(Token::parse("fake-token").unwrap()),
            proxy: Some(ProxySpec::parse(&proxy).unwrap()),
        },
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let source = client.resolve(&request, &cancel).await.unwrap();
    let entry = CatalogEntry {
        path: RepoPath::parse("small-complete.bin").unwrap(),
        size: Bytes::new(262144),
        content_hash: None,
    };
    client.download_url(&source, &entry, &cancel).await.unwrap();
    let state = fixture.state().await;
    let proxy_events = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["role"] == "proxy" && e["kind"] == "start")
        .collect::<Vec<_>>();
    assert_eq!(proxy_events.len(), 3);
    assert!(proxy_events.iter().all(|e| e["proxy_auth_present"] == true));
    assert!(
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["role"] == "origin")
            .all(|e| e["proxy_auth_present"] == false)
    );
    assert!(!state.to_string().contains("fake-password"));
    assert_eq!(state["page2_requests"], json!(0));
    fixture.stop().await;
}
