use hfdx_core::{
    CatalogState, DownloadOptions, Endpoint, NetworkAccess, RepoId, RepoKind, RepositoryRequest,
    Revision, SelectionChange, SettingsPaths,
    task::{self, CoreCommand, CoreConfig, LoadRequest, StartRequest, UiPhase},
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

#[tokio::test]
async fn pause_cancels_url_resolution_before_server_releases_head() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (head_tx, head_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut head_tx = Some(head_tx);
        let mut release_rx = Some(release_rx);
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = tokio::time::timeout(Duration::from_secs(10), async {
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    assert!(headers.len() < 8192, "request headers exceed 8192 bytes");
                    headers.push(
                        stream
                            .read_u8()
                            .await
                            .expect("EOF or error in request headers"),
                    );
                }
                String::from_utf8(headers).expect("invalid request headers")
            })
            .await
            .expect("timed out reading request headers");
            if request.starts_with("HEAD ") {
                head_tx.take().unwrap().send(()).unwrap();
                release_rx.take().unwrap().await.unwrap();
                break;
            }
            assert!(
                request.starts_with("GET /api/"),
                "unexpected request: {request}"
            );
            let body = if request.contains("/revision/") {
                format!("{{\"sha\":\"{}\"}}", "a".repeat(40))
            } else {
                "[{\"type\":\"file\",\"path\":\"held.bin\",\"size\":8}]".to_owned()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut session = task::spawn(CoreConfig {
        paths: SettingsPaths {
            settings_file: dir.path().join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    })
    .await
    .unwrap();
    let request = RepositoryRequest {
        endpoint: Endpoint::parse(&format!("http://{address}"), true).unwrap(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("org/model").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    session
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: request,
            access: NetworkAccess::default(),
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        session
            .snapshots
            .wait_for(|s| s.selection.state == CatalogState::Ready),
    )
    .await
    .unwrap()
    .unwrap();
    let catalog = session
        .snapshots
        .borrow()
        .selection
        .catalog
        .as_ref()
        .unwrap()
        .id;
    session
        .handle
        .submit(CoreCommand::ChangeSelection(SelectionChange::SelectMatches))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let start = session
        .handle
        .submit(CoreCommand::Start(StartRequest {
            catalog,
            destination: dir.path().join("download"),
            options: DownloadOptions::new(1, 1).unwrap(),
            verify_after_download: true,
        }))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), head_rx)
        .await
        .unwrap()
        .unwrap();
    let pause = session.handle.submit(CoreCommand::Pause).unwrap();
    tokio::time::timeout(Duration::from_secs(10), start)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), pause)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(session.snapshots.borrow().phase, UiPhase::Paused);
    let persisted: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("download/.hfdx/task.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(persisted["state"], "paused");
    release_tx.send(()).unwrap();
    server.await.unwrap();
    session.handle.request_shutdown().unwrap();
    let receipt = session.join().await.unwrap();
    assert!(receipt.child_pid.is_none());
    assert!(!dir.path().join("download/held.bin").exists());
}
