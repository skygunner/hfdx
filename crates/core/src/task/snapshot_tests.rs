// allow: SIZE_OK - Worker ownership regressions and manifest fixture share the authorized test module.
use super::*;
use crate::{
    repository::{
        Bytes, CatalogEntry, CommitId, ContentHash, Endpoint, HashAlgorithm, RepoId, RepoKind,
        RepoPath, RepositoryRequest, RepositorySpec, Revision,
    },
    storage::{DownloadOptions, SettingsPaths},
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

type Snapshots = watch::Receiver<Arc<AppSnapshot>>;
async fn worker_fixture(count: usize) -> (Worker, Snapshots, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let paths = SettingsPaths {
        settings_file: temp.path().join("settings.json"),
    };
    let settings = storage::load_settings(&paths).await.unwrap();
    let source = RepositorySpec {
        request: RepositoryRequest {
            endpoint: Endpoint::parse("http://127.0.0.1:12345", true).unwrap(),
            kind: RepoKind::Model,
            repo_id: RepoId::parse("org/model").unwrap(),
            requested_revision: Revision::parse("main").unwrap(),
        },
        resolved_commit: CommitId::parse(&"a".repeat(40)).unwrap(),
    };
    let files = (0..count)
        .map(|i| CatalogEntry {
            path: RepoPath::parse(&format!("part-{i:06}.bin")).unwrap(),
            size: Bytes::new(1),
            content_hash: None,
        })
        .collect();
    let record = storage::new_record(source, files, DownloadOptions::new(1, 1).unwrap()).unwrap();
    let store = TaskStore::create(&temp.path().join("download"), &record)
        .await
        .unwrap();
    let mut task = RunningTask {
        store,
        cached_record: Arc::new(record.clone()),
        record,
        engine: None,
        jobs: BTreeMap::new(),
        slots: BTreeMap::new(),
        pending: VecDeque::new(),
        active: Vec::new(),
        restart_notice: None,
        cached_progress: empty_progress(),
        verification: None,
    };
    task.refresh();
    let selection = Selection::new();
    let initial = AppSnapshot {
        revision: 0,
        phase: UiPhase::Idle,
        selection: selection.view(),
        task: None,
        settings: Arc::new(settings.clone()),
        recent: Arc::from([]),
        error: None,
        diagnostics: Diagnostics::default(),
        queue: None,
    };
    let (sender, receiver) = watch::channel(Arc::new(initial));
    let (load_sender, load_events) = mpsc::channel(2);
    let worker = Worker {
        budget: None,
        config: CoreConfig {
            paths,
            aria2_executable: "aria2c".into(),
        },
        settings,
        recent: Arc::from([]),
        selection,
        phase: UiPhase::Verifying,
        task: Some(task),
        access: NetworkAccess::default(),
        access_origin: None,
        snapshots: sender,
        revision: 0,
        error: None,
        load_cancel: CancellationToken::new(),
        load: None,
        load_events,
        load_sender,
        shutdown: CancellationToken::new(),
        run_cancel: CancellationToken::new(),
        status_unavailable_since: None,
        verification: None,
        verification_bytes: watch::channel(0).1,
        completion: Arc::new(|_| panic!("progress must not notify")),
        notice_epoch: 0,
        logger: None,
        logging_status: None,
        published_phase: UiPhase::Idle,
        verification_hook: None,
    };
    (worker, receiver, temp)
}

#[tokio::test]
async fn large_manifest_progress_snapshots_share_the_record() {
    let (mut worker, receiver, _temp) = worker_fixture(100_000).await;
    worker.task.as_mut().unwrap().verification = Some(VerificationProgress {
        file_index: 0,
        file_bytes_done: 0,
        files_done: 0,
        files_total: 100_000,
    });
    let original = worker.task.as_ref().unwrap().cached_record.clone();
    let started = Instant::now();
    for done in 0..100 {
        worker
            .task
            .as_mut()
            .unwrap()
            .verification
            .as_mut()
            .unwrap()
            .file_bytes_done = done;
        worker.publish().await;
        let snapshot = receiver.borrow();
        let task = snapshot.task.as_ref().unwrap();
        assert!(Arc::ptr_eq(&original, &task.record));
        assert_eq!(task.progress.total_files, 100_000);
        assert_eq!(task.verification.unwrap().file_bytes_done, done);
    }
    println!(
        "100000 files, 100 verification snapshots: {} us",
        started.elapsed().as_micros()
    );
}

#[tokio::test]
async fn pause_and_shutdown_join_chunk_blocked_digest_before_persisting_and_resume_restarts() {
    for shutdown in [false, true] {
        let (mut worker, _snapshots, temp) = worker_fixture(1).await;
        let task = worker.task.as_mut().unwrap();
        let path = task.store.destination().join("part-000000.bin");
        std::fs::write(&path, b"a").unwrap();
        task.record.files[0].completed = true;
        let record_path = task.store.task_file();
        let mut record = task.record.clone();
        record.files[0].entry.content_hash = Some(ContentHash {
            algorithm: HashAlgorithm::Sha256,
            hex: "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb".into(),
        });
        record.state = TaskState::Running;
        worker.task = None;
        std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let mut opened = TaskStore::open(&record_path).await.unwrap();
        opened.record.state = TaskState::Running;
        worker.task = Some(RunningTask {
            store: opened.store,
            record: opened.record,
            engine: None,
            jobs: BTreeMap::new(),
            slots: BTreeMap::new(),
            pending: VecDeque::new(),
            active: Vec::new(),
            restart_notice: None,
            cached_record: Arc::new(record),
            cached_progress: empty_progress(),
            verification: None,
        });
        let (chunk_tx, chunk_rx) = oneshot::channel();
        let chunk_tx = Mutex::new(Some(chunk_tx));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        worker.verification_hook = Some(Arc::new(move |done| {
            if done > 0 {
                if let Some(tx) = chunk_tx.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                release_rx.lock().unwrap().recv().unwrap();
            }
        }));
        worker.next_verification().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), chunk_rx)
            .await
            .unwrap()
            .unwrap();
        let cancelled = worker.run_cancel.clone();
        for command in [
            CoreCommand::Resume,
            CoreCommand::RetryFailed,
            CoreCommand::OpenTask(record_path.clone()),
            CoreCommand::SetAccess {
                endpoint: Endpoint::parse("http://127.0.0.1:12345", true).unwrap(),
                access: NetworkAccess::default(),
            },
        ] {
            assert!(matches!(
                worker.command(command).await,
                Err(CoreError::InvalidState { .. })
            ));
        }
        let pause = tokio::spawn(async move {
            if shutdown {
                worker.shutdown.cancel();
                worker.join_verification().await.unwrap();
                let task = worker.task.as_mut().unwrap();
                task.record.state = TaskState::Paused;
                task.persist().await.unwrap();
                worker.phase = UiPhase::Paused;
            } else {
                worker.pause().await.unwrap();
            }
            worker
        });
        tokio::time::timeout(Duration::from_secs(5), cancelled.cancelled())
            .await
            .unwrap();
        let disk: TaskRecord =
            serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
        assert_eq!(disk.state, TaskState::Running);
        assert!(!pause.is_finished());
        release_tx.send(()).unwrap();
        let mut worker = tokio::time::timeout(Duration::from_secs(5), pause)
            .await
            .unwrap()
            .unwrap();
        assert!(worker.verification.is_none());
        assert_eq!(
            worker.task.as_ref().unwrap().record.files[0].verification,
            VerificationStatus::Pending
        );
        let disk: TaskRecord =
            serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
        assert_eq!(disk.state, TaskState::Paused);
        let (notice_tx, mut notice_rx) = mpsc::unbounded_channel();
        worker.completion = Arc::new(move |notice| {
            notice_tx.send(notice).unwrap();
        });
        worker.verification_hook = None;
        worker.shutdown = CancellationToken::new();
        worker.run_cancel = worker.shutdown.child_token();
        worker.resume(false).await.unwrap();
        let result = (&mut worker.verification.as_mut().unwrap().result).await;
        worker.verified(result).await.unwrap();
        assert_eq!(
            notice_rx.try_recv().unwrap().outcome,
            CompletionOutcome::Verified
        );
        assert!(notice_rx.try_recv().is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"a");
        drop(worker);
        drop(temp);
    }
}

