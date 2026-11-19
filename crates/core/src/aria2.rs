use crate::storage::DownloadOptions;
use serde::{Deserialize, Serialize};
use std::{
    fmt, io,
    path::{Path, PathBuf},
    time::Instant,
};
#[path = "aria2/download.rs"]
mod download;
#[path = "aria2/process.rs"]
mod process;
#[path = "aria2/rpc.rs"]
mod rpc;
pub(crate) use download::{DownloadJob, EngineState, Gid};
use process::Process;
use rpc::Rpc;

#[derive(Debug)]
pub enum ProbeError {
    Io(io::Error),
    Protocol,
    Connection,
    Response(&'static str),
    Exited(Option<i32>),
    Rpc(i64),
    Timeout,
    UnsupportedProxy,
    Shutdown(ShutdownReceipt),
}
impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => f.write_str("probe I/O failed"),
            Self::Protocol => f.write_str("probe process or RPC protocol failed"),
            Self::Connection => f.write_str("aria2 RPC connection failed"),
            Self::Response(part) => write!(f, "aria2 RPC response invalid: {part}"),
            Self::Exited(code) => write!(f, "aria2 process exited ({code:?})"),
            Self::Rpc(code) => write!(f, "RPC failed with code {code}"),
            Self::Timeout => f.write_str("probe deadline exceeded"),
            Self::UnsupportedProxy => f.write_str("aria2 requires an HTTP proxy address"),
            Self::Shutdown(receipt) => write!(f, "engine shutdown failed ({:?})", receipt.stop),
        }
    }
}
impl std::error::Error for ProbeError {}
impl From<io::Error> for ProbeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
#[derive(Debug, Serialize)]
pub struct ProbeReceipt {
    pub parent_pid: u32,
    pub child_pid: u32,
    pub rpc_port: u16,
    pub version: String,
    pub session_id: String,
    pub authenticated: bool,
    pub unauthorized_code: i64,
    pub shutdown_response: String,
    pub child_exit_code: Option<i32>,
    pub shutdown_elapsed_ms: u128,
    pub private_config_removed: bool,
    pub temporary_directory_removed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum StopKind {
    Graceful,
    Forced,
}
#[derive(Debug, Serialize)]
pub struct ShutdownReceipt {
    pub stop: StopKind,
    pub child_pid: Option<u32>,
    pub child_exit_code: Option<i32>,
}
pub(crate) struct Aria2Launch {
    pub executable: PathBuf,
}
pub(crate) type Aria2Error = ProbeError;
pub(crate) struct Aria2 {
    rpc: Rpc,
    process: Process,
    version: String,
    session: String,
    unauthorized: i64,
    options: Option<DownloadOptions>,
    next_gid: u64,
}
#[derive(Deserialize)]
struct Version {
    version: String,
}
#[derive(Deserialize)]
struct Session {
    #[serde(rename = "sessionId")]
    id: String,
}
impl Aria2 {
    pub(crate) async fn launch(config: Aria2Launch) -> Result<Self, Aria2Error> {
        let (process, rpc, private) = Process::launch(&config).await?;
        let identity = async {
            let version: Version = rpc.retry("aria2.getVersion", vec![], true).await?;
            let session: Session = rpc.retry("aria2.getSessionInfo", vec![], true).await?;
            if version.version.is_empty()
                || session.id.len() != 40
                || !session.id.bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err(ProbeError::Protocol);
            }
            match rpc
                .retry::<Version>("aria2.getVersion", vec![], false)
                .await
            {
                Err(ProbeError::Rpc(code @ 1)) => Ok((version.version, session.id, code)),
                Err(error @ (ProbeError::Timeout | ProbeError::Connection)) => Err(error),
                Ok(_) | Err(_) => Err(ProbeError::Protocol),
            }
        }
        .await;
        let closed = private.close();
        match (identity, closed) {
            (Ok((version, session, unauthorized)), Ok(())) => Ok(Self {
                rpc,
                process,
                version,
                session,
                unauthorized,
                options: None,
                next_gid: 1,
            }),
            (result, cleanup) => {
                process.abort().await?;
                cleanup?;
                match result {
                    Err(error) => Err(error),
                    Ok(_) => Err(ProbeError::Protocol),
                }
            }
        }
    }
    pub(crate) fn pid(&self) -> u32 {
        self.process.pid
    }
    pub(crate) async fn shutdown(self) -> Result<ShutdownReceipt, Aria2Error> {
        self.process.shutdown(&self.rpc).await
    }
}

/// Probe the real private engine and return only after child wait and cleanup.
/// # Errors
/// Returns sanitized startup, authentication, shutdown, wait or cleanup failures.
pub async fn run_aria2_probe(executable: &Path) -> Result<ProbeReceipt, ProbeError> {
    let engine = Aria2::launch(Aria2Launch {
        executable: executable.into(),
    })
    .await?;
    let mut receipt = ProbeReceipt {
        parent_pid: std::process::id(),
        child_pid: engine.pid(),
        rpc_port: engine.process.port,
        version: engine.version.clone(),
        session_id: engine.session.clone(),
        authenticated: true,
        unauthorized_code: engine.unauthorized,
        shutdown_response: "OK".into(),
        child_exit_code: None,
        shutdown_elapsed_ms: 0,
        private_config_removed: true,
        temporary_directory_removed: false,
    };
    let started = Instant::now();
    let shutdown = engine.shutdown().await?;
    if shutdown.stop != StopKind::Graceful {
        return Err(ProbeError::Shutdown(shutdown));
    }
    receipt.child_exit_code = shutdown.child_exit_code;
    receipt.shutdown_elapsed_ms = started.elapsed().as_millis();
    receipt.temporary_directory_removed = true;
    Ok(receipt)
}
#[cfg(test)]
#[path = "aria2/tests/session.rs"]
mod session_tests;
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn aria2_authenticates_and_reaps_real_child() {
        let receipt = run_aria2_probe(Path::new("aria2c"))
            .await
            .expect("real aria2 probe");
        assert_eq!(receipt.version, "1.37.0");
        assert_eq!(receipt.unauthorized_code, 1);
        assert_eq!(receipt.child_exit_code, Some(0));
        assert!(receipt.private_config_removed && receipt.temporary_directory_removed);
        #[cfg(target_os = "linux")]
        assert!(!Path::new(&format!("/proc/{}", receipt.child_pid)).exists());
    }
    #[tokio::test]
    async fn aria2_missing_executable_is_an_error() {
        let result = run_aria2_probe(Path::new("/nonexistent/hfdx-missing-aria2")).await;
        assert!(
            matches!(result, Err(ProbeError::Io(error)) if error.kind() == io::ErrorKind::NotFound)
        );
    }
}
