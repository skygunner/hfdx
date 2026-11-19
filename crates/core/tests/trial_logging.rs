// allow: SIZE_OK — public logging contracts share the sole authorized integration test file.
use hfdx_core::{
    TaskId,
    logging::{
        EventKind, LogConfig, LogError, LogEvent, LogLevel, LogOutcome, LogStatus, SessionLogger,
    },
};
use serde_json::Value;
use std::{
    fs,
    num::NonZeroU64,
    path::{Path, PathBuf},
    process::Command,
};

fn event(outcome: LogOutcome) -> LogEvent<'static> {
    LogEvent {
        event: EventKind::Verification,
        task_id: None,
        outcome,
        count: 7,
        bytes: 8_765,
        elapsed_ms: 99,
    }
}
fn sessions(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect()
}
fn logs(root: &Path) -> Vec<PathBuf> {
    sessions(root)
        .into_iter()
        .flat_map(|session| {
            fs::read_dir(session)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>()
        })
        .filter(|path| path.extension().is_some_and(|extension| extension == "log"))
        .collect()
}
fn records(root: &Path) -> Vec<Value> {
    logs(root)
        .iter()
        .flat_map(|file| {
            fs::read_to_string(file)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<Value>>()
        })
        .collect()
}
fn limited(root: &Path, file: u64, total: u64) -> LogConfig {
    LogConfig::new(root.to_owned()).with_limits(
        NonZeroU64::new(file).unwrap(),
        NonZeroU64::new(total).unwrap(),
    )
}

#[test]
fn clean_defaults_and_live_debug_toggle_control_persisted_records() {
    let root = tempfile::tempdir().unwrap();
    let logger = SessionLogger::new(LogConfig::new(root.path().into())).unwrap();
    logger
        .record(LogLevel::Debug, event(LogOutcome::Disabled))
        .unwrap();
    logger.set_debug(true);
    logger
        .record(LogLevel::Debug, event(LogOutcome::Sha256))
        .unwrap();
    logger.set_debug(false);
    logger
        .record(LogLevel::Debug, event(LogOutcome::GitSha1))
        .unwrap();
    for level in [LogLevel::Info, LogLevel::Warn, LogLevel::Error] {
        logger.record(level, event(LogOutcome::Succeeded)).unwrap();
    }
    logger.flush().unwrap();
    let values = records(root.path());
    let debug: Vec<_> = values
        .iter()
        .filter(|value| value["level"] == "DEBUG")
        .collect();
    assert_eq!(debug.len(), 1);
    assert_eq!(debug[0]["event"]["outcome"], "sha256");
    let levels: Vec<_> = values
        .iter()
        .map(|value| value["level"].as_str().unwrap())
        .collect();
    assert_eq!(levels, ["INFO", "DEBUG", "INFO", "WARN", "ERROR"]);
    assert!(
        values
            .iter()
            .all(|value| value["time"].as_str().unwrap().ends_with("GMT"))
    );
    drop(logger);
    let restarted =
        SessionLogger::new(LogConfig::new(root.path().into()).with_debug(true)).unwrap();
    restarted
        .record(LogLevel::Debug, event(LogOutcome::GitSha1))
        .unwrap();
    restarted.flush().unwrap();
    assert_eq!(
        records(root.path())
            .iter()
            .filter(|value| value["level"] == "DEBUG")
            .count(),
        2
    );
}

#[test]
fn rotation_bounds_every_file_and_retains_latest_events() {
    let root = tempfile::tempdir().unwrap();
    let logger = SessionLogger::new(limited(root.path(), 1024, 100_000)).unwrap();
    for count in 0..100 {
        logger
            .record(
                LogLevel::Info,
                LogEvent {
                    count,
                    ..event(LogOutcome::Succeeded)
                },
            )
            .unwrap();
    }
    logger.flush().unwrap();
    let files = logs(root.path());
    assert_eq!(files.len(), 5);
    assert!(
        files
            .iter()
            .all(|file| fs::metadata(file).unwrap().len() <= 1024)
    );
    let current = files
        .iter()
        .find(|path| path.file_name().unwrap() == "events_rCURRENT.log")
        .unwrap();
    let last: Value =
        serde_json::from_str(fs::read_to_string(current).unwrap().lines().last().unwrap()).unwrap();
    assert_eq!(last["event"]["count"], 99);
}

