use hfdx_core::{
    CatalogState, DownloadOptions, Endpoint, NetworkAccess, RepoId, RepoKind, RepositoryRequest,
    Revision, SelectionChange, SettingsPaths,
    task::{self, CoreCommand, CoreConfig, LoadRequest, StartRequest},
};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    task::JoinSet,
};

#[tokio::test]
async fn real_aria2_submissions_remain_within_window() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let held = Arc::new(Notify::new());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let resolved = Arc::new(Mutex::new(BTreeSet::new()));
    let server = tokio::spawn({
        let held = held.clone();
        let shutdown = shutdown.clone();
        let resolved = resolved.clone();
        async move {
            let mut children = JoinSet::new();
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    Some(result) = children.join_next(), if !children.is_empty() => result.unwrap(),
                    accepted = listener.accept() => {
                        let (mut stream, _) = accepted.unwrap();
                        let resolved=resolved.clone(); let held=held.clone();
                        children.spawn(async move {
                            let request = tokio::time::timeout(Duration::from_secs(10), async {
                                let mut headers = Vec::new();
                                while !headers.ends_with(b"\r\n\r\n") {
                                    assert!(headers.len() < 8192, "request headers exceed 8192 bytes");
                                    headers.push(stream.read_u8().await.expect("EOF or error in request headers"));
                                }
                                String::from_utf8(headers).expect("invalid request headers")
                            }).await.expect("timed out reading request headers");
                            if request.starts_with("GET /api/") {
                                let body=if request.contains("/revision/") { format!("{{\"sha\":\"{}\"}}", "a".repeat(40)) } else { serde_json::to_string(&(0..40).map(|i| serde_json::json!({"type":"file","path":format!("item-{i:02}.bin"),"size":1048576})).collect::<Vec<_>>()).unwrap() };
                                let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
                                stream.write_all(response.as_bytes()).await.unwrap();
                            } else if request.starts_with("HEAD ") {
                                let path = request.split_whitespace().nth(1).expect("HEAD request path");
                                resolved.lock().unwrap().insert(path.to_owned());
                                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n").await.unwrap();
                            } else if request.starts_with("GET /org/") { held.notified().await; }
                            else { panic!("unexpected request: {request}"); }
                        });
                    }
                }
            }
            children.abort_all();
            while let Some(result) = children.join_next().await {
                if let Err(error) = result {
                    assert!(error.is_cancelled(), "HTTP handler failed: {error}");
                }
            }
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
    session
        .handle
        .submit(CoreCommand::Start(StartRequest {
            catalog,
            destination: dir.path().join("download"),
            options: DownloadOptions::new(1, 1).unwrap(),
            verify_after_download: true,
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|task| task.progress.active.len() == 1)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    {
        let snapshot = session.snapshots.borrow();
        let task = snapshot.task.as_ref().unwrap();
        let active: BTreeSet<_> = task.progress.active.iter().map(|file| file.index).collect();
        assert_eq!(active.len(), 1, "aria2 jobs must represent distinct files");
        assert_eq!(task.record.files.len(), 40);
        assert!(
            task.record
                .files
                .iter()
                .all(|file| !file.completed && file.failure.is_none())
        );
        let pending: BTreeSet<_> = (0..task.record.files.len())
            .filter(|index| !active.contains(index))
            .collect();
        assert_eq!(pending.len(), 39);
        let expected: BTreeSet<_> = active
            .iter()
            .map(|index| format!("/org/model/resolve/{}/item-{index:02}.bin", "a".repeat(40)))
            .collect();
        assert_eq!(
            *resolved.lock().unwrap(),
            expected,
            "only submitted files should have resolved URLs"
        );
    }
    println!(
        "40 nonempty files: 1 distinct aria2 job, 39 pending; URL resolution bounded to those jobs"
    );
    session.handle.request_shutdown().unwrap();
    let receipt = session.join().await.unwrap();
    println!("shutdown: {receipt:?}");
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("fixture server shutdown timed out")
        .unwrap();
}
