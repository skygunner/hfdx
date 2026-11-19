use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    time::timeout,
};

#[derive(Deserialize)]
pub struct FileInfo {
    pub size: u64,
    pub sha256: String,
}
#[derive(Deserialize)]
pub struct Ready {
    pub origin: String,
    pub fixtures: BTreeMap<String, FileInfo>,
}
pub struct Fixture {
    pub ready: Ready,
    pub child: Child,
    client: reqwest::Client,
    output: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}
impl Fixture {
    pub async fn start(root: &Path) -> Self {
        let mut child = Command::new("python3")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/qa/fixture.py"))
            .args(["--port", "0", "--rate", "262144"])
            .arg("--ready-file")
            .arg(root.join("ready.json"))
            .arg("--log")
            .arg(root.join("requests.jsonl"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("real fixture process");
        let stdout = child.stdout.take().expect("fixture stdout");
        let mut lines = BufReader::new(stdout).lines();
        let line = timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("fixture ready deadline")
            .expect("fixture stdout read")
            .expect("ready event");
        let ready = serde_json::from_str(&line).expect("ready JSON");
        Self {
            ready,
            child,
            output: lines,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("fixture client"),
        }
    }
    pub async fn events(&self, after: u64) -> Vec<Value> {
        self.state(after, None).await
    }
    async fn state(&self, after: u64, filter: Option<(&str, &str)>) -> Vec<Value> {
        let mut query = vec![("after", after.to_string()), ("timeout", "10".into())];
        if let Some((key, value)) = filter {
            query.push((key, value.into()));
        }
        let response: Value = self
            .client
            .get(format!("{}/__state", self.ready.origin))
            .query(&query)
            .send()
            .await
            .expect("fixture state request")
            .error_for_status()
            .expect("state status")
            .json()
            .await
            .expect("state JSON");
        response["events"].as_array().expect("event array").clone()
    }
    pub async fn wait_bytes(&self, path: &str, after: u64, bytes: u64) -> Vec<Value> {
        timeout(Duration::from_secs(30), async {
            let mut cursor = after;
            loop {
                let events = self.state(cursor, Some(("path", path))).await;
                if events.iter().any(|e| {
                    e["kind"] == "progress" && e["bytes"].as_u64().is_some_and(|n| n >= bytes)
                }) {
                    return events;
                }
                if let Some(seq) = events.last().and_then(|e| e["seq"].as_u64()) {
                    cursor = seq;
                }
            }
        })
        .await
        .expect("transfer progress event deadline")
    }
    pub async fn wait_interrupted(&self, path: &str, after: u64) {
        timeout(Duration::from_secs(10), async {
            let mut cursor = after;
            loop {
                let events = self.state(cursor, Some(("path", path))).await;
                if events
                    .iter()
                    .any(|e| e["kind"] == "complete" && e["interrupted"] == true)
                {
                    return;
                }
                if let Some(seq) = events.last().and_then(|e| e["seq"].as_u64()) {
                    cursor = seq;
                }
            }
        })
        .await
        .expect("paused connection termination event");
    }
    pub async fn unlimit(&self) {
        self.client
            .post(format!("{}/__control", self.ready.origin))
            .json(&json!({"rate":0}))
            .send()
            .await
            .expect("fixture control")
            .error_for_status()
            .expect("control status");
    }
    pub async fn close(mut self) -> u32 {
        let pid = self.child.id().expect("fixture PID");
        let signal = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await
            .expect("fixture TERM");
        assert!(signal.success());
        let cleanup = timeout(Duration::from_secs(10), self.output.next_line())
            .await
            .expect("fixture cleanup event deadline")
            .expect("fixture cleanup line")
            .expect("fixture cleanup event");
        let cleanup: Value = serde_json::from_str(&cleanup).expect("cleanup JSON");
        assert_eq!(
            cleanup["cleanup"],
            "listeners closed; request threads joined"
        );
        assert!(
            timeout(Duration::from_secs(10), self.child.wait())
                .await
                .expect("fixture wait deadline")
                .expect("fixture wait")
                .success()
        );
        pid
    }
}
pub fn body_count(events: &[Value], name: &str) -> usize {
    events
        .iter()
        .filter(|e| {
            e["method"] == "GET"
                && e["kind"] == "start"
                && e["path"].as_str().is_some_and(|p| p.ends_with(name))
        })
        .count()
}
pub async fn hash(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .await
        .expect("independent SHA-256 tool");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("sha256 ASCII")
        .split_whitespace()
        .next()
        .expect("digest")
        .into()
}
