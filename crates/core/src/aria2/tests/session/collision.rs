use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Deserialize, Serialize)]
struct Attempt {
    attempt: usize,
    pid: u32,
    port: u16,
    blocked: bool,
    config: PathBuf,
    old_children_absent: bool,
}

#[tokio::test]
async fn port_collision_retries_real_engine_and_leaves_unrelated_listener_untouched() {
    collision(1).await;
}

#[tokio::test]
async fn port_collision_exhausts_exactly_three_real_launches_and_reaps_them() {
    collision(3).await;
}

async fn collision(conflicts: usize) {
    let root = tempfile::Builder::new()
        .prefix("engine-collision-")
        .tempdir()
        .unwrap();
    let wrapper = root.path().join("aria2-wrapper");
    tokio::fs::write(&wrapper, include_str!("../collision_wrapper.py"))
        .await
        .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    tokio::fs::write(root.path().join("conflicts"), conflicts.to_string())
        .await
        .unwrap();
    let unrelated = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let unrelated_address = unrelated.local_addr().unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        Aria2::launch(Aria2Launch {
            executable: wrapper,
        }),
    )
    .await
    .expect("bounded native collision retries");
    let expected = if conflicts == 1 {
        let engine = result.expect("second real launch authenticates");
        assert_eq!(engine.version, "1.37.0");
        let receipt = engine.shutdown().await.unwrap();
        assert_eq!(receipt.stop, StopKind::Graceful);
        assert_eq!(receipt.child_exit_code, Some(0));
        2
    } else {
        assert!(
            matches!(result, Err(aria2::ProbeError::Protocol)),
            "three conflicts must fail closed"
        );
        3
    };
    let journal = tokio::fs::read_to_string(root.path().join("attempts.jsonl"))
        .await
        .unwrap();
    let attempts: Vec<Attempt> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(attempts.len(), expected);
    for (index, attempt) in attempts.iter().enumerate() {
        assert_eq!(attempt.attempt, index + 1);
        assert_eq!(attempt.blocked, index < conflicts);
        assert!(attempt.old_children_absent);
        assert!(!Path::new(&format!("/proc/{}", attempt.pid)).exists());
        assert!(!attempt.config.exists());
        assert!(!attempt.config.parent().unwrap().exists());
    }
    let (connected, accepted) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            tokio::net::TcpStream::connect(unrelated_address),
            unrelated.accept()
        )
    })
    .await
    .unwrap();
    connected.unwrap();
    accepted.unwrap();
    let proof = json!({"conflicts":conflicts,"attempts":attempts,"expected_attempts":expected,
        "real_executable":"/usr/bin/aria2c","unrelated_listener":unrelated_address.to_string(),
        "unrelated_listener_accepts_after":true,"all_owned_pids_absent":true,"all_private_directories_removed":true});
    println!("port_collision_proof={proof}");
    if let Some(directory) = std::env::var_os("HFDX_ENGINE_EVIDENCE_DIR") {
        tokio::fs::write(
            PathBuf::from(directory).join(format!("m2-engine-a08-{conflicts}.json")),
            serde_json::to_vec_pretty(&proof).unwrap(),
        )
        .await
        .unwrap();
    }
    drop(unrelated);
    root.close().unwrap();
}
