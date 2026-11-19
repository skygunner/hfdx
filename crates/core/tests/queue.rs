use hfdx_core::{
    task::{
        self, ConflictPolicy, CoreCommand, CoreConfig, CoreSession, LoadRequest, StartRequest,
        UiPhase,
    },
    *,
};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    time::Duration,
};
struct Fixture(Child, std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.0.kill().unwrap();
            let status = self.0.wait().unwrap();
            println!("fixture pid={} joined={status}", self.0.id());
        }
        if let Some(evidence) = std::env::var_os("HFDX_QUEUE_EVIDENCE") {
            let dest = std::path::PathBuf::from(evidence).join(self.1.file_name().unwrap());
            std::fs::create_dir_all(&dest).unwrap();
            for name in ["requests.jsonl", "ready.json"] {
                std::fs::copy(self.1.join(name), dest.join(name)).unwrap();
            }
            println!("wire evidence={}", dest.display());
        }
    }
}
fn fixture(root: &std::path::Path, rate: &str, hashes: bool) -> (Fixture, Endpoint) {
    let mut child = Command::new("python")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/qa/fixture.py"
        ))
        .args(["--port", "0", "--rate", rate, "--ready-file"])
        .arg(root.join("ready.json"))
        .arg("--log")
        .arg(root.join("requests.jsonl"))
        .args(hashes.then_some("--hashes"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    (
        Fixture(child, root.into()),
        Endpoint::parse(ready["origin"].as_str().unwrap(), true).unwrap(),
    )
}
async fn send(session: &CoreSession, command: CoreCommand) {
    session
        .handle
        .submit(command)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
}
async fn ready(session: &mut CoreSession, endpoint: &Endpoint, repo: &str, filter: &str) {
    send(
        session,
        CoreCommand::Load(LoadRequest {
            repository: RepositoryRequest {
                endpoint: endpoint.clone(),
                kind: RepoKind::Model,
                repo_id: RepoId::parse(repo).unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            },
            access: NetworkAccess::default(),
        }),
    )
    .await;
    tokio::time::timeout(
        Duration::from_secs(10),
        session.snapshots.wait_for(|s| {
            s.selection.state == CatalogState::Ready
                && s.selection
                    .catalog
                    .as_ref()
                    .is_some_and(|c| c.source.request.repo_id.as_str() == repo)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    send(session, CoreCommand::SetFilter(filter.into())).await;
    send(
        session,
        CoreCommand::ChangeSelection(SelectionChange::SelectMatches),
    )
    .await;
}
async fn add(session: &CoreSession, path: &std::path::Path) {
    let catalog = session
        .snapshots
        .borrow()
        .selection
        .catalog
        .as_ref()
        .unwrap()
        .id;
    send(
        session,
        CoreCommand::Start(StartRequest {
            catalog,
            destination: path.into(),
            options: DownloadOptions::new(1, 2).unwrap(),
            verify_after_download: true,
        }),
    )
    .await;
}
fn config(root: &std::path::Path) -> CoreConfig {
    CoreConfig {
        paths: SettingsPaths {
            settings_file: root.join("settings.json"),
        },
        aria2_executable: "aria2c".into(),
    }
}
async fn queue(root: &std::path::Path) -> CoreSession {
    task::spawn_queue_with_services(config(root), std::sync::Arc::new(|_| {}), None)
        .await
        .unwrap()
}

#[tokio::test]
async fn removing_tasks_requires_confirmation_and_preserves_downloads_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let (_fixture, endpoint) = fixture(root.path(), "1048576", false);
    let mut session = queue(root.path()).await;
    let active = root.path().join("A");
    ready(&mut session, &endpoint, "org/A", "large-partial.bin").await;
    add(&session, &active).await;
    let active_id = tokio::time::timeout(
        Duration::from_secs(15),
        session.snapshots.wait_for(|s| {
            s.task.as_ref().is_some_and(|t| {
                t.progress
                    .active
                    .iter()
                    .any(|file| file.completed_bytes.get() > 0)
            })
        }),
    )
    .await
    .unwrap()
    .unwrap()
    .task
    .as_ref()
    .unwrap()
    .record
    .task_id
    .clone();
    let rejected = session
        .handle
        .submit(CoreCommand::RemoveTask {
            id: active_id.clone(),
            confirmed: false,
        })
        .unwrap()
        .await
        .unwrap();
    assert!(matches!(
        rejected,
        Err(task::CoreError::InvalidState { .. })
    ));
    assert_eq!(
        session
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .tasks
            .len(),
        1
    );
    send(
        &session,
        CoreCommand::RemoveTask {
            id: active_id,
            confirmed: true,
        },
    )
    .await;
    assert!(active.join("large-partial.bin.aria2").is_file());
    assert!(active.join(".hfdx/task.json").is_file());

    let done = root.path().join("B");
    ready(&mut session, &endpoint, "org/B", "small-complete.bin").await;
    add(&session, &done).await;
    let done_id = tokio::time::timeout(
        Duration::from_secs(15),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
        }),
    )
    .await
    .unwrap()
    .unwrap()
    .task
    .as_ref()
    .unwrap()
    .record
    .task_id
    .clone();
    send(
        &session,
        CoreCommand::RemoveTask {
            id: done_id,
            confirmed: false,
        },
    )
    .await;
    assert_eq!(
        std::fs::metadata(done.join("small-complete.bin"))
            .unwrap()
            .len(),
        262144
    );
    assert!(done.join(".hfdx/task.json").is_file());
    assert!(session.snapshots.borrow().recent.is_empty());
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    let restored = queue(root.path()).await;
    assert!(
        restored
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .tasks
            .is_empty()
    );
    restored.handle.request_shutdown().unwrap();
    restored.join().await.unwrap();
}
#[tokio::test]
async fn multiple_repositories_append_hash_reuse_and_explicit_queue_resume() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "2097152", true);
    let mut session = queue(root).await;
    let mut settings = (*session.snapshots.borrow().settings).clone();
    settings.options = DownloadOptions::new(1, 2).unwrap();
    send(&session, CoreCommand::SaveSettings(settings)).await;
    let a = root.join("A");
    std::fs::create_dir(&a).unwrap();
    std::fs::write(a.join("unrelated-Q3.gguf"), b"keep Q3").unwrap();
    std::fs::write(
        a.join("small-complete.bin"),
        (0..=255u8).cycle().take(262144).collect::<Vec<_>>(),
    )
    .unwrap();
    ready(&mut session, &endpoint, "org/A", "large-partial.bin").await;
    add(&session, &a).await;
    let first = session
        .snapshots
        .borrow()
        .task
        .as_ref()
        .unwrap()
        .record
        .task_id
        .clone();
    ready(&mut session, &endpoint, "org/B", "small-complete.bin").await;
    add(&session, &root.join("B")).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        session.snapshots.wait_for(|s| {
            s.queue.as_ref().is_some_and(|q| {
                q.tasks.len() == 2
                    && q.tasks.iter().any(|t| {
                        t.view.record.source.request.repo_id.as_str() == "org/B"
                            && t.view.record.state == TaskState::Completed
                    })
            })
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        session
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .tasks
            .iter()
            .any(|t| t.view.record.task_id == first && t.view.record.state == TaskState::Running)
    );
    ready(
        &mut session,
        &endpoint,
        "org/A",
        "small-complete.bin\nempty.bin",
    )
    .await;
    add(&session, &a).await;
    {
        let snap = session.snapshots.borrow();
        let q = snap.queue.as_ref().unwrap();
        assert!(q.used_slots <= 2);
        assert_eq!(q.tasks.len(), 2);
        let t = q
            .tasks
            .iter()
            .find(|t| t.view.record.task_id == first)
            .unwrap();
        assert_eq!(t.view.record.files.len(), 3);
        let f = t
            .view
            .record
            .files
            .iter()
            .find(|f| f.entry.path.as_str() == "small-complete.bin")
            .unwrap();
        assert_eq!(f.verification, VerificationStatus::Passed);
        assert_eq!(f.disposition, storage::FileDisposition::ExistingMatch);
    }
    send(&session, CoreCommand::PauseTask(first.clone())).await;
    assert!(
        session
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .tasks
            .iter()
            .any(|t| t.view.record.task_id == first && t.phase == UiPhase::Paused)
    );
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    let mut reopened = queue(root).await;
    assert_eq!(
        reopened
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .tasks
            .len(),
        2,
        "both persisted tasks must be restored"
    );
    assert!(
        reopened
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .tasks
            .iter()
            .all(|t| !matches!(t.phase, UiPhase::Downloading | UiPhase::Verifying))
    );
    send(&reopened, CoreCommand::ResumeAll).await;
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let snap = reopened.snapshots.borrow().clone();
            let q = snap.queue.as_ref().unwrap();
            assert!(q.used_slots <= 2);
            assert!(
                q.tasks
                    .iter()
                    .map(|t| t.view.progress.active.len()
                        + usize::from(t.view.verification.is_some()))
                    .sum::<usize>()
                    <= 2
            );
            if q.tasks
                .iter()
                .all(|t| t.view.record.state == TaskState::Completed)
            {
                break;
            }
            reopened.snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(a.join("unrelated-Q3.gguf")).unwrap(),
        b"keep Q3"
    );
    reopened.handle.request_shutdown().unwrap();
    reopened.join().await.unwrap();
    let log = std::fs::read_to_string(root.join("requests.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(!events.iter().any(|e| {
        e["kind"] == "start"
            && e["path"]
                .as_str()
                .is_some_and(|p| p.contains("org/A/resolve") && p.ends_with("small-complete.bin"))
    }));
    assert!(events.iter().any(|e| {
        e["range"]
            .as_str()
            .is_some_and(|r| r.starts_with("bytes=") && !r.starts_with("bytes=0-"))
    }));
}
#[tokio::test]
async fn different_hash_requires_decision_and_archive_preserves_only_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "0", true);
    let mut session = queue(root).await;
    let dest = root.join("download");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("small-complete.bin"), b"older contents").unwrap();
    std::fs::write(dest.join("unrelated-Q3.gguf"), b"leave this alone").unwrap();
    ready(&mut session, &endpoint, "org/model", "small-complete.bin").await;
    add(&session, &dest).await;
    let plan = session
        .snapshots
        .borrow()
        .queue
        .as_ref()
        .unwrap()
        .conflicts
        .as_ref()
        .unwrap()
        .id;
    assert_eq!(
        std::fs::read(dest.join("small-complete.bin")).unwrap(),
        b"older contents"
    );
    send(&session, CoreCommand::CancelSubmission).await;
    assert!(!dest.join(".hfdx/task.json").exists());
    add(&session, &dest).await;
    let new_plan = session
        .snapshots
        .borrow()
        .queue
        .as_ref()
        .unwrap()
        .conflicts
        .as_ref()
        .unwrap()
        .id;
    assert_ne!(plan, new_plan);
    send(
        &session,
        CoreCommand::ConfirmConflicts {
            plan: new_plan,
            policy: ConflictPolicy::Archive,
        },
    )
    .await;
    tokio::time::timeout(
        Duration::from_secs(15),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        std::fs::metadata(dest.join("small-complete.bin"))
            .unwrap()
            .len(),
        262144
    );
    assert_eq!(
        std::fs::read(dest.join("unrelated-Q3.gguf")).unwrap(),
        b"leave this alone"
    );
    let archive = std::fs::read_dir(dest.join("archive"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read(archive.join("small-complete.bin")).unwrap(),
        b"older contents"
    );
    assert!(!archive.join("unrelated-Q3.gguf").exists());
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[tokio::test]
async fn overwrite_ignores_time_but_newer_policy_keeps_newer_local_unverified() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "0", true);
    for (n, policy, local_time, replace) in [
        (0, ConflictPolicy::Overwrite, 1893456000, true),
        (1, ConflictPolicy::NewerOnly, 1893456000, false),
        (2, ConflictPolicy::NewerOnly, 1577836800, true),
    ] {
        let scope = root.join(format!("case-{n}"));
        std::fs::create_dir(&scope).unwrap();
        let dest = scope.join("download");
        std::fs::create_dir(&dest).unwrap();
        let path = dest.join("small-complete.bin");
        std::fs::write(&path, b"local data").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(local_time)),
            )
            .unwrap();
        let mut session = queue(&scope).await;
        ready(&mut session, &endpoint, "org/model", "small-complete.bin").await;
        add(&session, &dest).await;
        let plan = session
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .conflicts
            .as_ref()
            .unwrap()
            .id;
        send(&session, CoreCommand::ConfirmConflicts { plan, policy }).await;
        tokio::time::timeout(
            Duration::from_secs(12),
            session.snapshots.wait_for(|s| {
                s.task
                    .as_ref()
                    .is_some_and(|t| t.record.state == TaskState::Completed)
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let snap = session.snapshots.borrow();
        let file = &snap.task.as_ref().unwrap().record.files[0];
        if replace {
            assert_eq!(std::fs::metadata(&path).unwrap().len(), 262144);
            assert_eq!(file.verification, VerificationStatus::Passed);
        } else {
            assert_eq!(std::fs::read(&path).unwrap(), b"local data");
            assert_eq!(file.disposition, storage::FileDisposition::KeptDifferent);
            assert_ne!(file.verification, VerificationStatus::Passed);
        }
        drop(snap);
        session.handle.request_shutdown().unwrap();
        session.join().await.unwrap();
    }
}
#[tokio::test]
async fn changed_file_after_conflict_preview_is_never_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "0", true);
    let mut session = queue(root).await;
    let dest = root.join("download");
    std::fs::create_dir(&dest).unwrap();
    let path = dest.join("small-complete.bin");
    std::fs::write(&path, b"original").unwrap();
    ready(&mut session, &endpoint, "org/model", "small-complete.bin").await;
    add(&session, &dest).await;
    let plan = session
        .snapshots
        .borrow()
        .queue
        .as_ref()
        .unwrap()
        .conflicts
        .as_ref()
        .unwrap()
        .id;
    std::fs::write(&path, b"user edited after preview").unwrap();
    let result = session
        .handle
        .submit(CoreCommand::ConfirmConflicts {
            plan,
            policy: ConflictPolicy::Overwrite,
        })
        .unwrap()
        .await
        .unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"user edited after preview");
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[tokio::test]
async fn unavailable_remote_digest_keeps_local_without_a_conflict_prompt_or_transfer() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "0", false);
    let mut session = queue(root).await;
    let dest = root.join("download");
    std::fs::create_dir(&dest).unwrap();
    let path = dest.join("small-complete.bin");
    std::fs::write(&path, b"unverified local data").unwrap();
    ready(&mut session, &endpoint, "org/model", "small-complete.bin").await;
    add(&session, &dest).await;
    let snap = session.snapshots.borrow();
    assert!(snap.queue.as_ref().unwrap().conflicts.is_none());
    let file = &snap.task.as_ref().unwrap().record.files[0];
    assert_eq!(file.disposition, storage::FileDisposition::Unchecked);
    assert_ne!(file.verification, VerificationStatus::Passed);
    assert_eq!(std::fs::read(path).unwrap(), b"unverified local data");
    drop(snap);
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    assert!(
        !std::fs::read_to_string(root.join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .any(|e| e["path"].as_str().is_some_and(|p| p.contains("/resolve/")))
    );
}
#[tokio::test]
async fn queued_preflight_can_be_cancelled_while_the_only_slot_is_downloading() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "1048576", true);
    let mut session = queue(root).await;
    let mut settings = (*session.snapshots.borrow().settings).clone();
    settings.options = DownloadOptions::new(1, 1).unwrap();
    send(&session, CoreCommand::SaveSettings(settings)).await;
    ready(&mut session, &endpoint, "org/A", "large-partial.bin").await;
    add(&session, &root.join("A")).await;
    let other = root.join("B");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("small-complete.bin"), b"local").unwrap();
    ready(&mut session, &endpoint, "org/B", "small-complete.bin").await;
    let catalog = session
        .snapshots
        .borrow()
        .selection
        .catalog
        .as_ref()
        .unwrap()
        .id;
    let submission = session
        .handle
        .submit(CoreCommand::Start(StartRequest {
            catalog,
            destination: other.clone(),
            options: DownloadOptions::new(1, 1).unwrap(),
            verify_after_download: true,
        }))
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        session
            .snapshots
            .wait_for(|s| s.queue.as_ref().unwrap().preparing),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        send(&session, CoreCommand::CancelSubmission).await;
        assert!(submission.await.unwrap().is_err());
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(other.join("small-complete.bin")).unwrap(),
        b"local"
    );
    assert_eq!(
        session
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .tasks
            .len(),
        1
    );
    assert_eq!(
        session.snapshots.borrow().queue.as_ref().unwrap().tasks[0]
            .view
            .record
            .state,
        TaskState::Running
    );
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[tokio::test]
async fn completed_directory_can_accept_a_new_commit_without_touching_other_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "0", true);
    let mut session = queue(root).await;
    let dest = root.join("download");
    ready(&mut session, &endpoint, "org/model", "small-complete.bin").await;
    add(&session, &dest).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let old = session
        .snapshots
        .borrow()
        .task
        .as_ref()
        .unwrap()
        .record
        .task_id
        .clone();
    let before = std::fs::read(dest.join("small-complete.bin")).unwrap();
    reqwest::Client::new()
        .post(format!("{}__control", endpoint.url()))
        .json(&serde_json::json!({"branch":"b"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    ready(&mut session, &endpoint, "org/model", "empty.bin").await;
    assert_eq!(
        session
            .snapshots
            .borrow()
            .selection
            .catalog
            .as_ref()
            .unwrap()
            .source
            .resolved_commit
            .as_str(),
        "b".repeat(40),
        "reload must expose the new catalog before edits"
    );
    add(&session, &dest).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed && t.record.task_id != old)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let snap = session.snapshots.borrow();
    assert_eq!(snap.queue.as_ref().unwrap().tasks.len(), 1);
    assert_eq!(snap.task.as_ref().unwrap().record.schema_version, 2);
    assert_eq!(
        snap.task
            .as_ref()
            .unwrap()
            .record
            .source
            .resolved_commit
            .as_str(),
        "b".repeat(40)
    );
    drop(snap);
    assert_eq!(
        std::fs::read(dest.join("small-complete.bin")).unwrap(),
        before
    );
    assert!(
        std::fs::read_dir(dest.join(".hfdx/history"))
            .unwrap()
            .next()
            .is_some()
    );
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[tokio::test]
async fn changed_local_file_during_replacement_is_preserved_and_failure_is_visible() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "2097152", true);
    let mut session = queue(root).await;
    let dest = root.join("download");
    std::fs::create_dir(&dest).unwrap();
    let local = dest.join("large-partial.bin");
    std::fs::write(&local, b"old model").unwrap();
    ready(&mut session, &endpoint, "org/model", "large-partial.bin").await;
    add(&session, &dest).await;
    let plan = session
        .snapshots
        .borrow()
        .queue
        .as_ref()
        .unwrap()
        .conflicts
        .as_ref()
        .unwrap()
        .id;
    send(
        &session,
        CoreCommand::ConfirmConflicts {
            plan,
            policy: ConflictPolicy::Archive,
        },
    )
    .await;
    tokio::time::timeout(
        Duration::from_secs(10),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.progress.acquired_bytes.get() > 0)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    std::fs::write(&local, b"user edited during download").unwrap();
    tokio::time::timeout(
        Duration::from_secs(30),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.files.iter().any(|f| f.failure.is_some()))
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        std::fs::read(&local).unwrap(),
        b"user edited during download"
    );
    assert!(!dest.join("archive").exists());
    let snap = session.snapshots.borrow();
    let record = &snap.task.as_ref().unwrap().record;
    let staged = dest
        .join(".hfdx/incoming")
        .join(record.task_id.as_str())
        .join("large-partial.bin");
    assert_eq!(std::fs::metadata(staged).unwrap().len(), 33554432);
    assert!(record.replacements["large-partial.bin"].ready.is_some());
    drop(snap);
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[tokio::test]
async fn newer_policy_preserves_local_when_mirror_has_no_file_time() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "0", true);
    reqwest::Client::new()
        .post(format!("{}__control", endpoint.url()))
        .json(&serde_json::json!({"tree_mode":"no-time"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let mut session = queue(root).await;
    let dest = root.join("download");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("small-complete.bin"), b"keep local").unwrap();
    ready(&mut session, &endpoint, "org/model", "small-complete.bin").await;
    add(&session, &dest).await;
    let plan = session
        .snapshots
        .borrow()
        .queue
        .as_ref()
        .unwrap()
        .conflicts
        .as_ref()
        .unwrap()
        .id;
    send(
        &session,
        CoreCommand::ConfirmConflicts {
            plan,
            policy: ConflictPolicy::NewerOnly,
        },
    )
    .await;
    tokio::time::timeout(
        Duration::from_secs(10),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        session
            .snapshots
            .borrow()
            .task
            .as_ref()
            .unwrap()
            .record
            .files[0]
            .disposition,
        hfdx_core::storage::FileDisposition::TimeUnknown
    );
    assert_eq!(
        std::fs::read(dest.join("small-complete.bin")).unwrap(),
        b"keep local"
    );
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[tokio::test]
async fn appended_files_wait_behind_earlier_repository_and_survive_idle_engine_release() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "8388608", true);
    let mut session = queue(root).await;
    let mut settings = (*session.snapshots.borrow().settings).clone();
    settings.options = DownloadOptions::new(1, 1).unwrap();
    send(&session, CoreCommand::SaveSettings(settings)).await;
    ready(&mut session, &endpoint, "org/A", "large-partial.bin").await;
    add(&session, &root.join("A")).await;
    ready(&mut session, &endpoint, "org/B", "large-partial.bin").await;
    add(&session, &root.join("B")).await;
    ready(&mut session, &endpoint, "org/A", "small-complete.bin").await;
    add(&session, &root.join("A")).await;
    let waited = tokio::time::timeout(
        Duration::from_secs(45),
        session.snapshots.wait_for(|s| {
            let q = s.queue.as_ref().unwrap();
            q.tasks.len() == 2
                && q.tasks
                    .iter()
                    .all(|t| t.view.record.state == TaskState::Completed)
        }),
    )
    .await;
    let stalled = waited.is_err();
    drop(waited);
    if stalled {
        let s = session.snapshots.borrow();
        eprintln!(
            "slots {:?}",
            s.queue.as_ref().map(|q| (q.used_slots, q.slot_limit))
        );
        for task in s.queue.as_ref().unwrap().tasks.iter() {
            eprintln!(
                "{} {:?} {:?} {:?}",
                task.view.record.source.request.repo_id.as_str(),
                task.phase,
                task.error,
                task.view
                    .record
                    .files
                    .iter()
                    .map(|f| (
                        f.entry.path.as_str(),
                        f.completed,
                        f.verification,
                        f.failure.as_ref()
                    ))
                    .collect::<Vec<_>>()
            );
        }
        drop(s);
        session.handle.request_shutdown().unwrap();
        session.join().await.unwrap();
        panic!("queue stalled");
    }
    let wire = std::fs::read_to_string(root.join("requests.jsonl")).unwrap();
    let bodies: Vec<_> = wire
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|e| {
            e["kind"] == "start"
                && e["method"] == "GET"
                && e["path"].as_str().is_some_and(|p| p.contains("/resolve/"))
        })
        .collect();
    let b = bodies
        .iter()
        .position(|e| e["path"].as_str().unwrap().starts_with("/org/B/"))
        .unwrap();
    let append = bodies
        .iter()
        .position(|e| e["path"].as_str().unwrap().ends_with("/small-complete.bin"))
        .unwrap();
    assert!(
        b < append,
        "appended A must wait behind earlier B: {bodies:?}"
    );
    assert_eq!(
        std::fs::metadata(root.join("A/small-complete.bin"))
            .unwrap()
            .len(),
        262144
    );
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn windows_filesystem_casing_merges_existing_and_new_destination_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "0", true);
    let mut session = queue(root).await;
    let dest = root.join("MixedCase").join("NewFolder");
    ready(&mut session, &endpoint, "org/model", "small-complete.bin").await;
    add(&session, &dest).await;
    tokio::time::timeout(
        Duration::from_secs(15),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let id = session
        .snapshots
        .borrow()
        .task
        .as_ref()
        .unwrap()
        .record
        .task_id
        .clone();
    ready(&mut session, &endpoint, "org/model", "small-complete.bin").await;
    add(
        &session,
        &std::path::PathBuf::from(dest.to_str().unwrap().to_uppercase()),
    )
    .await;
    assert_eq!(
        session
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .tasks
            .len(),
        1
    );
    assert_eq!(
        session
            .snapshots
            .borrow()
            .task
            .as_ref()
            .unwrap()
            .record
            .task_id,
        id
    );
    assert_eq!(
        session
            .snapshots
            .borrow()
            .queue
            .as_ref()
            .unwrap()
            .last_submission
            .as_ref()
            .unwrap()
            .identical,
        1
    );
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[tokio::test]
async fn another_repository_folder_has_an_owner_error_and_a_separate_draft_suggestion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (_fixture, endpoint) = fixture(root, "0", true);
    let mut session = queue(root).await;
    let destination = root.join("A");
    ready(&mut session, &endpoint, "org/A", "empty.bin").await;
    add(&session, &destination).await;
    let completed = tokio::time::timeout(
        Duration::from_secs(10),
        session.snapshots.wait_for(|s| {
            s.task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
        }),
    )
    .await
    .is_ok_and(|result| result.is_ok());
    if !completed {
        {
            let snapshot = session.snapshots.borrow();
            eprintln!(
                "initial owner phase={:?} error={:?} record={:?}",
                snapshot.phase,
                snapshot.error,
                snapshot.task.as_ref().map(|t| t.record.clone())
            );
        }
        session.handle.request_shutdown().unwrap();
        session.join().await.unwrap();
        panic!("initial owner task did not complete");
    }
    let before = std::fs::read(destination.join(".hfdx/task.json")).unwrap();
    ready(&mut session, &endpoint, "org/B", "small-complete.bin").await;
    let (catalog, request) = {
        let s = session.snapshots.borrow();
        let c = s.selection.catalog.as_ref().unwrap();
        (c.id, c.source.request.clone())
    };
    let error = session
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
        .err()
        .expect("foreign repository folder must be rejected")
        .ui_error();
    assert!(matches!(
        error.detail,
        Some(task::ErrorDetail::Destination {
            same_repository: false,
            ..
        })
    ));
    assert!(locale::error(Language::ZhHans, &error).contains("org/A"));
    let suggestion =
        task::draft_destination(&session.snapshots.borrow(), &request, &destination).unwrap();
    assert_eq!(suggestion, root.join("B"));
    assert_eq!(
        std::fs::read(destination.join(".hfdx/task.json")).unwrap(),
        before
    );
    assert!(!destination.join("small-complete.bin").exists());
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}
