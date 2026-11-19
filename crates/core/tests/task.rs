use hfdx_core::{
    CatalogState, DownloadOptions, Endpoint, NetworkAccess, RepoId, RepoKind, RepositoryRequest,
    Revision, SelectionChange, SettingsPaths, TaskState,
    task::{self, CoreCommand, CoreConfig, LoadRequest, StartRequest, UiPhase},
};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}

#[tokio::test]
async fn downloads_selected_files_and_reopens_paused_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new("python")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/qa/fixture.py"
        ))
        .args(["--port", "0", "--rate", "0", "--ready-file"])
        .arg(dir.path().join("ready.json"))
        .arg("--log")
        .arg(dir.path().join("events.jsonl"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let fixture = Fixture(child);
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let request = RepositoryRequest {
        endpoint: Endpoint::parse(ready["origin"].as_str().unwrap(), true).unwrap(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("org/model").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    let config = || CoreConfig {
        paths: SettingsPaths {
            settings_file: dir.path().join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    };
    let mut session = task::spawn(config()).await.unwrap();
    let mut settings = (*session.snapshots.borrow().settings).clone();
    settings.recent_directories = (0..10)
        .map(|index| dir.path().join(format!("old-{index}")))
        .collect();
    session
        .handle
        .submit(CoreCommand::SaveSettings(settings))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
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
        Duration::from_secs(15),
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
        .submit(CoreCommand::SetFilter(
            "small-complete.bin\nempty.bin\nspecial/space # 中文.bin".into(),
        ))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session
        .handle
        .submit(CoreCommand::ChangeSelection(SelectionChange::SelectMatches))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let destination = dir.path().join("download");
    session
        .handle
        .submit(CoreCommand::Start(StartRequest {
            catalog,
            destination: destination.clone(),
            options: DownloadOptions::new(1, 2).unwrap(),
            verify_after_download: true,
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(20),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
                || s.error.is_some()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(session.snapshots.borrow().error.is_none());
    assert_eq!(
        std::fs::read(destination.join("small-complete.bin")).unwrap(),
        (0..=255u8).cycle().take(256 * 1024).collect::<Vec<_>>()
    );
    assert_eq!(
        std::fs::metadata(destination.join("empty.bin"))
            .unwrap()
            .len(),
        0
    );
    session.handle.request_shutdown().unwrap();
    let receipt = session.join().await.unwrap();
    println!("aria2 shutdown receipt: {:?}", receipt);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let state_url = format!("{}/__state", ready["origin"].as_str().unwrap());
    let before: serde_json::Value = client
        .get(&state_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let file_gets_before = before["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| {
            event["method"] == "GET"
                && event["path"]
                    .as_str()
                    .is_some_and(|path| path.contains("/resolve/"))
        })
        .count();
    let reopened = task::spawn(config()).await.unwrap();
    {
        let snapshot = reopened.snapshots.borrow();
        let settings = &snapshot.settings;
        assert_eq!(settings.recent_directories.len(), 10);
        assert_eq!(settings.recent_directories[0], dir.path());
        assert_eq!(settings.recent_directories[9], dir.path().join("old-8"));
        assert_eq!(settings.options, DownloadOptions::new(1, 2).unwrap());
        assert_eq!(
            settings.endpoint.url().as_str(),
            ready["origin"].as_str().unwrap().to_owned() + "/"
        );
        assert_eq!(
            hfdx_core::storage::default_destination(settings, &RepoId::parse("org/model").unwrap())
                .unwrap(),
            dir.path().join("model")
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        use tokio::io::AsyncReadExt;
        let mut buffer = [0u8; 1024];
        assert!(stream.read(&mut buffer).await.unwrap() > 0);
        accepted_tx.send(()).unwrap();
        release_rx.await.unwrap();
    });
    let source = RepositoryRequest {
        endpoint: Endpoint::parse(&format!("http://{address}"), true).unwrap(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("org/model").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    let saved_id = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read(destination.join(".hfdx/task.json")).unwrap(),
    )
    .unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    reopened
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: source,
            access: NetworkAccess::default(),
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), accepted_rx)
        .await
        .unwrap()
        .unwrap();
    reopened
        .handle
        .submit(CoreCommand::OpenTask(destination.join(".hfdx/task.json")))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    release_tx.send(()).unwrap();
    server.await.unwrap();
    reopened
        .handle
        .submit(CoreCommand::SetSelectedOnly(false))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.snapshots.borrow().phase, UiPhase::Paused);
    let snapshot = reopened.snapshots.borrow();
    let task = snapshot.task.as_ref().unwrap();
    assert_eq!(task.record.state, TaskState::Completed);
    assert_eq!(task.record.task_id.as_str(), saved_id);
    drop(snapshot);
    let stale_access = NetworkAccess {
        token: Some(hfdx_core::Token::parse("old-source-token").unwrap()),
        proxy: None,
    };
    let rejected = reopened
        .handle
        .submit(CoreCommand::SetAccess {
            endpoint: Endpoint::parse(&format!("http://{address}"), true).unwrap(),
            access: stale_access,
        })
        .unwrap()
        .await
        .unwrap();
    assert!(matches!(
        rejected,
        Err(task::CoreError::InvalidState { .. })
    ));
    reopened
        .handle
        .submit(CoreCommand::SetAccess {
            endpoint: Endpoint::parse(ready["origin"].as_str().unwrap(), true).unwrap(),
            access: NetworkAccess::default(),
        })
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let after: serde_json::Value = client
        .get(&state_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let file_gets_after = after["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| {
            event["method"] == "GET"
                && event["path"]
                    .as_str()
                    .is_some_and(|path| path.contains("/resolve/"))
        })
        .count();
    assert_eq!(file_gets_before, file_gets_after);
    assert!(
        !after["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["auth_present"] == true)
    );
    for truncate in [false, true] {
        let path = destination.join("small-complete.bin");
        if truncate {
            std::fs::write(&path, b"partial").unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        reopened
            .handle
            .submit(CoreCommand::Resume)
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        let mut updates = reopened.snapshots.clone();
        tokio::time::timeout(
            Duration::from_secs(20),
            updates.wait_for(|s| {
                s.task
                    .as_ref()
                    .is_some_and(|t| t.record.state == TaskState::Completed)
                    || s.error.is_some()
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(updates.borrow().error.is_none());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            (0..=255u8).cycle().take(256 * 1024).collect::<Vec<_>>()
        );
    }
    let final_state: serde_json::Value = client
        .get(&state_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let unaffected = |state: &serde_json::Value| {
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| {
                event["method"] == "GET"
                    && event["path"]
                        .as_str()
                        .is_some_and(|path| path.ends_with("special/space # 中文.bin"))
            })
            .count()
    };
    assert!(unaffected(&after) > 0);
    assert_eq!(unaffected(&after), unaffected(&final_state));
    reopened.handle.request_shutdown().unwrap();
    reopened.join().await.unwrap();
    drop(fixture);
}

#[tokio::test]
async fn shutdown_reaps_active_engine_and_explicit_resume_retains_partial_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new("python")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/qa/fixture.py"
        ))
        .args(["--port", "0", "--rate", "1048576", "--ready-file"])
        .arg(dir.path().join("ready.json"))
        .arg("--log")
        .arg(dir.path().join("events.jsonl"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let fixture = Fixture(child);
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let origin = ready["origin"].as_str().unwrap();
    let request = RepositoryRequest {
        endpoint: Endpoint::parse(origin, true).unwrap(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("org/model").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    client
        .post(format!("{origin}/__control"))
        .json(&serde_json::json!({"tree_mode":"conflict"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let config = || CoreConfig {
        paths: SettingsPaths {
            settings_file: dir.path().join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    };
    let mut session = task::spawn(config()).await.unwrap();
    session
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: request.clone(),
            access: NetworkAccess::default(),
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(15),
        session
            .snapshots
            .wait_for(|s| s.phase == UiPhase::Browsing && s.error.is_some()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        session.snapshots.borrow().selection.state,
        CatalogState::Failed
    );
    client
        .post(format!("{origin}/__control"))
        .json(&serde_json::json!({"tree_mode":"normal"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
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
        Duration::from_secs(15),
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
        .submit(CoreCommand::SetFilter(
            "large-partial.bin\nsmall-complete.bin".into(),
        ))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session
        .handle
        .submit(CoreCommand::ChangeSelection(SelectionChange::SelectMatches))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let destination = dir.path().join("download");
    session
        .handle
        .submit(CoreCommand::Start(StartRequest {
            catalog,
            destination: destination.clone(),
            options: DownloadOptions::new(1, 2).unwrap(),
            verify_after_download: true,
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(20),
        session.snapshots.wait_for(|s| {
            s.task.as_ref().is_some_and(|t| {
                t.progress.completed_files == 1 && t.progress.acquired_bytes.get() > 256 * 1024
            })
        }),
    )
    .await
    .unwrap()
    .unwrap();
    session.handle.request_shutdown().unwrap();
    let receipt = session.join().await.unwrap();
    let pid = receipt.child_pid.unwrap();
    println!("active shutdown receipt: {receipt:?}");
    #[cfg(target_os = "linux")]
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert!(destination.join("large-partial.bin.aria2").exists());
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(destination.join(".hfdx/task.json")).unwrap())
            .unwrap();
    assert_eq!(persisted["state"], "paused");
    client
        .post(format!("{origin}/__control"))
        .json(&serde_json::json!({"rate":0,"branch":"b"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let mut reopened = task::spawn(config()).await.unwrap();
    reopened
        .handle
        .submit(CoreCommand::OpenTask(destination.join(".hfdx/task.json")))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.snapshots.borrow().phase, UiPhase::Paused);
    reopened
        .handle
        .submit(CoreCommand::Resume)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(30),
        reopened.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
                || s.error.is_some()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(reopened.snapshots.borrow().error.is_none());
    assert_eq!(
        std::fs::read(destination.join("large-partial.bin")).unwrap(),
        (0..=255u8)
            .cycle()
            .take(32 * 1024 * 1024)
            .collect::<Vec<_>>()
    );
    let events: serde_json::Value = client
        .get(format!("{origin}/__state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(events["events"].as_array().unwrap().iter().any(|e| {
        e["method"] == "GET"
            && e["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("large-partial.bin"))
            && e["range"]
                .as_str()
                .is_some_and(|r| r.starts_with("bytes=") && !r.starts_with("bytes=0-"))
    }));
    reopened.handle.request_shutdown().unwrap();
    reopened.join().await.unwrap();
    let task_file = destination.join(".hfdx/task.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&task_file).unwrap()).unwrap();
    record["state"] = "paused".into();
    let large_index = record["files"]
        .as_array()
        .unwrap()
        .iter()
        .position(|file| file["entry"]["path"] == "large-partial.bin")
        .unwrap();
    record["files"][large_index]["completed"] = false.into();
    std::fs::write(&task_file, serde_json::to_vec(&record).unwrap()).unwrap();
    let unrelated = destination.join("unowned.txt");
    std::fs::write(&unrelated, b"unowned data").unwrap();
    let before: serde_json::Value = client
        .get(format!("{origin}/__state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let small_gets = |state: &serde_json::Value| {
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| {
                event["kind"] == "start"
                    && event["method"] == "GET"
                    && event["path"]
                        .as_str()
                        .is_some_and(|path| path.ends_with("small-complete.bin"))
            })
            .count()
    };
    let before_small = small_gets(&before);
    assert!(before_small > 0);
    let before_seq = before["events"].as_array().unwrap().last().unwrap()["seq"]
        .as_u64()
        .unwrap();
    assert!(!destination.join("large-partial.bin.aria2").exists());
    let mut restarted = task::spawn(config()).await.unwrap();
    restarted
        .handle
        .submit(CoreCommand::OpenTask(task_file))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    restarted
        .handle
        .submit(CoreCommand::Resume)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    {
        let snapshot = restarted.snapshots.borrow();
        let task = snapshot.task.as_ref().unwrap();
        assert_eq!(
            task.restart_notice.as_deref(),
            Some(hfdx_core::storage::restart_owned_reason())
        );
        assert!(!task.record.files[large_index].completed);
    }
    tokio::time::timeout(
        Duration::from_secs(30),
        restarted.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
                || s.error.is_some()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        restarted.snapshots.borrow().error.is_none(),
        "{:?}",
        restarted.snapshots.borrow().error
    );
    assert_eq!(
        std::fs::read(destination.join("large-partial.bin")).unwrap(),
        (0..=255u8)
            .cycle()
            .take(32 * 1024 * 1024)
            .collect::<Vec<_>>()
    );
    restarted.handle.request_shutdown().unwrap();
    restarted.join().await.unwrap();
    let after: serde_json::Value = client
        .get(format!("{origin}/__state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before_small, small_gets(&after));
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"unowned data");
    assert!(after["events"].as_array().unwrap().iter().any(|event| {
        event["seq"].as_u64().unwrap() > before_seq
            && event["kind"] == "complete"
            && event["method"] == "GET"
            && event["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("large-partial.bin"))
            && event["range"] == ""
            && event["status"] == 200
            && event["bytes"] == 32 * 1024 * 1024
    }));
    let hash = Command::new("sha256sum")
        .arg(destination.join("large-partial.bin"))
        .output()
        .unwrap();
    assert!(hash.status.success());
    assert_eq!(
        String::from_utf8(hash.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap(),
        ready["fixtures"]["large-partial.bin"]["sha256"]
            .as_str()
            .unwrap()
    );
    drop(fixture);
    if let Some(path) = std::env::var_os("HFDX_RECOVERY_EVENTS") {
        std::fs::copy(dir.path().join("events.jsonl"), path).unwrap();
    }
}

#[tokio::test]
async fn cancel_load_and_new_catalog_preserve_the_current_task() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new("python")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/qa/fixture.py"
        ))
        .args(["--port", "0", "--rate", "0", "--ready-file"])
        .arg(dir.path().join("ready.json"))
        .arg("--log")
        .arg(dir.path().join("events.jsonl"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let fixture = Fixture(child);
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let origin = ready["origin"].as_str().unwrap().to_owned();
    let mut session = task::spawn(CoreConfig {
        paths: SettingsPaths {
            settings_file: dir.path().join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    })
    .await
    .unwrap();
    session
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: RepositoryRequest {
                endpoint: Endpoint::parse(&origin, true).unwrap(),
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            access: NetworkAccess::default(),
        }))
        .unwrap();
    let tree = format!(
        "/api/models/org/model/tree/{}",
        ready["commits"]["a"].as_str().unwrap()
    );
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let state: serde_json::Value = client
        .get(format!("{origin}/__state"))
        .query(&[("kind", "start"), ("path", &tree), ("timeout", "5")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        state["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    );
    tokio::time::timeout(
        Duration::from_secs(5),
        session.snapshots.wait_for(|snapshot| {
            snapshot.phase == UiPhase::Loading && snapshot.selection.state == CatalogState::Loading
        }),
    )
    .await
    .unwrap()
    .unwrap();
    session
        .handle
        .submit(CoreCommand::CancelLoad)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let snapshot = session.snapshots.borrow().clone();
    assert_eq!(snapshot.phase, UiPhase::Idle);
    assert_eq!(snapshot.selection.state, CatalogState::Empty);
    assert!(snapshot.selection.catalog.is_none());
    assert!(snapshot.task.is_none());
    let rejected = session
        .handle
        .submit(CoreCommand::ChangeSelection(SelectionChange::SelectMatches))
        .unwrap()
        .await
        .unwrap();
    assert!(rejected.is_err());
    let destination = dir.path().join("paused");
    std::fs::create_dir_all(destination.join(".hfdx")).unwrap();
    std::fs::write(
        destination.join(".hfdx/task.json"),
        br#"{"schema_version":1,"task_id":"0123456789abcdef0123456789abcdef","source":{"request":{"endpoint":{"url":"http://127.0.0.1:9/","allow_http":true},"kind":"model","repo_id":"org/old","requested_revision":"main"},"resolved_commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"files":[{"entry":{"path":"a.bin","size":1,"content_hash":null},"completed":false,"failure":null}],"options":{"connections_per_file":1,"concurrent_files":1},"proxy_address":null,"verify_after_download":true,"state":"paused","created_unix_ms":1,"updated_unix_ms":1}"#,
    )
    .unwrap();
    let task_file = destination.join(".hfdx/task.json");
    let before = std::fs::read(&task_file).unwrap();
    session
        .handle
        .submit(CoreCommand::OpenTask(task_file.clone()))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.snapshots.borrow().phase, UiPhase::Paused);
    session
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: RepositoryRequest {
                endpoint: Endpoint::parse(&origin, true).unwrap(),
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            access: NetworkAccess::default(),
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let reloaded = session.snapshots.borrow().clone();
    assert_eq!(reloaded.phase, UiPhase::Loading);
    assert_eq!(reloaded.task.as_ref().unwrap().destination, destination);
    assert_eq!(reloaded.selection.state, CatalogState::Empty);
    assert_eq!(std::fs::read(&task_file).unwrap(), before);
    session
        .handle
        .submit(CoreCommand::CancelLoad)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    drop(fixture);
}

#[tokio::test]
async fn start_records_requested_verification_without_changing_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new("python")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/qa/fixture.py"
        ))
        .args(["--port", "0", "--rate", "0", "--ready-file"])
        .arg(dir.path().join("ready.json"))
        .arg("--log")
        .arg(dir.path().join("events.jsonl"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let fixture = Fixture(child);
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let mut session = task::spawn(CoreConfig {
        paths: SettingsPaths {
            settings_file: dir.path().join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    })
    .await
    .unwrap();
    let mut settings = (*session.snapshots.borrow().settings).clone();
    settings.verify_after_download = true;
    session
        .handle
        .submit(CoreCommand::SaveSettings(settings))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: RepositoryRequest {
                endpoint: Endpoint::parse(ready["origin"].as_str().unwrap(), true).unwrap(),
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            access: NetworkAccess::default(),
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(15),
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
    session
        .handle
        .submit(CoreCommand::ChangeSelection(SelectionChange::SelectMatches))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let destination = dir.path().join("download");
    session
        .handle
        .submit(CoreCommand::Start(StartRequest {
            catalog,
            destination: destination.clone(),
            options: DownloadOptions::new(1, 1).unwrap(),
            verify_after_download: false,
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let record = session
        .snapshots
        .borrow()
        .task
        .as_ref()
        .unwrap()
        .record
        .clone();
    assert_eq!(record.verify_after_download, Some(false));
    assert!(session.snapshots.borrow().settings.verify_after_download);
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    let disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(destination.join(".hfdx/task.json")).unwrap())
            .unwrap();
    assert_eq!(disk["verify_after_download"], false);
    drop(fixture);
}

#[tokio::test]
async fn new_download_resets_draft_and_rejects_active_or_loading() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new("python")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/qa/fixture.py"
        ))
        .args(["--port", "0", "--rate", "0", "--ready-file"])
        .arg(dir.path().join("ready.json"))
        .arg("--log")
        .arg(dir.path().join("events.jsonl"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let fixture = Fixture(child);
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let origin = ready["origin"].as_str().unwrap().to_owned();
    let mut session = task::spawn(CoreConfig {
        paths: SettingsPaths {
            settings_file: dir.path().join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    })
    .await
    .unwrap();
    session
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: RepositoryRequest {
                endpoint: Endpoint::parse(&origin, true).unwrap(),
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            access: NetworkAccess::default(),
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(15),
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
    session
        .handle
        .submit(CoreCommand::ChangeSelection(SelectionChange::SelectMatches))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let destination = dir.path().join("download");
    session
        .handle
        .submit(CoreCommand::Start(StartRequest {
            catalog,
            destination: destination.clone(),
            options: DownloadOptions::new(1, 1).unwrap(),
            verify_after_download: false,
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let pause = session.handle.submit(CoreCommand::Pause).unwrap();
    pause.await.unwrap().unwrap();
    assert_eq!(session.snapshots.borrow().phase, UiPhase::Paused);
    let task_file = destination.join(".hfdx/task.json");
    let before = std::fs::read(&task_file).unwrap();
    session
        .handle
        .submit(CoreCommand::NewDownload)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let reset = session.snapshots.borrow().clone();
    assert_eq!(reset.phase, UiPhase::Idle);
    assert_eq!(reset.selection.state, CatalogState::Empty);
    assert!(reset.selection.catalog.is_none());
    assert!(reset.task.is_none());
    assert_eq!(std::fs::read(&task_file).unwrap(), before);
    let rejected = session
        .handle
        .submit(CoreCommand::ChangeSelection(SelectionChange::SelectMatches))
        .unwrap()
        .await
        .unwrap();
    assert!(matches!(
        rejected,
        Err(task::CoreError::Selection(
            hfdx_core::selection::SelectionError::NotReady
        ))
    ));
    session
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: RepositoryRequest {
                endpoint: Endpoint::parse(&origin, true).unwrap(),
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            access: NetworkAccess::default(),
        }))
        .unwrap();
    let tree = format!(
        "/api/models/org/model/tree/{}",
        ready["commits"]["a"].as_str().unwrap()
    );
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let state: serde_json::Value = client
        .get(format!("{origin}/__state"))
        .query(&[("kind", "start"), ("path", &tree), ("timeout", "5")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        state["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    );
    tokio::time::timeout(
        Duration::from_secs(5),
        session.snapshots.wait_for(|snapshot| {
            snapshot.phase == UiPhase::Loading && snapshot.selection.state == CatalogState::Loading
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let rejected = session
        .handle
        .submit(CoreCommand::NewDownload)
        .unwrap()
        .await
        .unwrap();
    assert!(matches!(
        rejected,
        Err(task::CoreError::InvalidState { .. })
    ));
    session
        .handle
        .submit(CoreCommand::CancelLoad)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    drop(fixture);
}

#[tokio::test]
async fn completed_files_verify_while_other_files_download_with_shared_slots() {
    use hfdx_core::logging::{LogConfig, SessionLogger};
    use hfdx_core::storage::VerificationStatus;
    for limit in [1, 2] {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new("python")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/qa/fixture.py"
            ))
            .args([
                "--port",
                "0",
                "--rate",
                "2097152",
                "--hashes",
                "--ready-file",
            ])
            .arg(dir.path().join("ready.json"))
            .arg("--log")
            .arg(dir.path().join("requests.jsonl"))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let fixture = Fixture(child);
        let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
        let paths = SettingsPaths {
            settings_file: dir.path().join("settings.json"),
        };
        let mut settings = hfdx_core::storage::load_settings(&paths).await.unwrap();
        settings.debug_logging = true;
        hfdx_core::storage::save_settings(&paths, &settings)
            .await
            .unwrap();
        let logger = std::sync::Arc::new(
            SessionLogger::new(LogConfig::new(dir.path().join("logs"))).unwrap(),
        );
        let mut session = task::spawn_with_services(
            CoreConfig {
                paths,
                aria2_executable: "aria2c".into(),
            },
            std::sync::Arc::new(|_| {}),
            Some(logger.clone()),
        )
        .await
        .unwrap();
        session
            .handle
            .submit(CoreCommand::Load(LoadRequest {
                repository: RepositoryRequest {
                    endpoint: Endpoint::parse(ready["origin"].as_str().unwrap(), true).unwrap(),
                    kind: RepoKind::Model,
                    repo_id: RepoId::parse("org/model").unwrap(),
                    requested_revision: Revision::parse("main").unwrap(),
                },
                access: NetworkAccess::default(),
            }))
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(15),
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
        let destination = dir.path().join("download");
        session
            .handle
            .submit(CoreCommand::Start(StartRequest {
                catalog,
                destination: destination.clone(),
                options: DownloadOptions::new(1, limit).unwrap(),
                verify_after_download: true,
            }))
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        let mut early = false;
        let mut resumed = false;
        let mut trace = Vec::new();
        tokio::time::timeout(Duration::from_secs(55), async {
            loop {
                let snapshot = session.snapshots.borrow().clone();
                assert!(snapshot.error.is_none(), "{:?}",snapshot.error);
                if let Some(task) = &snapshot.task {
                    let small = task.record.files.iter().find(|f|f.entry.path.as_str()=="small-complete.bin").unwrap();
                    let large = task.record.files.iter().find(|f|f.entry.path.as_str()=="large-partial.bin").unwrap();
                    early |= small.verification == VerificationStatus::Passed && !large.completed;
                    let occupied = task.progress.active.len() + usize::from(task.verification.is_some());
                    assert!(occupied <= usize::from(limit), "combined download/hash slots exceeded {limit}");
                    trace.push(serde_json::json!({"phase":format!("{:?}",snapshot.phase),"active":task.progress.active.len(),"verifying":task.verification.map(|v|v.file_index),"small_passed":small.verification==VerificationStatus::Passed,"large_downloaded":large.completed,"budget":limit}));
                    if task.record.state == TaskState::Completed {
                        assert!(task.record.files.iter().all(|f|f.completed && f.verification==VerificationStatus::Passed));
                        break;
                    }
                }
                if early && limit == 2 && !resumed {
                    #[cfg(target_os = "linux")]
                    {
                        // The isolated engine is still transferring the large file after the small hash passed.
                        logger.flush().unwrap();
                        let log = std::fs::read_dir(logger.directory()).unwrap().find_map(|entry| {
                            std::fs::read_to_string(entry.unwrap().path().join("events_rCURRENT.log")).ok()
                        }).unwrap();
                        let pid = log.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                            .find(|line| line["event"]["event"] == "engine_launch" && line["metrics"]["child_pid"].is_u64())
                            .unwrap()["metrics"]["child_pid"].as_u64().unwrap().to_string();
                        assert!(Command::new("kill").args(["-STOP", &pid]).status().unwrap().success());
                        tokio::time::sleep(Duration::from_millis(11500)).await;
                        let bytes = session.snapshots.borrow().task.as_ref().unwrap().progress.acquired_bytes;
                        assert!(Command::new("kill").args(["-CONT", &pid]).status().unwrap().success());
                        tokio::time::timeout(Duration::from_secs(8), session.snapshots.wait_for(|s| {
                            s.error.is_some() || s.task.as_ref().unwrap().progress.acquired_bytes > bytes
                        })).await.unwrap().unwrap();
                        assert!(session.snapshots.borrow().error.is_none(), "Temporary status failures must not fail the task: {:?}", session.snapshots.borrow().error);
                    }
                    session.handle.submit(CoreCommand::Pause).unwrap().await.unwrap().unwrap();
                    assert_eq!(session.snapshots.borrow().phase, UiPhase::Paused);
                    assert_eq!(session.snapshots.borrow().task.as_ref().unwrap().record.files.iter().find(|f| f.entry.path.as_str()=="small-complete.bin").unwrap().verification, VerificationStatus::Passed);
                    session.handle.submit(CoreCommand::Resume).unwrap().await.unwrap().unwrap();
                    resumed = true;
                    continue;
                }
                session.snapshots.changed().await.unwrap();
            }
        }).await.unwrap();
        assert!(
            early,
            "small file must pass SHA-256 before the large file finishes, budget {limit}"
        );
        let requests: Vec<serde_json::Value> =
            std::fs::read_to_string(dir.path().join("requests.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        let small_bodies = requests
            .iter()
            .filter(|e| {
                e["kind"] == "complete"
                    && e["method"] == "GET"
                    && e["path"]
                        .as_str()
                        .is_some_and(|p| p.ends_with("small-complete.bin"))
                    && e["bytes"] == 262144
            })
            .count();
        assert_eq!(
            small_bodies, 1,
            "verified small file is never downloaded again"
        );
        if limit == 2 {
            assert!(resumed);
            assert!(
                requests.iter().any(|e| e["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("large-partial.bin"))
                    && e["range"]
                        .as_str()
                        .is_some_and(|r| r.starts_with("bytes=") && !r.starts_with("bytes=0-"))),
                "unfinished file resumes with a nonzero Range"
            );
        }
        if let Ok(root) = std::env::var("HFDX_PIPELINE_EVIDENCE") {
            let root = std::path::PathBuf::from(root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join(format!("slots-{limit}.json")),
                serde_json::to_vec_pretty(&trace).unwrap(),
            )
            .unwrap();
            std::fs::copy(
                dir.path().join("requests.jsonl"),
                root.join(format!("requests-{limit}.jsonl")),
            )
            .unwrap();
            std::fs::copy(
                destination.join(".hfdx/task.json"),
                root.join(format!("task-{limit}.json")),
            )
            .unwrap();
        }
        session.handle.request_shutdown().unwrap();
        session.join().await.unwrap();
        drop(fixture);
    }
}
