use super::*;

fn signal(pid: u32, signal: &str) {
    assert!(
        std::process::Command::new("kill")
            .args([signal, &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn pause_resume_remove_preserve_control_for_explicit_restart() {
    let root = tempfile::Builder::new()
        .prefix("engine-control-")
        .tempdir()
        .unwrap();
    let fixture = Fixture::start(root.path()).await;
    let endpoint = Endpoint::parse(&fixture.ready.origin, true).unwrap();
    let client = RepositoryClient::new(&endpoint, NetworkAccess::default()).unwrap();
    let source = client
        .resolve(
            &RepositoryRequest {
                endpoint,
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let entry = CatalogEntry {
        path: RepoPath::parse(LARGE).unwrap(),
        size: Bytes::new(33554432),
        content_hash: None,
    };
    let record = new_record(
        source.clone(),
        vec![entry.clone()],
        DownloadOptions::default(),
    )
    .unwrap();
    let mut store = TaskStore::create(&root.path().join("downloads"), &record)
        .await
        .unwrap();
    let mut engine = engine().await;
    let path = format!(
        "/org/model/resolve/{}/{LARGE}",
        source.resolved_commit.as_str()
    );
    let (gid, _) = tokio::join!(
        async {
            let file = store.prepare(&entry, ResumeAction::Create).await.unwrap();
            let url = client
                .download_url(&source, &entry, &CancellationToken::new())
                .await
                .unwrap();
            #[cfg(target_os = "linux")]
            let pid = {
                let pid = engine.pid().to_string();
                assert!(
                    std::process::Command::new("kill")
                        .args(["-STOP", &pid])
                        .status()
                        .unwrap()
                        .success()
                );
                pid
            };
            let (added, ()) = tokio::join!(
                engine.add(DownloadJob {
                    url,
                    file,
                    proxy: None,
                }),
                async {
                    #[cfg(target_os = "linux")]
                    {
                        tokio::time::sleep(Duration::from_millis(11500)).await;
                        assert!(
                            std::process::Command::new("kill")
                                .args(["-CONT", &pid])
                                .status()
                                .unwrap()
                                .success()
                        );
                    }
                }
            );
            added.expect("slow addUri must not discard a valid download")
        },
        fixture.wait_bytes(&path, 0, 1048576)
    );
    let totals: Value = engine
        .rpc
        .call("aria2.getGlobalStat", vec![])
        .await
        .unwrap();
    assert_eq!(totals["numActive"], "1");
    assert_eq!(totals["numWaiting"], "0");
    assert_eq!(totals["numStoppedTotal"], "0");
    let cursor = fixture.events(0).await.last().unwrap()["seq"]
        .as_u64()
        .unwrap();
    let (_, result) = tokio::join!(fixture.wait_interrupted(&path, cursor), engine.pause_all());
    result.unwrap();
    assert_eq!(
        engine.statuses(&[gid]).await.unwrap()[0].state,
        EngineState::Paused
    );
    #[cfg(target_os = "linux")]
    {
        let pid = engine.pid().to_string();
        assert!(
            std::process::Command::new("kill")
                .args(["-STOP", &pid])
                .status()
                .unwrap()
                .success()
        );
        let gids = [gid];
        let (result, ()) = tokio::join!(engine.statuses(&gids), async {
            tokio::time::sleep(Duration::from_millis(2200)).await;
            assert!(
                std::process::Command::new("kill")
                    .args(["-CONT", &pid])
                    .status()
                    .unwrap()
                    .success()
            );
        });
        assert_eq!(result.unwrap()[0].state, EngineState::Paused);
    }
    let cursor = fixture.events(0).await.last().unwrap()["seq"]
        .as_u64()
        .unwrap();
    let (_, result) = tokio::join!(
        fixture.wait_bytes(&path, cursor, 16384),
        engine.resume_all()
    );
    result.unwrap();
    assert_eq!(
        engine.statuses(&[gid]).await.unwrap()[0].state,
        EngineState::Active
    );
    let cursor = fixture.events(0).await.last().unwrap()["seq"]
        .as_u64()
        .unwrap();
    let (_, result) = tokio::join!(fixture.wait_interrupted(&path, cursor), engine.remove(&gid));
    result.unwrap();
    let status = engine.statuses(&[gid]).await.unwrap().remove(0);
    assert_eq!(status.state, EngineState::Failed);
    assert_eq!(status.failure.unwrap().kind, FailureKind::Interrupted);
    assert!(store.destination().join(format!("{LARGE}.aria2")).exists());
    engine.forget(&gid).await.unwrap();
    stop(engine).await;
    assert_eq!(
        store.inspect(&record).await.unwrap()[0].action,
        ResumeAction::ResumeControl
    );
    fixture.close().await;
    drop(store);
    root.close().unwrap();
}

#[tokio::test]
async fn active_shutdown_saves_control_and_retains_native_incomplete_exit_code() {
    active_shutdown(false).await;
}
#[tokio::test]
async fn active_shutdown_reports_control_file_save_failure() {
    active_shutdown(true).await;
}
async fn active_shutdown(block_save: bool) {
    let root = tempfile::Builder::new()
        .prefix("engine-active-stop-")
        .tempdir()
        .unwrap();
    let fixture = Fixture::start(root.path()).await;
    let endpoint = Endpoint::parse(&fixture.ready.origin, true).unwrap();
    let client = RepositoryClient::new(&endpoint, NetworkAccess::default()).unwrap();
    let source = client
        .resolve(
            &RepositoryRequest {
                endpoint,
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let entry = CatalogEntry {
        path: RepoPath::parse(LARGE).unwrap(),
        size: Bytes::new(33554432),
        content_hash: None,
    };
    let record = new_record(
        source.clone(),
        vec![entry.clone()],
        DownloadOptions::default(),
    )
    .unwrap();
    let mut store = TaskStore::create(&root.path().join("downloads"), &record)
        .await
        .unwrap();
    let mut engine = engine().await;
    let path = format!(
        "/org/model/resolve/{}/{LARGE}",
        source.resolved_commit.as_str()
    );
    let (_, _) = tokio::join!(
        async {
            let file = store.prepare(&entry, ResumeAction::Create).await.unwrap();
            let url = client
                .download_url(&source, &entry, &CancellationToken::new())
                .await
                .unwrap();
            engine
                .add(DownloadJob {
                    url,
                    file,
                    proxy: None,
                })
                .await
                .unwrap()
        },
        fixture.wait_bytes(&path, 0, 1048576)
    );
    let pid = engine.pid();
    let conf = private_path(pid).await;
    let control = store.destination().join(format!("{LARGE}.aria2"));
    if block_save {
        let old_control = store.destination().join("saved-control-backup");
        if control.exists() {
            std::fs::rename(&control, &old_control).unwrap();
        }
        std::fs::create_dir(&control).unwrap();
    }
    let delayed = cfg!(target_os = "linux") && !block_save;
    if delayed {
        signal(pid, "-STOP");
    }
    let started = Instant::now();
    let (result, ()) = tokio::join!(engine.shutdown(), async {
        if delayed {
            tokio::time::sleep(Duration::from_millis(11500)).await;
            signal(pid, "-CONT");
        }
    });
    let elapsed = started.elapsed();
    println!(
        "active_shutdown_result={result:?}; elapsed_ms={}; control_is_file={}; blocked_save={block_save}",
        elapsed.as_millis(),
        control.is_file()
    );
    fixture.close().await;
    if block_save {
        assert!(
            matches!(result, Err(aria2::ProbeError::Protocol)),
            "control save failure must remain visible: {result:?}"
        );
    } else {
        let receipt = result.expect("authenticated active shutdown with saved control");
        assert_eq!(receipt.stop, StopKind::Graceful);
        assert_eq!(receipt.child_exit_code, Some(7));
        assert!(std::fs::metadata(&control).unwrap().len() > 0);
    }
    if delayed {
        assert!(elapsed > Duration::from_secs(10) && elapsed < Duration::from_secs(30));
    } else {
        assert!(elapsed < Duration::from_secs(5));
    }
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert!(!conf.parent().unwrap().exists());
    drop(store);
    root.close().unwrap();
}

#[tokio::test]
async fn unresponsive_owned_child_is_forced_reaped_with_visible_receipt() {
    let engine = engine().await;
    let pid = engine.pid();
    let conf = private_path(pid).await;
    let stopped = tokio::process::Command::new("kill")
        .args(["-STOP", &pid.to_string()])
        .status()
        .await
        .unwrap();
    assert!(stopped.success());
    let started = Instant::now();
    let result = engine.shutdown().await;
    let receipt = match result {
        Err(aria2::ProbeError::Shutdown(receipt)) => receipt,
        other => panic!("forced shutdown must expose the failed RPC: {other:?}"),
    };
    assert_eq!(receipt.stop, StopKind::Forced);
    assert_eq!(receipt.child_pid, Some(pid));
    assert!(started.elapsed() >= Duration::from_secs(30));
    assert!(started.elapsed() < Duration::from_secs(36));
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert!(!conf.parent().unwrap().exists());
    println!(
        "forced_shutdown={}",
        serde_json::to_string(&receipt).unwrap()
    );
}