fn log_records(root: &std::path::Path) -> Vec<serde_json::Value> {
    let mut records = Vec::new();
    for directory in std::fs::read_dir(root).unwrap() {
        let directory = directory.unwrap().path();
        if directory.is_dir() {
            for file in std::fs::read_dir(directory).unwrap() {
                let file = file.unwrap().path();
                if file.extension().is_some_and(|extension| extension == "log") {
                    records.extend(
                        std::fs::read_to_string(file)
                            .unwrap()
                            .lines()
                            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()),
                    );
                }
            }
        }
    }
    records
}
#[tokio::test]
async fn settings_debug_changes_only_after_durable_save_and_log_failure_is_diagnostic() {
    use crate::logging::{LogConfig, LogError};
    let (mut worker, snapshots, temp) = worker_fixture(1).await;
    let root = temp.path().join("logs");
    let logger = Arc::new(SessionLogger::new(LogConfig::new(root.clone())).unwrap());
    worker.logger = Some(logger.clone());
    let mut settings = worker.settings.clone();
    settings.debug_logging = true;
    worker
        .command(CoreCommand::SaveSettings(settings))
        .await
        .unwrap();
    worker.event(
        LogLevel::Debug,
        (EventKind::EngineLaunch, LogOutcome::Started),
    );
    let original_path = worker.config.paths.settings_file.clone();
    let blocked = temp.path().join("blocked");
    std::fs::write(&blocked, b"block settings parent").unwrap();
    worker.config.paths.settings_file = blocked.join("settings.json");
    let mut settings = worker.settings.clone();
    settings.debug_logging = false;
    assert!(
        worker
            .command(CoreCommand::SaveSettings(settings))
            .await
            .is_err()
    );
    worker.event(
        LogLevel::Debug,
        (EventKind::EngineConfigure, LogOutcome::Succeeded),
    );
    worker.config.paths.settings_file = original_path;
    assert!(
        storage::load_settings(&worker.config.paths)
            .await
            .unwrap()
            .debug_logging
    );
    logger.flush().unwrap();
    let events = log_records(&root);
    assert_eq!(
        events
            .iter()
            .filter(|event| event["level"] == "DEBUG")
            .count(),
        2
    );
    assert!(worker.settings.debug_logging);
    let startup_size = events[0].to_string().len() + 1;
    let failed_root = temp.path().join("failed-logs");
    let failing = Arc::new(
        SessionLogger::new(LogConfig::new(failed_root.clone()).with_limits(
            std::num::NonZeroU64::new(u64::try_from(startup_size).unwrap()).unwrap(),
            std::num::NonZeroU64::new(100_000).unwrap(),
        ))
        .unwrap(),
    );
    worker.logger = Some(failing);
    let state = worker.task.as_ref().unwrap().record.state;
    worker.event(
        LogLevel::Info,
        (EventKind::TaskResumed, LogOutcome::Started),
    );
    worker.publish().await;
    assert_eq!(
        snapshots.borrow().diagnostics.logging,
        Some(LogStatus::Failed(LogError::RecordTooLarge))
    );
    assert_eq!(
        snapshots.borrow().diagnostics.log_directory,
        Some(failed_root)
    );
    assert!(snapshots.borrow().error.is_none());
    assert_eq!(worker.task.as_ref().unwrap().record.state, state);
}

