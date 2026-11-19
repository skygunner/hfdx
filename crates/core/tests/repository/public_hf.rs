use crate::repository::*;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// Wired only by an explicit temporary probe crate so default tests never require public HTTP.
#[tokio::test]
async fn public_gpt2_metadata_catalog_and_anonymous_config_bytes() {
    let request = RepositoryRequest {
        endpoint: Endpoint::parse("https://huggingface.co", false).unwrap(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("gpt2").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    let client = RepositoryClient::new(&request.endpoint, NetworkAccess::default()).unwrap();
    let cancel = CancellationToken::new();
    let source = client
        .resolve(&request, &cancel)
        .await
        .unwrap_or_else(|error| panic!("BLOCKED public metadata: {error}"));
    let catalog = CatalogRequest {
        id: CatalogId(1),
        source: source.clone(),
    };
    let (tx, mut rx) = mpsc::channel(2);
    let (result, entries) = tokio::join!(client.catalog(&catalog, tx, &cancel), async {
        let mut entries = Vec::new();
        let mut complete = false;
        while let Some(page) = rx.recv().await {
            entries.extend(page.entries);
            complete |= page.complete;
        }
        assert!(complete, "BLOCKED public catalog incomplete");
        entries
    });
    result.unwrap_or_else(|error| panic!("BLOCKED public catalog: {error}"));
    let config = entries
        .iter()
        .find(|entry| entry.path.as_str() == "config.json")
        .expect("public gpt2 catalog must contain config.json");
    let url = client
        .download_url(&source, config, &cancel)
        .await
        .unwrap_or_else(|error| panic!("BLOCKED public URL resolution: {error}"));
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let response = http
        .get(url.url().clone())
        .send()
        .await
        .unwrap_or_else(|_| panic!("BLOCKED anonymous byte fetch: network error"));
    let status = response.status();
    assert!(
        status.is_success(),
        "BLOCKED anonymous byte fetch HTTP {status}"
    );
    let bytes = response.bytes().await.expect("anonymous response body");
    assert_eq!(u64::try_from(bytes.len()).unwrap(), config.size.get());
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["model_type"], "gpt2");
    let mut hash = Command::new("python")
        .args([
            "-c",
            "import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    hash.stdin.take().unwrap().write_all(&bytes).unwrap();
    let hash = hash.wait_with_output().unwrap();
    assert!(hash.status.success());
    println!(
        "PUBLIC_HF_PASS repo=gpt2 commit={} config_bytes={} anonymous_status={} sha256={}",
        source.resolved_commit.as_str(),
        bytes.len(),
        status.as_u16(),
        String::from_utf8(hash.stdout).unwrap().trim()
    );
}
