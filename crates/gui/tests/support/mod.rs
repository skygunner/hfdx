use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::JoinHandle,
    time::Duration,
};
use tempfile::TempDir;

pub struct Environment {
    fixture: Option<Fixture>,
    _directory: TempDir,
    pub origin: String,
    pub root: PathBuf,
    pub evidence: PathBuf,
}

impl Environment {
    pub fn start(count: Option<usize>, hashes: bool) -> Self {
        let directory = tempfile::tempdir().expect("unique GUI test directory");
        let path = |name, fallback| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .unwrap_or_else(|| directory.path().join(fallback))
        };
        let root = path("HFDX_GUI_TEST_ROOT", "root");
        let evidence = path("HFDX_GUI_EVIDENCE", "evidence");
        std::fs::create_dir_all(&root).expect("test root");
        std::fs::create_dir_all(&evidence).expect("evidence directory");
        let (fixture, origin) = match std::env::var("HFDX_GUI_FIXTURE") {
            Ok(origin) => (None, origin),
            Err(std::env::VarError::NotPresent) => {
                let (fixture, origin) = Fixture::start(&directory, hashes);
                if let Some(count) = count {
                    configure_catalog(&origin, count);
                }
                (Some(fixture), origin)
            }
            Err(error) => panic!("fixture environment: {error}"),
        };
        println!(
            "GUI fixture origin={origin} root={} evidence={} catalog={}",
            root.display(),
            evidence.display(),
            count.unwrap_or(4)
        );
        Self {
            fixture,
            _directory: directory,
            origin,
            root,
            evidence,
        }
    }

    pub fn finish(mut self) {
        if let Some(mut fixture) = self.fixture.take() {
            let receipt = fixture.stop().expect("fixture clean shutdown");
            assert_eq!(
                serde_json::from_str::<Value>(receipt.trim()).expect("cleanup JSON"),
                json!({"cleanup": "listeners closed; request threads joined"})
            );
            println!("fixture teardown verified: {}", receipt.trim());
        }
    }
}

struct Fixture {
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    done: mpsc::Receiver<std::io::Result<String>>,
}

impl Fixture {
    fn start(directory: &TempDir, hashes: bool) -> (Self, String) {
        let rate = std::env::var("HFDX_GUI_FIXTURE_RATE").unwrap_or_else(|_| "8388608".into());
        let mut child = Command::new("python")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/qa/fixture.py"
            ))
            .args(
                if hashes || std::env::var_os("HFDX_GUI_FIXTURE_HASHES").is_some() {
                    vec!["--hashes", "--port", "0", "--rate", &rate, "--ready-file"]
                } else {
                    vec!["--port", "0", "--rate", &rate, "--ready-file"]
                },
            )
            .arg(directory.path().join("ready.json"))
            .arg("--log")
            .arg(directory.path().join("fixture.jsonl"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start Python wire fixture");
        let stdout = child.stdout.take().expect("fixture stdout");
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (done_tx, done) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut first = String::new();
            let ready = reader.read_line(&mut first).map(|_| first);
            if let Err(error) = ready_tx.send(ready) {
                eprintln!("fixture readiness receiver closed: {error}");
            }
            let mut rest = String::new();
            let result = reader.read_to_string(&mut rest).map(|_| rest);
            if let Err(error) = done_tx.send(result) {
                eprintln!("fixture cleanup receiver closed: {error}");
            }
        });
        let fixture = Self {
            child: Some(child),
            reader: Some(reader),
            done,
        };
        let line = ready_rx
            .recv_timeout(Duration::from_secs(15))
            .expect("fixture stdout ready deadline")
            .expect("fixture ready read");
        let ready: Value = serde_json::from_str(&line).expect("fixture ready JSON");
        assert_eq!(ready["catalog_size"], 4);
        let origin = ready["origin"].as_str().expect("fixture origin").to_owned();
        println!(
            "fixture pid={} ready={line}",
            fixture.child.as_ref().expect("running child").id()
        );
        (fixture, origin)
    }

    fn stop(&mut self) -> Result<String, String> {
        let Some(mut child) = self.child.take() else {
            return Ok(String::new());
        };
        let signal = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        let result = match signal {
            Ok(status) if status.success() => self
                .done
                .recv_timeout(Duration::from_secs(10))
                .map_err(|error| format!("fixture exit deadline: {error}"))
                .and_then(|result| result.map_err(|error| format!("fixture stdout: {error}"))),
            other => Err(format!("fixture SIGTERM failed: {other:?}")),
        };
        if result.is_err()
            && let Err(error) = child.kill()
        {
            eprintln!("fixture kill failed: {error}");
        }
        let status = child
            .wait()
            .map_err(|error| format!("fixture wait: {error}"));
        let reader = self.reader.take().expect("fixture reader").join();
        println!("fixture pid={} wait={status:?}", child.id());
        reader.map_err(|error| format!("fixture reader panic: {error:?}"))?;
        let status = status?;
        if !status.success() {
            return Err(format!("fixture exited {status}; stdout={result:?}"));
        }
        result
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.child.is_some()
            && let Err(error) = self.stop()
        {
            eprintln!("fixture emergency cleanup: {error}");
        }
    }
}

fn configure_catalog(origin: &str, count: usize) {
    let address = origin.strip_prefix("http://").expect("local HTTP fixture");
    let mut stream = TcpStream::connect(address).expect("fixture control connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("control read deadline");
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .expect("control write deadline");
    let body = json!({"catalog_count": count}).to_string();
    write!(
        stream,
        "POST /__control HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("configure synthetic catalog");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("control response");
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
}
