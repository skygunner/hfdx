use super::*;

pub async fn public_hf_download_through_authenticated_connect_proxy() {
    let root = tempfile::tempdir().unwrap();
    let log = std::fs::File::create(root.path().join("proxy.jsonl")).unwrap();
    let mut proxy = Proxy(
        Command::new("python")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/qa/round2_network_proxy.py"
            ))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = tokio::process::ChildStdout::from_std(proxy.0.stdout.take().unwrap()).unwrap();
    let mut output = BufReader::new(stdout);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), output.read_line(&mut line))
        .await
        .expect("proxy readiness timed out")
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let port = ready["port"].as_u64().unwrap();
    let log_thread = tokio::spawn(async move {
        let mut writer = tokio::fs::File::from_std(log);
        tokio::io::copy(&mut output, &mut writer).await.unwrap();
    });
    let request = RepositoryRequest {
        endpoint: Endpoint::parse("https://huggingface.co", false).unwrap(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("gpt2").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    let access = |credential: &str| NetworkAccess {
        token: None,
        proxy: Some(
            ProxySpec::parse(&format!("http://proof:{credential}@127.0.0.1:{port}")).unwrap(),
        ),
    };
    let wrong = RepositoryClient::new(&request.endpoint, access("incorrect")).unwrap();
    assert!(
        wrong
            .resolve(&request, &CancellationToken::new())
            .await
            .is_err()
    );
    let client = RepositoryClient::new(&request.endpoint, access("round2")).unwrap();
    let resolved = client
        .resolve(&request, &CancellationToken::new())
        .await
        .unwrap();
    println!("repository_commit={}", resolved.resolved_commit.as_str());
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
            access: access("round2"),
        }))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(60),
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
        .submit(CoreCommand::SetFilter("config.json".into()))
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
        Duration::from_secs(90),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| matches!(t.record.state, TaskState::Completed | TaskState::Failed))
                || s.error.is_some()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let snapshot = session.snapshots.borrow();
    assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
    assert_eq!(
        snapshot.task.as_ref().unwrap().record.state,
        TaskState::Completed
    );
    drop(snapshot);
    let bytes = std::fs::read(destination.join("config.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["model_type"],
        "gpt2"
    );
    let hash = Command::new("sha256sum")
        .arg(destination.join("config.json"))
        .output()
        .unwrap();
    assert!(hash.status.success());
    println!(
        "public_bytes={} sha256={}",
        bytes.len(),
        String::from_utf8(hash.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
    );
    session.handle.request_shutdown().unwrap();
    println!("engine_shutdown={:?}", session.join().await.unwrap());
    drop(proxy);
    log_thread.await.unwrap();
    let journal = std::fs::read_to_string(root.path().join("proxy.jsonl")).unwrap();
    print!("{journal}");
    let events: Vec<serde_json::Value> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "connect" && e["authorized"] == false)
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "connect" && e["authorized"] == true)
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "tunnel_closed" && e["origin_bytes"].as_u64().unwrap_or(0) > 0)
    );
}
