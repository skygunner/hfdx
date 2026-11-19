use super::*;
use crate::repository::*;
fn record(path: &str) -> TaskRecord {
    let source = RepositorySpec {
        request: RepositoryRequest {
            endpoint: Endpoint::parse("https://huggingface.co", false).unwrap(),
            kind: RepoKind::Model,
            repo_id: RepoId::parse("org/model").unwrap(),
            requested_revision: Revision::parse("main").unwrap(),
        },
        resolved_commit: CommitId::parse(&"a".repeat(40)).unwrap(),
    };
    new_record(
        source,
        vec![CatalogEntry {
            path: RepoPath::parse(path).unwrap(),
            size: Bytes::new(4),
            content_hash: None,
        }],
        DownloadOptions::new(4, 5).unwrap(),
    )
    .unwrap()
}
#[tokio::test]
async fn failure_messages_never_persist_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = record("data");
    let mut store = TaskStore::create(dir.path(), &r).await.unwrap();
    for kind in [
        FailureKind::Network,
        FailureKind::Authentication,
        FailureKind::Disk,
        FailureKind::Engine,
        FailureKind::Integrity,
        FailureKind::Interrupted,
    ] {
        r.files[0].failure = Some(FileFailure {
            kind,
            message: "fake-token fake-password https://host/?signature=fake".into(),
            aria2_code: None,
            http_status: None,
        });
        store.persist(&r).await.unwrap();
        let bytes = std::fs::read(store.task_file()).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            !text.contains("fake-token")
                && !text.contains("fake-password")
                && !text.contains("signature=")
        );
        let stored: TaskRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(stored.files[0].failure.as_ref().unwrap().kind, kind);
        assert!(!stored.files[0].failure.as_ref().unwrap().message.is_empty());
    }
}
#[tokio::test]
async fn unknown_control_and_directory_collisions_are_rejected() {
    for name in ["data.aria2", "data"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(name)).unwrap();
        assert!(matches!(
            TaskStore::create(dir.path(), &record("data")).await,
            Err(StorageError::Conflict(_))
        ));
        assert!(dir.path().join(name).is_dir());
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("data.aria2"), b"unknown").unwrap();
    assert!(matches!(
        TaskStore::create(dir.path(), &record("data")).await,
        Err(StorageError::Conflict(_))
    ));
    assert_eq!(
        std::fs::read(dir.path().join("data.aria2")).unwrap(),
        b"unknown"
    );
}
#[tokio::test]
async fn malicious_record_paths_are_rejected_without_modifying_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::create(dir.path(), &record("data"))
        .await
        .unwrap();
    let path = store.task_file();
    drop(store);
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for unsafe_path in [
        "../escape",
        "/absolute",
        "C:/drive",
        "a\\\\b",
        ".hfdx/task.json",
        "x/../y",
        "a\u{0000}b",
    ] {
        json["files"][0]["entry"]["path"] = unsafe_path.into();
        let bytes = serde_json::to_vec(&json).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(TaskStore::open(&path).await.is_err(), "{unsafe_path:?}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
#[tokio::test]
async fn manifest_collisions_are_rejected() {
    for paths in [["a", "a/b"], ["a", "a.aria2"]] {
        let mut r = record(paths[0]);
        r.files.push(TaskFile {
            disposition: FileDisposition::Download,
            entry: CatalogEntry {
                path: RepoPath::parse(paths[1]).unwrap(),
                size: Bytes::new(4),
                content_hash: None,
            },
            completed: false,
            failure: None,
            verification: VerificationStatus::Pending,
        });
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            TaskStore::create(dir.path(), &r).await,
            Err(StorageError::Conflict(_))
        ));
    }
}
#[tokio::test]
async fn modified_record_cannot_authorize_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let original = record("data");
    let mut store = TaskStore::create(dir.path(), &original).await.unwrap();
    std::fs::write(dir.path().join("data"), b"sentinel").unwrap();
    let other = record("data");
    std::fs::write(store.task_file(), serde_json::to_vec(&other).unwrap()).unwrap();
    assert!(matches!(
        store
            .prepare(&original.files[0].entry, ResumeAction::RestartOwned)
            .await,
        Err(StorageError::Conflict(_))
    ));
    assert_eq!(std::fs::read(dir.path().join("data")).unwrap(), b"sentinel");
}
#[tokio::test]
async fn unknown_files_are_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("data"), b"sentinel").unwrap();
    assert!(matches!(
        TaskStore::create(dir.path(), &record("data")).await,
        Err(StorageError::Conflict(_))
    ));
    assert_eq!(std::fs::read(dir.path().join("data")).unwrap(), b"sentinel");
}
#[cfg(unix)]
#[tokio::test]
async fn symlink_cannot_reach_outside_destination() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("data"), b"sentinel").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    assert!(matches!(
        TaskStore::create(dir.path(), &record("link/data")).await,
        Err(StorageError::UnsafePath(_))
    ));
    assert_eq!(
        std::fs::read(outside.path().join("data")).unwrap(),
        b"sentinel"
    );
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn rename_failure_preserves_record_and_removes_temporary_file() {
    if let Some(root) = std::env::var_os("HFDX_RENAME_FAILURE_CHILD") {
        let task_file = std::path::PathBuf::from(root).join(".hfdx/task.json");
        let mut opened = TaskStore::open(&task_file).await.unwrap();
        opened.record.state = TaskState::Failed;
        match opened.store.persist(&opened.record).await {
            Err(StorageError::Io(error)) => assert_eq!(error.raw_os_error(), Some(1)),
            _ => panic!("expected EPERM from rename"),
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::create(dir.path(), &record("data"))
        .await
        .unwrap();
    let task_file = store.task_file();
    let before = std::fs::read(&task_file).unwrap();
    drop(store);
    let filter = dir.path().join("filter.bpf");
    let instructions: [(u16, u8, u8, u32); 6] = [
        (0x20, 0, 0, 0),
        (0x15, 3, 0, 82),
        (0x15, 2, 0, 264),
        (0x15, 1, 0, 316),
        (0x06, 0, 0, 0x7fff0000),
        (0x06, 0, 0, 0x00050001),
    ];
    let mut bytes = Vec::new();
    for (code, jt, jf, k) in instructions {
        bytes.extend_from_slice(&code.to_ne_bytes());
        bytes.push(jt);
        bytes.push(jf);
        bytes.extend_from_slice(&k.to_ne_bytes());
    }
    std::fs::write(&filter, bytes).unwrap();
    let status = std::process::Command::new("setpriv")
        .arg("--no-new-privs")
        .arg("--seccomp-filter")
        .arg(&filter)
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "storage::tests::rename_failure_preserves_record_and_removes_temporary_file",
            "--nocapture",
        ])
        .env("HFDX_RENAME_FAILURE_CHILD", dir.path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "fault-injection subprocess failed: {status}"
    );
    assert_eq!(std::fs::read(task_file).unwrap(), before);
    let mut names: Vec<_> = std::fs::read_dir(dir.path().join(".hfdx"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            std::ffi::OsString::from("lock"),
            std::ffi::OsString::from("task.json")
        ]
    );
}
#[tokio::test]
async fn failed_persist_preserves_existing_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut original = record("data");
    let mut store = TaskStore::create(dir.path(), &original).await.unwrap();
    let before = std::fs::read(store.task_file()).unwrap();
    original.schema_version = 3;
    assert!(matches!(
        store.persist(&original).await,
        Err(StorageError::UnsupportedSchema(3))
    ));
    assert_eq!(std::fs::read(store.task_file()).unwrap(), before);
}
#[tokio::test]
async fn incompatible_schema_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::create(dir.path(), &record("data"))
        .await
        .unwrap();
    let path = store.task_file();
    drop(store);
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["schema_version"] = 3.into();
    let bytes = serde_json::to_vec(&json).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let result = TaskStore::open(&path).await;
    assert!(
        matches!(result, Err(StorageError::UnsupportedSchema(3))),
        "unexpected open result: {}",
        match &result {
            Ok(_) => "success".to_owned(),
            Err(error) => format!("{error:?}"),
        }
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[cfg(unix)]
#[tokio::test]
async fn inherited_descriptor_does_not_extend_store_lock() {
    use std::process::Stdio;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::{Duration, timeout};

    let dir = tempfile::tempdir().unwrap();
    let store = TaskStore::create(dir.path(), &record("data"))
        .await
        .unwrap();
    let path = store.task_file();
    let worker_lock = Arc::clone(&store.lock);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let worker = tokio::task::spawn_blocking(move || {
        let _lock = worker_lock;
        started_tx.send(()).unwrap();
        finish_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let inherited = store.lock.0.try_clone().unwrap();
    let mut child = tokio::process::Command::new("python3")
        .args(["-c", "import sys; sys.stdout.buffer.write(b'R'); sys.stdout.buffer.flush(); sys.stdin.buffer.read(1)"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(inherited))
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut ready = [0];
    timeout(
        Duration::from_secs(10),
        child.stdout.as_mut().unwrap().read_exact(&mut ready),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ready, *b"R");
    drop(store);
    assert!(matches!(
        TaskStore::open(&path).await,
        Err(StorageError::Locked)
    ));
    finish_tx.send(()).unwrap();
    timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap();
    let result = TaskStore::open(&path).await;
    assert!(
        result.is_ok(),
        "unexpected open error: {}",
        match &result {
            Ok(_) => "success".to_owned(),
            Err(error) => format!("{error:?}"),
        }
    );
    child.stdin.as_mut().unwrap().write_all(b"x").await.unwrap();
    assert!(
        timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
#[tokio::test]
async fn corrupted_record_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let metadata_dir = dir.path().join(".hfdx");
    std::fs::create_dir(&metadata_dir).unwrap();
    let path = metadata_dir.join("task.json");
    std::fs::write(&path, b"broken").unwrap();
    let result = TaskStore::open(&path).await;
    assert!(
        matches!(result, Err(StorageError::CorruptRecord)),
        "unexpected open result: {}",
        match &result {
            Ok(_) => "success".to_owned(),
            Err(error) => format!("{error:?}"),
        }
    );
    assert_eq!(std::fs::read(path).unwrap(), b"broken");
}

#[cfg(unix)]
#[tokio::test]
async fn replacement_symlink_after_inspection_does_not_touch_external_file() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let entry_record = record("nested/data");
    let mut store = TaskStore::create(dir.path(), &entry_record).await.unwrap();
    assert_eq!(
        store.inspect(&entry_record).await.unwrap()[0].action,
        ResumeAction::Create
    );
    std::fs::write(outside.path().join("data"), b"outside-sentinel").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("nested")).unwrap();
    assert!(matches!(
        store
            .prepare(&entry_record.files[0].entry, ResumeAction::Create)
            .await,
        Err(StorageError::UnsafePath(_))
    ));
    assert_eq!(
        std::fs::read(outside.path().join("data")).unwrap(),
        b"outside-sentinel"
    );
}
#[tokio::test]
async fn control_file_has_priority_when_payload_is_missing_or_incomplete() {
    for completed in [false, true] {
        for content in [None, Some(b"x".as_slice()), Some(b"1234".as_slice())] {
            let dir = tempfile::tempdir().unwrap();
            let mut r = record("data");
            let store = TaskStore::create(dir.path(), &r).await.unwrap();
            r.files[0].completed = completed;
            if let Some(bytes) = content {
                std::fs::write(dir.path().join("data"), bytes).unwrap();
            }
            std::fs::write(dir.path().join("data.aria2"), b"control").unwrap();
            assert_eq!(
                store.inspect(&r).await.unwrap()[0].action,
                ResumeAction::ResumeControl
            );
        }
    }
}
#[tokio::test]
async fn resume_table_and_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let template = record("done");
    let files = ["done", "control", "missing", "incomplete", "wrong", "empty"]
        .into_iter()
        .map(|path| CatalogEntry {
            path: RepoPath::parse(path).unwrap(),
            size: Bytes::new(if path == "empty" { 0 } else { 4 }),
            content_hash: None,
        })
        .collect();
    let mut record = new_record(template.source, files, template.options).unwrap();
    let mut store = TaskStore::create(dir.path(), &record).await.unwrap();
    for i in [0, 1, 4] {
        record.files[i].completed = true;
    }
    std::fs::write(dir.path().join("done"), b"1234").unwrap();
    std::fs::write(dir.path().join("control"), b"1234").unwrap();
    std::fs::write(dir.path().join("control.aria2"), b"control").unwrap();
    std::fs::write(dir.path().join("incomplete"), b"1234").unwrap();
    std::fs::write(dir.path().join("wrong"), b"x").unwrap();
    let actions: Vec<_> = store
        .inspect(&record)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.action)
        .collect();
    assert_eq!(
        actions,
        [
            ResumeAction::SkipSizeChecked,
            ResumeAction::ResumeControl,
            ResumeAction::Create,
            ResumeAction::RestartOwned,
            ResumeAction::RestartOwned,
            ResumeAction::Create
        ]
    );
    assert_eq!(actions[3].reason(), Some(restart_owned_reason()));
    assert_eq!(actions[0].reason(), None);
    store.persist(&record).await.unwrap();
    assert!(matches!(
        store
            .prepare(&record.files[0].entry, ResumeAction::RestartOwned)
            .await,
        Err(StorageError::Conflict(_))
    ));
    assert_eq!(std::fs::read(dir.path().join("done")).unwrap(), b"1234");
    store
        .prepare(&record.files[3].entry, ResumeAction::RestartOwned)
        .await
        .unwrap();
    assert!(!dir.path().join("incomplete").exists());
    let empty = store
        .prepare(&record.files[5].entry, ResumeAction::Create)
        .await
        .unwrap();
    store.create_empty(empty).await.unwrap();
    store.verify_size(&record.files[5].entry).await.unwrap();
    store.persist(&record).await.unwrap();
    assert!(matches!(
        TaskStore::open(&store.task_file()).await,
        Err(StorageError::Locked)
    ));
    let path = store.task_file();
    drop(store);
    let opened = TaskStore::open(&path).await.unwrap();
    assert_eq!(opened.record, record);
}

#[tokio::test]
async fn legacy_records_default_to_unverified_and_require_explicit_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let mut initial = record("data");
    initial.files[0].completed = true;
    initial.state = TaskState::Completed;
    let store = TaskStore::create(dir.path(), &initial).await.unwrap();
    let path = store.task_file();
    drop(store);
    let mut json = serde_json::to_value(&initial).unwrap();
    json.as_object_mut()
        .unwrap()
        .remove("verify_after_download");
    json["files"][0]
        .as_object_mut()
        .unwrap()
        .remove("verification");
    let legacy = serde_json::to_vec(&json).unwrap();
    std::fs::write(&path, &legacy).unwrap();

    let mut opened = TaskStore::open(&path).await.unwrap();
    assert_eq!(opened.record.verify_after_download, None);
    assert_eq!(
        opened.record.files[0].verification,
        VerificationStatus::LegacyUnverified
    );
    assert_eq!(std::fs::read(&path).unwrap(), legacy);
    opened.record.verify_after_download = Some(true);
    assert!(matches!(
        opened.store.persist(&opened.record).await,
        Err(StorageError::CorruptRecord)
    ));
    opened.record.verify_after_download = None;
    opened
        .store
        .enable_verification_for_legacy(&mut opened.record)
        .await
        .unwrap();
    assert_eq!(
        read_record(&path).unwrap().verify_after_download,
        Some(true)
    );
    opened.record.verify_after_download = Some(false);
    assert!(matches!(
        opened.store.persist(&opened.record).await,
        Err(StorageError::CorruptRecord)
    ));
}

