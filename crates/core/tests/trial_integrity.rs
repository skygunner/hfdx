// allow: SIZE_OK - Wire fixture and public worker lifecycle regressions share the authorized integration test.
use hfdx_core::{
    CatalogState, DownloadOptions, Endpoint, NetworkAccess, RepoId, RepoKind, RepositoryRequest,
    Revision, SelectionChange, SettingsPaths, TaskState,
    storage::{TaskRecord, VerificationStatus},
    task::{
        self, CompletionNotice, CompletionOutcome, CoreCommand, CoreConfig, CoreSession,
        LoadRequest, StartRequest,
    },
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
const SHA: &str = "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb";
const EMPTY_GIT: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
struct Wire {
    corrected: AtomicBool,
    gets: [AtomicUsize; 4],
}
impl Wire {
    async fn serve(&self, mut stream: TcpStream) {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            if request.len() > 8192 {
                panic!("headers too large");
            }
            request.push(stream.read_u8().await.unwrap());
        }
        let request = String::from_utf8(request).unwrap();
        let first = request.lines().next().unwrap();
        let path = first.split_whitespace().nth(1).unwrap();
        let head = first.starts_with("HEAD ");
        let body = if path.contains("/revision/") {
            format!("{{\"sha\":\"{}\"}}", "a".repeat(40)).into_bytes()
        } else if path.contains("/tree/") {
            serde_json::to_vec(&serde_json::json!([
                {"type":"file","path":"sha.bin","size":1,"lfs":{"oid":SHA,"size":1}},
                {"type":"file","path":"empty.bin","size":0,"oid":EMPTY_GIT},
                {"type":"file","path":"bad.bin","size":1,"lfs":{"oid":SHA,"size":1}},
                {"type":"file","path":"missing.bin","size":1}
            ]))
            .unwrap()
        } else {
            let name = path.rsplit('/').next().unwrap();
            let index = ["sha.bin", "empty.bin", "bad.bin", "missing.bin"]
                .iter()
                .position(|item| *item == name)
                .unwrap();
            if !head {
                self.gets[index].fetch_add(1, Ordering::SeqCst);
            }
            match index {
                1 => Vec::new(),
                2 if !self.corrected.load(Ordering::SeqCst) => b"b".to_vec(),
                _ => b"a".to_vec(),
            }
        };
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        if !head {
            stream.write_all(&body).await.unwrap();
        }
    }
}
async fn fixture() -> (
    String,
    Arc<Wire>,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Wire {
        corrected: AtomicBool::new(false),
        gets: std::array::from_fn(|_| AtomicUsize::new(0)),
    });
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let wire = state.clone();
    let server = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                () = stop.cancelled() => break,
                connection = listener.accept() => {
                    let (stream, _) = connection.unwrap();
                    let wire = wire.clone();
                    clients.spawn(async move { wire.serve(stream).await });
                },
                result = clients.join_next(), if !clients.is_empty() => { result.unwrap().unwrap(); }
            }
        }
        while let Some(result) = clients.join_next().await {
            result.unwrap();
        }
    });
    (origin, state, cancel, server)
}
fn config(root: &Path) -> CoreConfig {
    CoreConfig {
        paths: SettingsPaths {
            settings_file: root.join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    }
}
async fn command(session: &CoreSession, command: CoreCommand) {
    tokio::time::timeout(
        Duration::from_secs(15),
        session.handle.submit(command).unwrap(),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
}
async fn terminal(session: &mut CoreSession) {
    tokio::time::timeout(
        Duration::from_secs(20),
        session.snapshots.wait_for(|snapshot| {
            snapshot.error.is_some()
                || snapshot.task.as_ref().is_some_and(|task| {
                    matches!(task.record.state, TaskState::Completed | TaskState::Failed)
                })
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        session.snapshots.borrow().error.is_none(),
        "{:?}",
        session.snapshots.borrow().error
    );
}
fn notice_sink() -> (
    task::CompletionCallback,
    mpsc::UnboundedReceiver<CompletionNotice>,
) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (
        Arc::new(move |notice| {
            sender.send(notice).unwrap();
        }),
        receiver,
    )
}
#[tokio::test]
async fn real_aria2_mixed_hash_results_retry_only_integrity_failed_and_notice_once() {
    let temp = tempfile::tempdir().unwrap();
    let (origin, wire, cancel, server) = fixture().await;
    let log_root = temp.path().join("logs");
    let logger = Arc::new(
        hfdx_core::logging::SessionLogger::new(hfdx_core::logging::LogConfig::new(
            log_root.clone(),
        ))
        .unwrap(),
    );
    let (sink, mut notices) = notice_sink();
    let mut session = task::spawn_with_services(config(temp.path()), sink, Some(logger.clone()))
        .await
        .unwrap();
    command(
        &session,
        CoreCommand::Load(LoadRequest {
            repository: RepositoryRequest {
                endpoint: Endpoint::parse(&origin, true).unwrap(),
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            access: NetworkAccess::default(),
        }),
    )
    .await;
    tokio::time::timeout(
        Duration::from_secs(10),
        session
            .snapshots
            .wait_for(|snapshot| snapshot.selection.state == CatalogState::Ready),
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
    command(
        &session,
        CoreCommand::ChangeSelection(SelectionChange::SelectMatches),
    )
    .await;
    let destination = temp.path().join("download");
    command(
        &session,
        CoreCommand::Start(StartRequest {
            catalog,
            destination: destination.clone(),
            options: DownloadOptions::new(1, 1).unwrap(),
            verify_after_download: true,
        }),
    )
    .await;
    terminal(&mut session).await;
    let first = notices.try_recv().unwrap();
    assert_eq!(
        (first.outcome, first.passed, first.unavailable, first.failed),
        (CompletionOutcome::IntegrityFailed, 2, 1, 1)
    );
    let disk: TaskRecord =
        serde_json::from_slice(&std::fs::read(destination.join(".hfdx/task.json")).unwrap())
            .unwrap();
    assert_eq!(disk.state, TaskState::Failed);
    assert_eq!(
        disk.files
            .iter()
            .filter(|file| file.verification == VerificationStatus::Failed)
            .count(),
        1
    );
    let before = wire
        .gets
        .each_ref()
        .map(|count| count.load(Ordering::SeqCst));
    command(&session, CoreCommand::Resume).await;
    command(&session, CoreCommand::SetSelectedOnly(false)).await;
    assert!(notices.try_recv().is_err());
    wire.corrected.store(true, Ordering::SeqCst);
    command(&session, CoreCommand::RetryFailed).await;
    terminal(&mut session).await;
    let second = notices.try_recv().unwrap();
    assert_eq!(
        (
            second.outcome,
            second.passed,
            second.unavailable,
            second.failed
        ),
        (CompletionOutcome::Unverified, 3, 1, 0)
    );
    assert_eq!(second.epoch, first.epoch + 1);
    assert_eq!(
        wire.gets
            .each_ref()
            .map(|count| count.load(Ordering::SeqCst)),
        [before[0], before[1], before[2] + 1, before[3]]
    );
    command(&session, CoreCommand::Resume).await;
    assert!(notices.try_recv().is_err());
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    logger.flush().unwrap();
    let log_file = std::fs::read_dir(&log_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("events_rCURRENT.log");
    let text = std::fs::read_to_string(log_file).unwrap();
    let records: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let outcomes: Vec<_> = records
        .iter()
        .filter(|record| record["event"]["event"] == "verification")
        .map(|record| record["event"]["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["integrity_failed", "unavailable"]);
    assert!(
        records
            .iter()
            .any(|record| record["event"]["event"] == "metadata"
                && record["event"]["outcome"] == "succeeded")
    );
    assert!(
        records
            .iter()
            .any(|record| record["event"]["event"] == "shutdown")
    );
    assert!(!text.contains(&origin));
    assert_eq!(logger.status(), hfdx_core::logging::LogStatus::Ready);
    let (sink, mut notices) = notice_sink();
    let reopened = task::spawn_with_completion(config(temp.path()), sink)
        .await
        .unwrap();
    command(
        &reopened,
        CoreCommand::OpenTask(destination.join(".hfdx/task.json")),
    )
    .await;
    assert!(notices.try_recv().is_err());
    reopened.handle.request_shutdown().unwrap();
    reopened.join().await.unwrap();
    cancel.cancel();
    server.await.unwrap();
}

fn saved_task(root: &Path, policy: Option<bool>, hash: Option<&str>) -> std::path::PathBuf {
    let destination = root.join("download");
    std::fs::create_dir_all(destination.join(".hfdx")).unwrap();
    std::fs::write(destination.join("a.bin"), b"a").unwrap();
    let mut value = serde_json::json!({
        "schema_version":1,"task_id":"0123456789abcdef0123456789abcdef",
        "source":{"request":{"endpoint":{"url":"http://127.0.0.1:9/","allow_http":true},"kind":"model","repo_id":"org/model","requested_revision":"main"},"resolved_commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
        "files":[{"entry":{"path":"a.bin","size":1,"content_hash":hash.map(|hex| serde_json::json!({"algorithm":"sha256","hex":hex}))},"completed":true,"failure":null}],
        "options":{"connections_per_file":1,"concurrent_files":1},"proxy_address":null,
        "state":"completed","created_unix_ms":1,"updated_unix_ms":1
    });
    if let Some(policy) = policy {
        value["verify_after_download"] = policy.into();
    }
    let path = destination.join(".hfdx/task.json");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    path
}
#[tokio::test]
async fn reopened_paused_task_verifies_completed_file_without_downloading() {
    let temp = tempfile::tempdir().unwrap();
    let path = saved_task(temp.path(), Some(true), Some(SHA));
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["state"] = "paused".into();
    value["files"][0]["verification"] = "pending".into();
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let file = temp.path().join("download/a.bin");
    let original = std::fs::read(&file).unwrap();
    let (sender, mut notices) = mpsc::unbounded_channel();
    let notice_path = path.clone();
    let sink = Arc::new(move |notice| {
        let disk: TaskRecord =
            serde_json::from_slice(&std::fs::read(&notice_path).unwrap()).unwrap();
        sender.send((notice, disk)).unwrap();
    });
    let mut offline_config = config(temp.path());
    offline_config.aria2_executable = temp.path().join("nonexistent-aria2");
    let session = task::spawn_with_completion(offline_config, sink)
        .await
        .unwrap();
    command(&session, CoreCommand::OpenTask(path)).await;
    assert!(notices.try_recv().is_err());
    command(&session, CoreCommand::Resume).await;
    let (notice, disk) = tokio::time::timeout(Duration::from_secs(20), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice.outcome, CompletionOutcome::Verified);
    assert_eq!(
        (notice.passed, notice.unavailable, notice.failed),
        (1, 0, 0)
    );
    assert_eq!(disk.state, TaskState::Completed);
    assert_eq!(disk.files[0].verification, VerificationStatus::Passed);
    assert_eq!(std::fs::read(file).unwrap(), original);
    assert!(notices.try_recv().is_err());
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn retry_after_verification_io_failure_reuses_downloaded_file() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let path = saved_task(temp.path(), Some(true), Some(SHA));
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["state"] = "paused".into();
    value["files"][0]["verification"] = "pending".into();
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let file = temp.path().join("download/a.bin");
    let permissions = std::fs::metadata(&file).unwrap().permissions();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o0)).unwrap();

    let (sink, mut notices) = notice_sink();
    let mut offline_config = config(temp.path());
    offline_config.aria2_executable = temp.path().join("nonexistent-aria2");
    let mut session = task::spawn_with_completion(offline_config, sink)
        .await
        .unwrap();
    command(&session, CoreCommand::OpenTask(path.clone())).await;
    command(&session, CoreCommand::Resume).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        session.snapshots.wait_for(|snapshot| {
            snapshot
                .task
                .as_ref()
                .is_some_and(|task| task.record.state == TaskState::Failed)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let failed: TaskRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(failed.files[0].completed);
    assert_eq!(failed.files[0].verification, VerificationStatus::Pending);
    assert_eq!(
        failed.files[0].failure.as_ref().unwrap().kind,
        hfdx_core::storage::FailureKind::Disk
    );

    std::fs::set_permissions(&file, permissions).unwrap();
    command(&session, CoreCommand::RetryFailed).await;
    let notice = tokio::time::timeout(Duration::from_secs(10), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice.outcome, CompletionOutcome::Verified);
    let done: TaskRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(done.state, TaskState::Completed);
    assert_eq!(done.files[0].verification, VerificationStatus::Passed);
    assert!(done.files[0].failure.is_none());
    assert_eq!(std::fs::read(file).unwrap(), b"a");
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[tokio::test]
async fn legacy_completed_requires_explicit_offline_verification_without_open_notice() {
    let temp = tempfile::tempdir().unwrap();
    let path = saved_task(temp.path(), None, Some(SHA));
    let original = std::fs::read(&path).unwrap();
    let (sink, mut notices) = notice_sink();
    let mut session = task::spawn_with_completion(config(temp.path()), sink)
        .await
        .unwrap();
    command(&session, CoreCommand::OpenTask(path.clone())).await;
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        session
            .snapshots
            .borrow()
            .task
            .as_ref()
            .unwrap()
            .record
            .files[0]
            .verification,
        VerificationStatus::LegacyUnverified
    );
    assert!(notices.try_recv().is_err());
    command(&session, CoreCommand::VerifyExisting).await;
    terminal(&mut session).await;
    assert_eq!(
        notices.try_recv().unwrap().outcome,
        CompletionOutcome::Verified
    );
    let disk: TaskRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(disk.verify_after_download, Some(true));
    assert_eq!(disk.files[0].verification, VerificationStatus::Passed);
    command(&session, CoreCommand::VerifyExisting).await;
    assert!(notices.try_recv().is_err());
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}
#[tokio::test]
async fn disabled_policy_finishes_without_hashing_and_rejects_verify_existing() {
    let temp = tempfile::tempdir().unwrap();
    let path = saved_task(temp.path(), Some(false), Some(&"0".repeat(64)));
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["state"] = "paused".into();
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let (sink, mut notices) = notice_sink();
    let mut session = task::spawn_with_completion(config(temp.path()), sink)
        .await
        .unwrap();
    command(&session, CoreCommand::OpenTask(path)).await;
    command(&session, CoreCommand::Resume).await;
    terminal(&mut session).await;
    assert_eq!(
        notices.try_recv().unwrap().outcome,
        CompletionOutcome::VerificationDisabled
    );
    let result = session
        .handle
        .submit(CoreCommand::VerifyExisting)
        .unwrap()
        .await
        .unwrap();
    assert!(matches!(result, Err(task::CoreError::InvalidState { .. })));
    assert!(notices.try_recv().is_err());
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}