#[test]
fn retention_preserves_active_sessions_and_unknown_user_files() {
    let root = tempfile::tempdir().unwrap();
    let first = SessionLogger::new(limited(root.path(), 1024, 1)).unwrap();
    let first_dir = sessions(root.path()).pop().unwrap();
    let second = SessionLogger::new(limited(root.path(), 1024, 1)).unwrap();
    let second_dir = sessions(root.path())
        .into_iter()
        .find(|path| path != &first_dir)
        .unwrap();
    let foreign = root.path().join(format!("hfdx-session-{}", "a".repeat(32)));
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("keep.txt"), b"user file").unwrap();
    first.flush().unwrap();
    assert_eq!(first.status(), LogStatus::BudgetExceeded);
    assert_eq!(second.status(), LogStatus::BudgetExceeded);
    assert!(first_dir.is_dir() && second_dir.is_dir());
    drop(first);
    second.flush().unwrap();
    assert!(!first_dir.exists());
    assert!(second_dir.is_dir());
    assert_eq!(fs::read(foreign.join("keep.txt")).unwrap(), b"user file");
    second
        .record(LogLevel::Info, event(LogOutcome::Succeeded))
        .unwrap();
    assert!(records(root.path()).iter().all(|value| {
        value["session"].as_str().unwrap()
            == second_dir
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .strip_prefix("hfdx-session-")
                .unwrap()
    }));
}

#[test]
fn concurrent_independent_sessions_never_overwrite_or_delete_each_other() {
    let root = tempfile::tempdir().unwrap();
    let first = SessionLogger::new(limited(root.path(), 1024, 100_000)).unwrap();
    let second = SessionLogger::new(limited(root.path(), 1024, 100_000)).unwrap();
    let start = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for (logger, count) in [(&first, 11), (&second, 22)] {
            let start = &start;
            scope.spawn(move || {
                start.wait();
                for _ in 0..400 {
                    logger
                        .record(
                            LogLevel::Warn,
                            LogEvent {
                                count,
                                ..event(LogOutcome::Network)
                            },
                        )
                        .unwrap();
                    logger.flush().unwrap();
                }
                logger.flush().unwrap();
            });
        }
    });
    assert_eq!(sessions(root.path()).len(), 2);
    for session in sessions(root.path()) {
        let mut counts = Vec::new();
        for file in fs::read_dir(&session).unwrap() {
            let file = file.unwrap().path();
            if file.extension().is_some_and(|extension| extension == "log") {
                for line in fs::read_to_string(file).unwrap().lines() {
                    let value: Value = serde_json::from_str(line).unwrap();
                    counts.push(value["event"]["count"].as_u64().unwrap());
                }
            }
        }
        counts.sort_unstable();
        counts.dedup();
        assert!(counts == [11] || counts == [22]);
    }
}

#[test]
fn closed_field_boundary_excludes_secret_paths_and_invalid_identifiers() {
    let root = tempfile::tempdir().unwrap();
    let secrets = [
        "hf_fake_token",
        "proxy-password",
        "rpc-secret",
        "Cookie=secret",
        "Authorization=Bearer_fake",
        "https://example.test/?signature=private",
        "raw-body-private",
    ];
    let directory = root.path().join(secrets[..3].join(" ")).join("中文 log");
    let logger = SessionLogger::new(LogConfig::new(directory.clone()).with_debug(true)).unwrap();
    let task: TaskId = serde_json::from_str(&format!("\"{}\"", "b".repeat(32))).unwrap();
    for secret in secrets {
        assert!(serde_json::from_value::<TaskId>(Value::String(secret.into())).is_err());
    }
    for level in [
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
        LogLevel::Debug,
    ] {
        logger
            .record(
                level,
                LogEvent {
                    task_id: Some(&task),
                    ..event(LogOutcome::IntegrityFailed)
                },
            )
            .unwrap();
    }
    logger.flush().unwrap();
    let output: String = logs(&directory)
        .iter()
        .map(|file| fs::read_to_string(file).unwrap())
        .collect();
    for secret in secrets {
        assert!(!output.contains(secret));
    }
    assert!(!output.contains(&directory.to_string_lossy().into_owned()));
    assert_eq!(
        records(&directory)
            .iter()
            .filter(|value| value["event"]["task_id"] == task.as_str())
            .count(),
        4
    );
}