#[tokio::test]
async fn verification_path_rejects_control_wrong_size_and_unknown_entry() {
    let dir = tempfile::tempdir().unwrap();
    let r = record("data");
    let store = TaskStore::create(dir.path(), &r).await.unwrap();
    let path = dir.path().join("data");
    std::fs::write(&path, b"1234").unwrap();
    assert_eq!(
        store.verification_path(&r.files[0].entry).await.unwrap(),
        path
    );
    let mut other = r.files[0].entry.clone();
    other.size = Bytes::new(3);
    assert!(store.verification_path(&other).await.is_err());
    std::fs::write(dir.path().join("data.aria2"), b"control").unwrap();
    assert!(store.verification_path(&r.files[0].entry).await.is_err());
    std::fs::remove_file(dir.path().join("data.aria2")).unwrap();
    std::fs::write(&path, b"bad").unwrap();
    assert!(matches!(
        store.verification_path(&r.files[0].entry).await,
        Err(StorageError::SizeMismatch)
    ));
}

#[tokio::test]
async fn integrity_retry_removes_only_owned_failed_payload() {
    for size in [0, 4] {
        let dir = tempfile::tempdir().unwrap();
        let mut r = record("failed");
        r.files[0].entry.size = Bytes::new(size);
        let mut passed = r.files[0].clone();
        passed.entry.path = RepoPath::parse("passed").unwrap();
        passed.completed = true;
        passed.verification = VerificationStatus::Passed;
        r.files.push(passed);
        let mut store = TaskStore::create(dir.path(), &r).await.unwrap();
        let contents = vec![b'x'; usize::try_from(size).unwrap()];
        std::fs::write(dir.path().join("failed"), &contents).unwrap();
        std::fs::write(dir.path().join("passed"), &contents).unwrap();
        r.files[0].completed = true;
        r.files[0].verification = VerificationStatus::Failed;
        r.files[0].failure = Some(FileFailure {
            kind: FailureKind::Integrity,
            message: String::new(),
            aria2_code: None,
            http_status: None,
        });
        r.state = TaskState::Failed;
        store.persist(&r).await.unwrap();
        let actions = store.inspect(&r).await.unwrap();
        assert_eq!(actions[0].action, ResumeAction::RestartIntegrityFailed);
        assert_eq!(actions[1].action, ResumeAction::SkipSizeChecked);
        assert!(
            store
                .prepare(&r.files[1].entry, ResumeAction::RestartIntegrityFailed)
                .await
                .is_err()
        );
        std::fs::write(dir.path().join("failed.aria2"), b"control").unwrap();
        assert!(
            store
                .prepare(&r.files[0].entry, actions[0].action)
                .await
                .is_err()
        );
        std::fs::remove_file(dir.path().join("failed.aria2")).unwrap();
        let prepared = store
            .prepare(&r.files[0].entry, actions[0].action)
            .await
            .unwrap();
        assert!(!dir.path().join("failed").exists());
        assert_eq!(std::fs::read(dir.path().join("passed")).unwrap(), contents);
        // A crash after deletion but before saving the retry must remain recoverable.
        assert_eq!(
            store.inspect(&r).await.unwrap()[0].action,
            ResumeAction::RestartIntegrityFailed
        );
        store
            .prepare(&r.files[0].entry, ResumeAction::RestartIntegrityFailed)
            .await
            .unwrap();
        if size == 0 {
            store.create_empty(prepared).await.unwrap();
            store.verify_size(&r.files[0].entry).await.unwrap();
        }
    }
}

