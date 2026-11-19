use hfdx_core::{
    CatalogState, DownloadOptions, Endpoint, NetworkAccess, ProxySpec, RepoId, RepoKind,
    RepositoryClient, RepositoryRequest, Revision, SelectionChange, SettingsPaths, TaskState,
    task::{self, CoreCommand, CoreConfig, LoadRequest, StartRequest},
};
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

#[path = "network_round2/expiry.rs"]
mod expiry;
#[path = "network_round2/live.rs"]
mod live;

#[tokio::test]
async fn aria2_https_proxy_scheme_sends_plaintext_connect() {
    use tokio::io::AsyncReadExt;
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut child = tokio::process::Command::new("aria2c")
        .args([
            "--no-conf",
            "--max-tries=1",
            "--connect-timeout=3",
            "--timeout=3",
        ])
        .arg(format!("--dir={}", root.path().display()))
        .arg(format!("--all-proxy=https://{addr}"))
        .arg("https://example.com/")
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut buf = [0; 512];
    let count = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(buf[..count].starts_with(b"CONNECT "));
    println!("aria2_https_proxy_first_bytes=CONNECT tls_handshake=false");
    drop(stream);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    println!("aria2_capture_exit={status}");
    assert!(!status.success());
    if std::env::var_os("HFDX_ROUND2_PUBLIC_HF").as_deref() == Some(std::ffi::OsStr::new("1")) {
        live::public_hf_download_through_authenticated_connect_proxy().await;
    } else {
        println!("public_hf=NOT RUN (set HFDX_ROUND2_PUBLIC_HF=1 to opt in)");
    }
}

#[tokio::test]
async fn aria2_proxy_407_is_failure() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut child = tokio::process::Command::new("aria2c")
        .args([
            "--no-conf",
            "--max-tries=1",
            "--connect-timeout=3",
            "--timeout=3",
        ])
        .arg(format!("--dir={}", root.path().display()))
        .arg(format!(
            "--all-proxy=http://{}:{}@{addr}",
            "proof", "incorrect"
        ))
        .arg("https://example.com/")
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut request = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
    })
    .await
    .unwrap();
    let text = String::from_utf8(request).unwrap().to_ascii_lowercase();
    assert!(text.starts_with("connect "));
    assert!(text.contains("proxy-authorization: basic "));
    stream.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    drop(stream);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    drop(listener);
    let rebound = std::net::TcpListener::bind(addr).unwrap();
    println!(
        "wrong_credentials aria2_pid={pid} port={} proxy_status=407 aria2_exit={status} child_reaped=true port_rebind=true",
        addr.port()
    );
    drop(rebound);
}

struct Proxy(Child);
impl Drop for Proxy {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}