#[test]
fn initialization_and_rotation_failures_are_safe_and_sticky() {
    const CHILD: &str = "HFDX_LOGGING_ROTATION_FAILURE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "initialization_and_rotation_failures_are_safe_and_sticky",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("private-path-token");
    fs::write(&blocked, b"file").unwrap();
    let error = SessionLogger::new(LogConfig::new(blocked)).err().unwrap();
    assert!(!error.to_string().contains("private-path-token"));
    let logger = SessionLogger::new(limited(root.path(), 1024, 100_000)).unwrap();
    let session = sessions(root.path()).pop().unwrap();
    fs::create_dir(session.join("events_r00000.log")).unwrap();
    let failure = (0..20)
        .find_map(|_| {
            logger
                .record(LogLevel::Info, event(LogOutcome::Succeeded))
                .err()
        })
        .unwrap();
    assert_eq!(failure, LogError::Rotation);
    assert_eq!(logger.status(), LogStatus::Failed(failure));
    assert_eq!(
        logger.record(LogLevel::Error, event(LogOutcome::Internal)),
        Err(failure)
    );
    assert_eq!(logger.flush(), Err(failure));
    assert!(
        fs::metadata(session.join("events_rCURRENT.log"))
            .unwrap()
            .len()
            <= 1024
    );
}

#[tokio::test]
async fn filter_events_record_counts_without_input_content() {
    use hfdx_core::{
        storage::SettingsPaths,
        task::{self, CoreCommand, CoreConfig},
    };
    use std::sync::Arc;

    let root = tempfile::tempdir().unwrap();
    let log_root = root.path().join("logs");
    let logger = Arc::new(SessionLogger::new(LogConfig::new(log_root.clone())).unwrap());
    let session = task::spawn_with_services(
        CoreConfig {
            paths: SettingsPaths {
                settings_file: root.path().join("settings.json"),
            },
            aria2_executable: "aria2c".into(),
        },
        Arc::new(|_| {}),
        Some(logger.clone()),
    )
    .await
    .unwrap();
    let mut settings = (*session.snapshots.borrow().settings).clone();
    settings.debug_logging = true;
    session
        .handle
        .submit(CoreCommand::SaveSettings(settings))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session
        .handle
        .submit(CoreCommand::SetFilter("secret-filter".into()))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session
        .handle
        .submit(CoreCommand::SetSelectedOnly(true))
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    logger.flush().unwrap();
    let values = records(&log_root);
    let filtered: Vec<_> = values
        .iter()
        .filter(|v| v["event"]["event"] == "filter")
        .collect();
    assert_eq!(filtered.len(), 2);
    for record in filtered {
        assert_eq!(record["metrics"]["input_bytes"], 13);
        assert_eq!(record["metrics"]["matched"], 0);
        assert_eq!(record["metrics"]["visible"], 0);
        assert_eq!(record["metrics"]["selected"], 0);
        assert!(!record.to_string().contains("secret-filter"));
    }
}

#[cfg(not(target_os = "windows"))]
#[test]
fn platform_default_path_uses_isolated_user_state() {
    const CHILD: &str = "HFDX_LOGGING_PATH_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        let expected = PathBuf::from(root).join("hfdx/logs");
        let actual = hfdx_core::logging::default_log_directory().unwrap();
        assert_eq!(actual, expected);
        let logger = SessionLogger::new(LogConfig::new(actual)).unwrap();
        logger.flush().unwrap();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "platform_default_path_uses_isolated_user_state"])
        .env(CHILD, root.path())
        .env("XDG_STATE_HOME", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
}

#[cfg(target_os = "windows")]
#[test]
fn platform_default_path_uses_windows_known_folder() {
    let expected = dirs::data_local_dir().unwrap().join("hfdx/logs");
    assert_eq!(
        hfdx_core::logging::default_log_directory().unwrap(),
        expected
    );
}

#[cfg(target_os = "linux")]
#[test]
fn actual_disk_write_failure_is_reported_without_terminal_output() {
    const CHILD: &str = "HFDX_LOGGING_WRITE_FAILURE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let root = tempfile::tempdir().unwrap();
        let logger = SessionLogger::new(LogConfig::new(root.path().into())).unwrap();
        let failure = (0..20)
            .find_map(|_| {
                logger
                    .record(LogLevel::Error, event(LogOutcome::Disk))
                    .err()
            })
            .unwrap();
        assert!(matches!(failure, LogError::Io(_)));
        assert_eq!(logger.status(), LogStatus::Failed(failure));
        return;
    }
    let output = Command::new("bash")
        .args(["-c", "trap '' XFSZ; ulimit -f 1; exec \"$1\" --exact actual_disk_write_failure_is_reported_without_terminal_output", "logging-test"])
        .arg(std::env::current_exe().unwrap()).env(CHILD, "1").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
