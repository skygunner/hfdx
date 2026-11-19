use crate::aria2::{self, Aria2, Aria2Launch, DownloadJob, EngineState, StopKind};
#[cfg(target_os = "linux")]
#[path = "session/collision.rs"]
mod collision;
#[path = "session/control.rs"]
mod control;
#[path = "session/fixture.rs"]
mod fixture;
use crate::repository::*;
use crate::storage::*;
use fixture::{Fixture, body_count, hash};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
const SMALL: &str = "small-complete.bin";
const LARGE: &str = "large-partial.bin";

async fn engine() -> Aria2 {
    let mut engine = Aria2::launch(Aria2Launch {
        executable: "aria2c".into(),
    })
    .await
    .expect("real engine launch");
    engine
        .configure(&DownloadOptions::new(4, 5).unwrap())
        .await
        .expect("4 connections, 5 files");
    engine
}
async fn private_path(pid: u32) -> PathBuf {
    let cmdline = tokio::fs::read(format!("/proc/{pid}/cmdline"))
        .await
        .expect("owned child argv");
    let args: Vec<_> = cmdline.split(|b| *b == 0).collect();
    let position = args
        .iter()
        .position(|a| *a == b"--conf-path")
        .expect("private config argument");
    PathBuf::from(std::str::from_utf8(args[position + 1]).expect("config path UTF8"))
}
async fn stop(engine: Aria2) -> Value {
    let pid = engine.pid();
    let conf = private_path(pid).await;
    assert!(!conf.exists(), "secret config removed after authentication");
    let directory = conf.parent().unwrap().to_path_buf();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let started = Instant::now();
    let receipt = engine
        .shutdown()
        .await
        .expect("shutdown and save/wait/cleanup");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "single shutdown budget: {elapsed:?}"
    );
    assert_eq!(receipt.stop, StopKind::Graceful);
    assert_eq!(receipt.child_exit_code, Some(0));
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert!(!directory.exists());
    json!({"receipt":receipt,"elapsed_ms":elapsed.as_millis(),"pid_absent":true,"config_removed":true,"directory_removed":true})
}
#[tokio::test]
async fn real_transfer_resumes_control_without_redownloading_completed_file() {
    let root = tempfile::Builder::new()
        .prefix("engine-session-")
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
    let entries = [SMALL, LARGE].map(|name| CatalogEntry {
        path: RepoPath::parse(name).unwrap(),
        size: Bytes::new(fixture.ready.fixtures[name].size),
        content_hash: None,
    });
    assert_eq!(
        [entries[0].size.get(), entries[1].size.get()],
        [262144, 33554432]
    );
    let mut record = new_record(
        source.clone(),
        entries.to_vec(),
        DownloadOptions::new(4, 5).unwrap(),
    )
    .unwrap();
    let destination = root.path().join("downloads");
    let mut store = TaskStore::create(&destination, &record).await.unwrap();
    let mut first = engine().await;
    let small_path = format!(
        "/org/model/resolve/{}/{SMALL}",
        source.resolved_commit.as_str()
    );
    let large_path = format!(
        "/org/model/resolve/{}/{LARGE}",
        source.resolved_commit.as_str()
    );
    let small_wait = fixture.wait_bytes(&small_path, 0, 262144);
    let large_wait = fixture.wait_bytes(&large_path, 0, 1048576);
    let (gids, _, _) = tokio::join!(
        async {
            let mut gids = Vec::new();
            for entry in &entries {
                let file = store.prepare(entry, ResumeAction::Create).await.unwrap();
                let url = client
                    .download_url(&source, entry, &CancellationToken::new())
                    .await
                    .unwrap();
                gids.push(
                    first
                        .add(DownloadJob {
                            url,
                            file,
                            proxy: None,
                        })
                        .await
                        .unwrap(),
                );
            }
            record.state = TaskState::Running;
            store.persist(&record).await.unwrap();
            gids
        },
        small_wait,
        large_wait
    );
    let statuses = first.statuses(&gids).await.unwrap();
    assert_eq!(statuses[0].state, EngineState::Complete);
    assert_eq!(statuses[0].completed_bytes.get(), 262144);
    assert_eq!(statuses[1].state, EngineState::Active);
    assert!(statuses[1].completed_bytes.get() > 0 && statuses[1].completed_bytes.get() < 33554432);
    assert_eq!(statuses[1].total_bytes.get(), 33554432);
    assert!(statuses.iter().all(|s| s.failure.is_none()));
    store.verify_size(&entries[0]).await.unwrap();
    record.files[0].completed = true;
    store.persist(&record).await.unwrap();
    first.forget(&gids[0]).await.unwrap();
    let pause_cursor = fixture.events(0).await.last().unwrap()["seq"]
        .as_u64()
        .unwrap();
    let (_, paused) = tokio::join!(
        fixture.wait_interrupted(&large_path, pause_cursor),
        first.pause_all()
    );
    paused.unwrap();
    assert_eq!(
        first.statuses(&gids[1..]).await.unwrap()[0].state,
        EngineState::Paused
    );
    let first_stop = stop(first).await;
    assert!(destination.join(format!("{LARGE}.aria2")).exists());
    let task_path = store.task_file();
    drop(store);
    let opened = TaskStore::open(&task_path).await.unwrap();
    let mut store = opened.store;
    let mut record = opened.record;
    assert_eq!(record.state, TaskState::Paused);
    let actions = store.inspect(&record).await.unwrap();
    assert_eq!(
        actions.iter().map(|i| i.action).collect::<Vec<_>>(),
        [ResumeAction::SkipSizeChecked, ResumeAction::ResumeControl]
    );
    let before = fixture.events(0).await;
    let small_before = body_count(&before, SMALL);
    assert_eq!(small_before, 1);
    let cursor = before.last().unwrap()["seq"].as_u64().unwrap();
    let mut second = engine().await;
    let resume_events = fixture.wait_bytes(&large_path, cursor, 16384);
    let (large_gid, _) = tokio::join!(
        async {
            let file = store.prepare(&entries[1], actions[1].action).await.unwrap();
            let url = client
                .download_url(&source, &entries[1], &CancellationToken::new())
                .await
                .unwrap();
            second
                .add(DownloadJob {
                    url,
                    file,
                    proxy: None,
                })
                .await
                .unwrap()
        },
        resume_events
    );
    let resumed = fixture.events(cursor).await;
    let ranges: Vec<_> = resumed
        .iter()
        .filter(|e| e["kind"] == "start" && e["method"] == "GET")
        .filter_map(|e| e["range"].as_str())
        .collect();
    assert!(
        ranges.iter().any(|r| r
            .strip_prefix("bytes=")
            .and_then(|r| r.split('-').next())
            .and_then(|n| n.parse::<u64>().ok())
            .is_some_and(|n| n > 0)),
        "nonzero resume Range"
    );
    let (_, _) = tokio::join!(fixture.unlimit(), async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut after = cursor;
            loop {
                let events = fixture.events(after).await;
                let statuses = second.statuses(&[large_gid]).await.unwrap();
                match statuses[0].state {
                    EngineState::Complete => break,
                    EngineState::Failed => panic!("download failed: {:?}", statuses[0].failure),
                    EngineState::Active | EngineState::Waiting | EngineState::Paused => {}
                }
                if let Some(seq) = events.last().and_then(|e| e["seq"].as_u64()) {
                    after = seq;
                }
            }
        })
        .await
        .expect("complete after progress events");
    });
    store.verify_size(&entries[1]).await.unwrap();
    record.files[1].completed = true;
    record.state = TaskState::Completed;
    store.persist(&record).await.unwrap();
    second.forget(&large_gid).await.unwrap();
    let second_stop = stop(second).await;
    let all_events = fixture.events(0).await;
    assert_eq!(body_count(&all_events, SMALL), small_before);
    let mut hashes = serde_json::Map::new();
    for name in [SMALL, LARGE] {
        let actual = hash(&destination.join(name)).await;
        assert_eq!(actual, fixture.ready.fixtures[name].sha256);
        hashes.insert(name.into(), json!(actual));
    }
    let fixture_pid = fixture.close().await;
    assert!(!Path::new(&format!("/proc/{fixture_pid}")).exists());
    let proof = json!({"source_commit":source.resolved_commit.as_str(),
        "sizes":[262144,33554432],"options":{"connections":4,"files":5},
        "first_shutdown":first_stop,"second_shutdown":second_stop,"small_body_requests_before":small_before,
        "small_body_requests_after":body_count(&all_events,SMALL),"hashes":hashes,
        "fixture_pid":fixture_pid,"fixture_pid_absent":true,"resume_ranges":ranges});
    if let Some(directory) = std::env::var_os("HFDX_ENGINE_EVIDENCE_DIR") {
        let directory = PathBuf::from(directory);
        tokio::fs::write(
            directory.join("m2-engine-transfer.json"),
            serde_json::to_vec_pretty(&proof).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::copy(
            root.path().join("requests.jsonl"),
            directory.join("m2-engine-requests.jsonl"),
        )
        .await
        .unwrap();
    }
    println!("{}", serde_json::to_string_pretty(&proof).unwrap());
    drop(store);
    root.close().expect("owned test root cleanup");
}
