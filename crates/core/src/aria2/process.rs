use super::{Aria2Launch, ProbeError, ShutdownReceipt, StopKind, rpc::Rpc};
use rand::TryRngCore;
use std::{
    io::{self, Write},
    net::TcpListener,
    process::Stdio,
    time::Duration,
};
use tempfile::{NamedTempFile, TempDir};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::oneshot,
    task::JoinSet,
    time::{Instant, timeout, timeout_at},
};

pub(super) struct Process {
    pub(super) child: Child,
    pub(super) directory: TempDir,
    readers: JoinSet<Result<(), ProbeError>>,
    pub(super) pid: u32,
    pub(super) port: u16,
}
enum Ready {
    Listening,
    PortConflict,
}
fn private_config() -> Result<(TempDir, NamedTempFile, String, u16), ProbeError> {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let directory = builder.prefix("hfdx-aria2-").tempdir()?;
    let mut config = NamedTempFile::new_in(directory.path())?;
    let mut random = [0_u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut random)
        .map_err(|_| io::Error::other("OS randomness unavailable"))?;
    let mut secret = String::with_capacity(64);
    for byte in random {
        use std::fmt::Write;
        write!(secret, "{byte:02x}").map_err(|_| ProbeError::Protocol)?;
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    writeln!(config, "rpc-secret={secret}")?;
    config.flush()?;
    Ok((directory, config, secret, port))
}
impl Process {
    pub(super) async fn launch(
        config: &Aria2Launch,
    ) -> Result<(Self, Rpc, NamedTempFile), ProbeError> {
        for attempt in 0..3 {
            let (directory, private, secret, port) = tokio::task::spawn_blocking(private_config)
                .await
                .map_err(|_| ProbeError::Protocol)??;
            let rpc = Rpc::new(port, secret)?;
            let mut command = Command::new(&config.executable);
            command.env_clear();
            #[cfg(windows)]
            if let Some(system_root) = std::env::var_os("SystemRoot") {
                command.env("SystemRoot", system_root);
            }
            let mut child = command
                .arg("--conf-path")
                .arg(private.path())
                .args(
                    "--enable-rpc=true --rpc-listen-all=false --rpc-allow-origin-all=false \
                    --disable-ipv6=true --no-netrc=true --enable-color=false \
                    --enable-dht=false --enable-dht6=false --enable-peer-exchange=false --bt-enable-lpd=false \
                    --follow-torrent=false --follow-metalink=false --auto-file-renaming=false \
                    --allow-overwrite=false --check-certificate=true --continue=true --always-resume=true --file-allocation=none \
                    --summary-interval=0 --console-log-level=notice --download-result=hide \
                    --all-proxy= --http-proxy= --https-proxy= --ftp-proxy= --log= \
                    --max-download-result=1000 --keep-unfinished-download-result=false --auto-save-interval=1"
                        .split_ascii_whitespace(),
                )
                .arg(format!("--rpc-listen-port={port}"))
                .arg(format!("--stop-with-process={}", std::process::id()))
                .current_dir(directory.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let pid = child.id().ok_or(ProbeError::Protocol)?;
            let stdout = child.stdout.take().ok_or(ProbeError::Protocol)?;
            let (tx, rx) = oneshot::channel();
            let mut readers = JoinSet::new();
            readers.spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                let mut sender = Some(tx);
                let mut save_failed = false;
                while let Some(line) = lines.next_line().await? {
                    let ready = if line.contains(&format!("IPv4 RPC: listening on TCP port {port}"))
                    {
                        Some(Ready::Listening)
                    } else if line.contains("Failed to bind")
                        || line.contains("Address already in use")
                    {
                        Some(Ready::PortConflict)
                    } else {
                        None
                    };
                    if let Some(event) = ready
                        && let Some(tx) = sender.take()
                    {
                        // A cancelled launch still drains output until its owned child stops.
                        if tx.send(event).is_err() {
                            sender = None;
                        }
                    }
                    save_failed |= line.contains("Failed to save control file")
                        || line.contains("Failed to write into the segment file");
                }
                if save_failed {
                    Err(ProbeError::Protocol)
                } else {
                    Ok(())
                }
            });
            let mut process = Self {
                child,
                directory,
                readers,
                pid,
                port,
            };
            let ready = timeout(Duration::from_secs(15), rx).await;
            match ready {
                Ok(Ok(Ready::Listening)) => return Ok((process, rpc, private)),
                Ok(Ok(Ready::PortConflict)) | Err(_) if attempt < 2 => {
                    process.kill_wait().await?;
                    process.drain().await?;
                    private.close()?;
                    process.directory.close()?;
                }
                other => {
                    process.kill_wait().await?;
                    process.drain().await?;
                    private.close()?;
                    process.directory.close()?;
                    return Err(if other.is_err() {
                        ProbeError::Timeout
                    } else {
                        ProbeError::Protocol
                    });
                }
            }
        }
        Err(ProbeError::Protocol)
    }
    async fn kill_wait(&mut self) -> Result<std::process::ExitStatus, ProbeError> {
        if self.child.try_wait()?.is_none() {
            self.child.start_kill()?;
        }
        self.child.wait().await.map_err(ProbeError::Io)
    }
    async fn drain(&mut self) -> Result<(), ProbeError> {
        match self.readers.join_next().await {
            Some(Ok(result)) => result,
            Some(Err(_)) | None => Err(ProbeError::Protocol),
        }
    }
    pub(super) async fn abort(mut self) -> Result<(), ProbeError> {
        self.kill_wait().await?;
        let drained = self.drain().await;
        self.directory.close()?;
        drained
    }
    pub(super) async fn shutdown(mut self, rpc: &Rpc) -> Result<ShutdownReceipt, ProbeError> {
        let started = Instant::now();
        let graceful = started + Duration::from_secs(30);
        let deadline = graceful + Duration::from_secs(5);
        let exited = timeout_at(graceful, async {
            tokio::select! {
                status = self.child.wait() => status,
                _ = rpc.ok("aria2.shutdown", vec![]) => self.child.wait().await,
            }
        })
        .await;
        let mut stop = StopKind::Graceful;
        let status = match exited {
            Ok(result) => result?,
            Err(_) => {
                stop = StopKind::Forced;
                self.child.start_kill()?;
                timeout_at(deadline, self.child.wait())
                    .await
                    .map_err(|_| ProbeError::Timeout)??
            }
        };
        let receipt = ShutdownReceipt {
            stop,
            child_pid: Some(self.pid),
            child_exit_code: status.code(),
        };
        let drained = timeout_at(deadline, self.drain())
            .await
            .map_err(|_| ProbeError::Timeout)?;
        self.directory.close()?;
        drained?;
        // aria2 exits 7 when shutdown leaves resumable work. A lost RPC reply does not change that.
        if stop == StopKind::Graceful && (status.success() || status.code() == Some(7)) {
            Ok(receipt)
        } else {
            Err(ProbeError::Shutdown(receipt))
        }
    }
}
