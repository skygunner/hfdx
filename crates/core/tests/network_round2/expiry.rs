use super::*;

#[tokio::test]
async fn retry_failed_reresolves_expired_url_from_pinned_source() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("events.jsonl");
    let mut fixture = Proxy(
        Command::new("python")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/qa/round2_network_expiry.py"
            ))
            .arg(&log)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = tokio::process::ChildStdout::from_std(fixture.0.stdout.take().unwrap()).unwrap();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .expect("fixture readiness timed out")
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let request = RepositoryRequest {
        endpoint: Endpoint::parse(ready["origin"].as_str().unwrap(), true).unwrap(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("org/model").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    let mut session = task::spawn(CoreConfig {
        paths: SettingsPaths {
            settings_file: root.path().join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    })
    .await
    .unwrap();
    session
        .handle
        .submit(CoreCommand::Load(LoadRequest {
            repository: request,
            access: NetworkAccess {
                token: Some(hfdx_core::Token::parse("round2-bearer-marker").unwrap()),
                proxy: None,
            },
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(20),
        session
            .snapshots
            .wait_for(|s| s.selection.state == CatalogState::Ready || s.error.is_some()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(session.snapshots.borrow().error.is_none());
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
        .submit(CoreCommand::SetFilter("small-complete.bin".into()))
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
    let destination = root.path().join("download");
    session
        .handle
        .submit(CoreCommand::Start(StartRequest {
            catalog,
            destination: destination.clone(),
            options: DownloadOptions::new(1, 1).unwrap(),
            verify_after_download: true,
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(30),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Failed)
                || s.error.is_some()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(session.snapshots.borrow().error.is_none());
    {
        let snapshot = session.snapshots.borrow();
        let failure = &snapshot.task.as_ref().unwrap().progress.failed[0].1;
        assert_eq!(failure.http_status, Some(403));
        assert_eq!(failure.aria2_code, Some(22));
        let visible = hfdx_core::locale::file_failure(hfdx_core::Language::ZhHans, failure);
        assert!(visible.contains("HTTP 403") && visible.contains("aria2 22"));
        assert!(!visible.contains("signature="));
    }
    let first = std::fs::read_to_string(&log).unwrap();
    let events: Vec<serde_json::Value> = first
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e["role"] == "capture" && e["method"] == "GET" && e["status"] == 403)
    );
    println!("engine_403_observed=true");
    session
        .handle
        .submit(CoreCommand::RetryFailed)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(30),
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
    let bytes = std::fs::read(destination.join("small-complete.bin")).unwrap();
    assert_eq!(
        bytes,
        (0..=255u8).cycle().take(256 * 1024).collect::<Vec<_>>()
    );
    let hash = Command::new("sha256sum")
        .arg(destination.join("small-complete.bin"))
        .output()
        .unwrap();
    assert!(hash.status.success());
    println!(
        "retry_bytes={} sha256={}",
        bytes.len(),
        String::from_utf8(hash.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
    );
    let persisted = std::fs::read_to_string(destination.join(".hfdx/task.json")).unwrap();
    assert!(
        !persisted.contains("round2-bearer-marker")
            && !persisted.contains("signature")
            && !persisted.contains("round2-1")
    );
    session.handle.request_shutdown().unwrap();
    println!("retry_engine_shutdown={:?}", session.join().await.unwrap());
    let events = std::fs::read_to_string(&log).unwrap();
    print!("{events}");
    let parsed: Vec<serde_json::Value> = events
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let resolves: Vec<_> = parsed
        .iter()
        .filter(|e| {
            e["role"] == "origin"
                && e["method"] == "HEAD"
                && e["kind"] == "start"
                && e["path"].as_str().is_some_and(|p| p.contains("/resolve/"))
        })
        .collect();
    assert_eq!(resolves.len(), 2);
    assert!(
        resolves
            .iter()
            .all(|e| e["path"].as_str().unwrap().contains(&"a".repeat(40)))
    );
    assert!(
        parsed
            .iter()
            .filter(|e| e["role"] == "capture")
            .all(|e| e["auth_present"] == false && e["proxy_auth_present"] == false)
    );
    println!("pinned_source_resolve_count={}", resolves.len());
    drop(fixture);
}
