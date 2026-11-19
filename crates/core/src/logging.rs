//! Fixed-field JSONL diagnostics; no process-global logger or terminal output.
// allow: SIZE_OK — fixed event vocabulary and session retention share the sole authorized logging module.
use crate::TaskId;
use flexi_logger::{
    Cleanup, Criterion, FileSpec, Naming,
    writers::{ArcFileLogWriter, FileLogWriter, FileLogWriterHandle},
};
use fs2::FileExt;
use serde::Serialize;
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

/// Stable event names. No user-controlled text can enter a record.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Startup,
    Shutdown,
    Metadata,
    TaskCreated,
    TaskStarted,
    TaskPaused,
    TaskResumed,
    PhaseChanged,
    TransferCompleted,
    Verification,
    TaskFailed,
    SettingsChanged,
    NotificationFailed,
    Pagination,
    Retry,
    Filter,
    EngineLaunch,
    EngineConfigure,
    EngineAdd,
    EngineStatus,
    EngineForget,
    EngineShutdown,
    RpcFailure,
    ProcessExit,
    ExitCancellation,
    LockRelease,
}
/// Levels supported by the diagnostic facade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
}
/// Safe outcome categories, never an error's Display or Debug representation.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogOutcome {
    Started,
    Succeeded,
    Cancelled,
    Disabled,
    Unavailable,
    Sha256,
    GitSha1,
    IntegrityFailed,
    Authentication,
    Network,
    RateLimited,
    Disk,
    Engine,
    InvalidState,
    Internal,
    Enabled,
    Paused,
    Downloading,
    Verifying,
    Closed,
}
/// A bounded diagnostic record. Numeric values describe counts, bytes and duration.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct LogEvent<'a> {
    pub event: EventKind,
    pub task_id: Option<&'a TaskId>,
    pub outcome: LogOutcome,
    pub count: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
}
/// HTTP measurements omit fields that are not available for the operation.
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct HttpLogEvent {
    pub event: EventKind,
    pub outcome: LogOutcome,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<u32>,
}
/// Optional measurements available at the event source; absent values are not fabricated.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct LogMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_operation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_failure: Option<crate::task::EngineFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<LogOutcome>,
}
/// Safe failures contain no original I/O message, filename, or error chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogError {
    DirectoryUnavailable,
    Io(io::ErrorKind),
    Rotation,
    Poisoned,
    RecordTooLarge,
}
impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "logging unavailable: {self:?}")
    }
}
impl std::error::Error for LogError {}
impl From<io::Error> for LogError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}
/// The first write/flush/retention failure remains visible until a new session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogStatus {
    Ready,
    BudgetExceeded,
    Failed(LogError),
}
/// Per-session limits. A record may not exceed the file limit.
#[derive(Clone, Debug)]
pub struct LogConfig {
    directory: PathBuf,
    debug: bool,
    file_bytes: u64,
    directory_bytes: u64,
}
impl LogConfig {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            debug: false,
            file_bytes: 10 * 1024 * 1024,
            directory_bytes: 100 * 1024 * 1024,
        }
    }
    pub fn with_debug(mut self, enabled: bool) -> Self {
        self.debug = enabled;
        self
    }
    pub fn with_limits(mut self, file_bytes: NonZeroU64, directory_bytes: NonZeroU64) -> Self {
        self.file_bytes = file_bytes.get();
        self.directory_bytes = directory_bytes.get();
        self
    }
}
/// Platform state directory (Linux XDG_STATE_HOME, Windows LocalAppData).
/// # Errors
/// Returns DirectoryUnavailable when the platform has no user state directory.
pub fn default_log_directory() -> Result<PathBuf, LogError> {
    #[cfg(target_os = "windows")]
    let base = dirs::data_local_dir();
    #[cfg(not(target_os = "windows"))]
    let base = dirs::state_dir();
    base.map(|path| path.join("hfdx/logs"))
        .ok_or(LogError::DirectoryUnavailable)
}
struct Output {
    writer: ArcFileLogWriter,
    _handle: FileLogWriterHandle,
    written: u64,
    status: LogStatus,
}
/// Independently owned session. Share an Arc; retain it until the core has joined.
/// Writes are synchronous and intended for lifecycle events, never per-frame progress.
pub struct SessionLogger {
    output: Mutex<Output>,
    config: LogConfig,
    session: String,
    debug: AtomicBool,
    // Field order closes the writer before releasing the session lock.
    _lock: File,
}
impl SessionLogger {
    /// Opens a uniquely named locked session and records startup.
    /// # Errors
    /// Returns a safe category when initialization or initial writing fails.
    pub fn new(config: LogConfig) -> Result<Self, LogError> {
        fs::create_dir_all(&config.directory)?;
        let root_lock = directory_lock(&config.directory)?;
        let session = format!("{:032x}", rand::random::<u128>());
        let path = config.directory.join(format!("hfdx-session-{session}"));
        fs::create_dir(&path)?;
        let lock = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path.join(".lock"))?;
        lock.lock_exclusive()?;
        let (writer, handle) = FileLogWriter::builder(
            FileSpec::default()
                .directory(&path)
                .basename("events")
                .suppress_timestamp(),
        )
        .rotate(
            Criterion::Size(config.file_bytes),
            Naming::Numbers,
            Cleanup::KeepLogFiles(4),
        )
        .cleanup_in_background_thread(false)
        .try_build_with_handle()
        .map_err(|_| LogError::Rotation)?;
        drop(root_lock);
        let logger = Self {
            output: Mutex::new(Output {
                writer,
                _handle: handle,
                written: 0,
                status: LogStatus::Ready,
            }),
            debug: AtomicBool::new(config.debug),
            config,
            session,
            _lock: lock,
        };
        logger.record(
            LogLevel::Info,
            LogEvent {
                event: EventKind::Startup,
                task_id: None,
                outcome: LogOutcome::Started,
                count: 0,
                bytes: 0,
                elapsed_ms: 0,
            },
        )?;
        logger.retain_closed()?;
        Ok(logger)
    }
    pub fn directory(&self) -> &Path {
        &self.config.directory
    }
    pub fn set_debug(&self, enabled: bool) {
        self.debug.store(enabled, Ordering::Relaxed);
    }
    pub fn status(&self) -> LogStatus {
        match self.output.lock() {
            Ok(output) => output.status,
            Err(_) => LogStatus::Failed(LogError::Poisoned),
        }
    }
    /// Serializes only the closed event vocabulary, task identifier and numbers.
    /// # Errors
    /// Returns and latches a safe error; callers surface it without failing a task.
    pub fn record(&self, level: LogLevel, event: LogEvent<'_>) -> Result<(), LogError> {
        self.record_with_metrics(level, event, LogMetrics::default())
    }
    /// Records an event with additional numeric or closed-vocabulary measurements.
    /// # Errors
    /// Returns a safe, latched logging error.
    pub fn record_with_metrics(
        &self,
        level: LogLevel,
        event: LogEvent<'_>,
        metrics: LogMetrics,
    ) -> Result<(), LogError> {
        self.write_record(level, event, metrics)
    }
    pub(crate) fn record_http(&self, event: HttpLogEvent) -> Result<(), LogError> {
        self.write_record(LogLevel::Debug, event, LogMetrics::default())
    }
    fn write_record(
        &self,
        level: LogLevel,
        event: impl Serialize,
        metrics: LogMetrics,
    ) -> Result<(), LogError> {
        if level == LogLevel::Debug && !self.debug.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut output = self.output.lock().map_err(|_| LogError::Poisoned)?;
        if let LogStatus::Failed(error) = output.status {
            return Err(error);
        }
        let mut line = serde_json::to_vec(&serde_json::json!({
            "time": httpdate::fmt_http_date(SystemTime::now()), "level": level,
            "version": env!("CARGO_PKG_VERSION"), "session": self.session,
            "pid": std::process::id(), "event": event, "metrics": metrics,
        }))
        .map_err(|_| LogError::RecordTooLarge)?;
        line.push(b'\n');
        let size = u64::try_from(line.len()).map_err(|_| LogError::RecordTooLarge)?;
        let result = (|| {
            if size > self.config.file_bytes {
                return Err(LogError::RecordTooLarge);
            }
            // Explicit library rotation propagates errors; its automatic write path swallows them.
            if output.written.saturating_add(size) > self.config.file_bytes {
                output.writer.rotate().map_err(|_| LogError::Rotation)?;
                output.written = 0;
            }
            output.writer.write_all(&line)?;
            output.written = output.written.saturating_add(size);
            Ok(())
        })();
        if let Err(error) = result {
            output.status = LogStatus::Failed(error);
        }
        result
    }
    /// Flushes the direct writer, then applies closed-session retention.
    /// # Errors
    /// Returns a latched safe write/flush/retention category.
    pub fn flush(&self) -> Result<(), LogError> {
        {
            let mut output = self.output.lock().map_err(|_| LogError::Poisoned)?;
            if let LogStatus::Failed(error) = output.status {
                return Err(error);
            }
            if let Err(error) = output.writer.flush() {
                let error = LogError::from(error);
                output.status = LogStatus::Failed(error);
                return Err(error);
            }
        }
        self.retain_closed()
    }
    fn retain_closed(&self) -> Result<(), LogError> {
        let result = retain(&self.config);
        let mut output = self.output.lock().map_err(|_| LogError::Poisoned)?;
        match result {
            Ok(over_budget) => {
                if !matches!(output.status, LogStatus::Failed(_)) {
                    output.status = if over_budget {
                        LogStatus::BudgetExceeded
                    } else {
                        LogStatus::Ready
                    };
                }
            }
            Err(error) => {
                output.status = LogStatus::Failed(error);
                return Err(error);
            }
        }
        Ok(())
    }
}
fn directory_lock(path: &Path) -> Result<File, LogError> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.join(".retention.lock"))?;
    lock.lock_exclusive()?;
    Ok(lock)
}
fn is_session(name: &str) -> bool {
    name.strip_prefix("hfdx-session-").is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}
