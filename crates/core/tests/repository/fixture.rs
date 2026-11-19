use crate::repository::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

pub struct Fixture {
    child: Child,
    done: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    directory: Option<TempDir>,
    pub ready: Value,
    pub http: reqwest::Client,
    stopped: bool,
}
impl Fixture {
    pub fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut child = Command::new("python")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/qa/fixture.py"
            ))
            .args(["--port", "0", "--rate", "0", "--ready-file"])
            .arg(directory.path().join("ready.json"))
            .arg("--log")
            .arg(directory.path().join("events.jsonl"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (done_tx, done) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            ready_tx.send(first).unwrap();
            let mut rest = String::new();
            reader.read_to_string(&mut rest).unwrap();
            if let Err(error) = done_tx.send(rest) {
                eprintln!("fixture receipt receiver closed: {error}");
            }
        });
        let line = match ready_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(line) => line,
            Err(error) => {
                child.kill().unwrap();
                child.wait().unwrap();
                reader.join().unwrap();
                panic!("fixture readiness failed: {error}");
            }
        };
        Self {
            child,
            done,
            reader: Some(reader),
            directory: Some(directory),
            ready: serde_json::from_str(&line).unwrap(),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            stopped: false,
        }
    }
    pub fn origin(&self) -> &str {
        self.ready["origin"].as_str().unwrap()
    }
    pub fn request(&self) -> RepositoryRequest {
        RepositoryRequest {
            endpoint: Endpoint::parse(self.origin(), true).unwrap(),
            kind: RepoKind::Model,
            repo_id: RepoId::parse("org/model").unwrap(),
            requested_revision: Revision::parse("main").unwrap(),
        }
    }
    pub fn client(&self) -> RepositoryClient {
        RepositoryClient::new(&self.request().endpoint, NetworkAccess::default()).unwrap()
    }
    pub async fn source(&self) -> RepositorySpec {
        self.client()
            .resolve(&self.request(), &CancellationToken::new())
            .await
            .unwrap()
    }
    pub async fn state(&self) -> Value {
        self.http
            .get(format!("{}/__state", self.origin()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    pub async fn stop(mut self) {
        let response = self
            .http
            .post(format!("{}/__shutdown", self.origin()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
        let receipt = self.done.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(self.child.wait().unwrap().code(), Some(0));
        self.stopped = true;
        self.reader.take().unwrap().join().unwrap();
        let parsed: Value = serde_json::from_str(receipt.trim()).unwrap();
        assert_eq!(
            parsed,
            json!({"cleanup":"listeners closed; request threads joined"})
        );
        println!("fixture pid={} exit=0 receipt={parsed}", self.child.id());
        self.directory.take().unwrap().close().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.stopped {
            if let Err(error) = self.child.kill() {
                eprintln!("fixture emergency kill: {error}");
            }
            match self.child.wait() {
                Ok(status) => eprintln!("fixture emergency wait: {status}"),
                Err(error) => eprintln!("fixture emergency wait failed: {error}"),
            }
            if let Some(reader) = self.reader.take()
                && let Err(error) = reader.join()
            {
                eprintln!("fixture reader failed: {error:?}");
            }
        }
    }
}