#[tokio::test]
async fn failed_shutdown_receipt_records_actual_child_exit() {
    use crate::logging::LogConfig;
    let (mut worker, _snapshots, temp) = worker_fixture(1).await;
    let root = temp.path().join("shutdown-logs");
    let logger =
        Arc::new(SessionLogger::new(LogConfig::new(root.clone()).with_debug(true)).unwrap());
    worker.logger = Some(logger.clone());
    worker.log_shutdown_failure(
        &ProbeError::Shutdown(ShutdownReceipt {
            stop: StopKind::Graceful,
            child_pid: Some(1234),
            child_exit_code: Some(23),
        }),
        Instant::now(),
    );
    logger.flush().unwrap();
    let events = log_records(&root);
    let event = events
        .iter()
        .find(|value| value["event"]["event"] == "process_exit")
        .unwrap();
    assert_eq!(event["event"]["outcome"], "engine");
    assert_eq!(event["metrics"]["child_pid"], 1234);
    assert_eq!(event["metrics"]["child_exit_code"], 23);
}

#[tokio::test]
async fn queued_start_tokens_cannot_undo_pause_cancellation() {
    let shutdown = CancellationToken::new();
    let (commands, mut receiver) = mpsc::channel(32);
    let handle = CoreHandle {
        commands,
        shutdown: shutdown.clone(),
        interrupt: Arc::new(Mutex::new(shutdown.child_token())),
    };
    let _first = handle.submit(CoreCommand::Resume).unwrap();
    let first = receiver.recv().await.unwrap().run_cancel.unwrap();
    let _queued = handle.submit(CoreCommand::RetryFailed).unwrap();
    let _pause = handle.submit(CoreCommand::Pause).unwrap();
    let queued = receiver.recv().await.unwrap().run_cancel.unwrap();
    assert!(first.is_cancelled());
    assert!(queued.is_cancelled());
    receiver.recv().await.unwrap();
    let _next = handle.submit(CoreCommand::Resume).unwrap();
    assert!(
        !receiver
            .recv()
            .await
            .unwrap()
            .run_cancel
            .unwrap()
            .is_cancelled()
    );
}