fn is_log(name: &str) -> bool {
    match name
        .strip_prefix("events_r")
        .and_then(|name| name.strip_suffix(".log"))
    {
        Some("CURRENT") => true,
        Some(number) => !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}
fn session_files(path: &Path) -> Result<Option<(Vec<PathBuf>, u64)>, LogError> {
    let mut files = Vec::new();
    let mut bytes = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        if !entry.file_type()?.is_file() {
            return Ok(None);
        }
        match name.to_str() {
            Some(".lock") => {}
            Some(name) if is_log(name) => {
                // Windows directory entries can report stale sizes while a writer is open.
                let file = match File::open(entry.path()) {
                    Ok(file) => file,
                    // An active writer may rotate a file after read_dir enumerates its name.
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                bytes = bytes.saturating_add(file.metadata()?.len());
                files.push(entry.path());
            }
            _ => return Ok(None),
        }
    }
    Ok(Some((files, bytes)))
}
fn retain(config: &LogConfig) -> Result<bool, LogError> {
    let _root_lock = directory_lock(&config.directory)?;
    let mut total = 0u64;
    let mut closed = Vec::new();
    for entry in fs::read_dir(&config.directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !entry.file_name().to_str().is_some_and(is_session) {
            continue;
        }
        let path = entry.path();
        let Some((_, bytes)) = session_files(&path)? else {
            continue;
        };
        total = total.saturating_add(bytes);
        let lock_path = path.join(".lock");
        if !fs::symlink_metadata(&lock_path)?.file_type().is_file() {
            continue;
        }
        let lock = OpenOptions::new().read(true).write(true).open(&lock_path)?;
        match lock.try_lock_exclusive() {
            Ok(()) => closed.push((entry.metadata()?.modified()?, path, lock)),
            Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {}
            Err(error) => return Err(error.into()),
        }
    }
    closed.sort_by_key(|(modified, _, _)| *modified);
    for (_, path, lock) in closed {
        if total <= config.directory_bytes {
            break;
        }
        // Re-enumerate under both locks; unknown files and symlinks are never deleted.
        let Some((files, bytes)) = session_files(&path)? else {
            continue;
        };
        for file in files {
            fs::remove_file(file)?;
        }
        // Creation/cleanup share the root lock; old random session IDs are never reopened.
        // Close this handle before unlinking, including on Windows.
        drop(lock);
        fs::remove_file(path.join(".lock"))?;
        fs::remove_dir(path)?;
        total = total.saturating_sub(bytes);
    }
    Ok(total > config.directory_bytes)
}