#[tokio::test]
async fn replacement_publication_recovers_each_rename_boundary() {
    for boundary in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path();
        let mut saved = record("data.bin");
        let mut store = TaskStore::create(dest, &saved).await.unwrap();
        let logical = dest.join("data.bin");
        std::fs::write(&logical, b"old!").unwrap();
        let original = FileStamp::read(&logical).unwrap().unwrap();
        let staged = dest
            .join(".hfdx/incoming")
            .join(saved.task_id.as_str())
            .join("data.bin");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"new!").unwrap();
        let archive_dir = format!("unit-{}", saved.task_id.as_str());
        let archived = dest.join("archive").join(&archive_dir).join("data.bin");
        saved.schema_version = 2;
        saved.files[0].completed = true;
        saved.files[0].verification = VerificationStatus::Passed;
        saved.replacements.insert(
            "data.bin".into(),
            Replacement {
                archive: true,
                original,
                ready: FileStamp::read(&staged).unwrap(),
                archive_dir: Some(archive_dir),
            },
        );
        store.persist(&saved).await.unwrap();
        if boundary >= 1 {
            std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
            std::fs::rename(&logical, &archived).unwrap();
        }
        if boundary == 2 {
            std::fs::rename(&staged, &logical).unwrap();
        }
        let file = store.task_file();
        drop(store);
        let mut reopened = TaskStore::open(&file).await.unwrap();
        reopened
            .store
            .resume_publications(&mut reopened.record)
            .await
            .unwrap();
        assert_eq!(std::fs::read(logical).unwrap(), b"new!");
        assert_eq!(std::fs::read(archived).unwrap(), b"old!");
        assert!(!staged.exists());
        assert!(reopened.record.replacements.is_empty());
    }
}
#[tokio::test]
async fn version_one_refuses_queue_only_semantics_instead_of_silently_downgrading() {
    let temp = tempfile::tempdir().unwrap();
    let mut old = record("data.bin");
    old.files[0].disposition = FileDisposition::Unchecked;
    assert!(matches!(
        TaskStore::create(temp.path(), &old).await,
        Err(StorageError::UnsupportedSchema(2))
    ));
    assert!(!temp.path().join(".hfdx/task.json").exists());
}
