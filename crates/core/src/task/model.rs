// allow: SIZE_OK - Shared task API is kept in the sole authorized model module.
use crate::{
    repository::{Bytes, CatalogId, Endpoint, NetworkAccess, RepositoryError, RepositoryRequest},
    selection::{SelectionChange, SelectionError, SelectionView},
    storage::{
        DownloadOptions, FileFailure, RecentTask, Settings, SettingsPaths, StorageError, TaskRecord,
    },
};
use std::{fmt, path::PathBuf, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct CoreConfig {
    pub paths: SettingsPaths,
    pub aria2_executable: PathBuf,
}
pub struct LoadRequest {
    pub repository: RepositoryRequest,
    pub access: NetworkAccess,
}
pub struct StartRequest {
    pub catalog: CatalogId,
    pub destination: PathBuf,
    pub options: DownloadOptions,
    pub verify_after_download: bool,
}
pub enum CoreCommand {
    Load(LoadRequest),
    LoadFrozen(LoadRequest),
    CancelLoad,
    NewDownload,
    SetFilter(String),
    SetSelectedOnly(bool),
    ChangeSelection(SelectionChange),
    ChangeProfile {
        catalog: CatalogId,
        index: usize,
        selected: bool,
    },
    Start(StartRequest),
    SelectTask(crate::TaskId),
    RemoveTask {
        id: crate::TaskId,
        confirmed: bool,
    },
    PauseTask(crate::TaskId),
    ResumeTask(crate::TaskId),
    LoadActive(crate::TaskId),
    PauseAll,
    ResumeAll,
    ConfirmConflicts {
        plan: u64,
        policy: super::ConflictPolicy,
    },
    CancelSubmission,
    StartPrepared(Box<super::PreparedTask>),
    AppendPrepared(super::PreparedAppend),
    OpenTask(PathBuf),
    SetAccess {
        endpoint: Endpoint,
        access: NetworkAccess,
    },
    Pause,
    Resume,
    RetryFailed,
    VerifyExisting,
    SaveSettings(Settings),
    SavePlatformTokens {
        hf: Option<crate::Token>,
        modelscope: Option<crate::Token>,
    },
    ForgetRecent(PathBuf),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Input,
    Authentication,
    NotFound,
    Network,
    RateLimited,
    UnsupportedEndpoint,
    UnsafePath,
    DestinationConflict,
    Locked,
    CorruptRecord,
    UnsupportedSchema,
    Disk,
    Engine,
    InvalidState,
    Cancelled,
    Internal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineFailure {
    UnsupportedProxy,
    Io,
    Protocol,
    Connection,
    Response(&'static str),
    Exited(Option<i32>),
    Rpc(i64),
    Timeout,
    Shutdown,
}
#[derive(Clone, Debug)]
pub enum ErrorDetail {
    Destination {
        path: PathBuf,
        owner: Box<crate::RepositorySpec>,
        same_repository: bool,
    },
    Http {
        endpoint: Endpoint,
        status: u16,
    },
    Engine {
        operation: &'static str,
        reason: EngineFailure,
    },
}
#[derive(Clone, Debug)]
pub struct UiError {
    pub code: ErrorCode,
    pub message: String,
    pub hint: String,
    pub detail: Option<ErrorDetail>,
}
#[derive(Debug)]
pub enum CoreError {
    Repository(RepositoryError),
    Selection(SelectionError),
    Storage(StorageError),
    DestinationInUse {
        path: PathBuf,
        owner: Box<crate::RepositorySpec>,
        same_repository: bool,
    },
    Engine(Box<UiError>),
    InvalidState {
        action: &'static str,
    },
    WorkerStopped,
}
impl From<RepositoryError> for CoreError {
    fn from(e: RepositoryError) -> Self {
        Self::Repository(e)
    }
}
impl From<SelectionError> for CoreError {
    fn from(e: SelectionError) -> Self {
        Self::Selection(e)
    }
}
impl From<StorageError> for CoreError {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}
impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.ui_error().message)
    }
}
impl std::error::Error for CoreError {}
impl CoreError {
    pub fn ui_error(&self) -> UiError {
        let (code, message, hint) = match self {
            Self::Repository(error) => match error {
                RepositoryError::Input(_) => (
                    ErrorCode::Input,
                    "Invalid repository input",
                    "Check the repository and revision",
                ),
                RepositoryError::Http {
                    status: 401 | 403, ..
                } => (
                    ErrorCode::Authentication,
                    "Authentication failed",
                    "Check access credentials",
                ),
                RepositoryError::Http { status: 404, .. } => (
                    ErrorCode::NotFound,
                    "Repository or file not found",
                    "Check the repository and revision",
                ),
                RepositoryError::Http { status: 429, .. } => (
                    ErrorCode::RateLimited,
                    "Repository rate limit reached",
                    "Try again later",
                ),
                RepositoryError::Http { .. } | RepositoryError::Network => (
                    ErrorCode::Network,
                    "Repository request failed",
                    "Check the endpoint and network",
                ),
                RepositoryError::UnsupportedEndpoint => (
                    ErrorCode::UnsupportedEndpoint,
                    "Download endpoint is unsupported",
                    "Downloads requiring a bearer token at the final URL are unsupported",
                ),
                RepositoryError::UnsafePath => (
                    ErrorCode::UnsafePath,
                    "Unsafe repository path",
                    "Choose a different repository",
                ),
                RepositoryError::InvalidResponse => (
                    ErrorCode::Network,
                    "Invalid repository response",
                    "Check endpoint compatibility",
                ),
                RepositoryError::Cancelled => (
                    ErrorCode::Cancelled,
                    "Operation cancelled",
                    "Retry if needed",
                ),
            },
            Self::Selection(_) => (
                ErrorCode::Input,
                "Invalid selection",
                "Load a complete catalog and select files",
            ),
            Self::DestinationInUse { .. } => (
                ErrorCode::DestinationConflict,
                "Destination belongs to an existing task",
                "Use a different repository folder or append to the existing version",
            ),
            Self::Storage(error) => match error {
                StorageError::InvalidOptions => (
                    ErrorCode::Input,
                    "Invalid download options",
                    "Choose values between 1 and 10",
                ),
                StorageError::Locked => (
                    ErrorCode::Locked,
                    "Task is already open",
                    "Close the other application",
                ),
                StorageError::ExistingTask(_) | StorageError::Conflict(_) => (
                    ErrorCode::DestinationConflict,
                    "Destination conflicts with existing files",
                    "Choose another destination or open the existing task",
                ),
                StorageError::UnsafePath(_) => (
                    ErrorCode::UnsafePath,
                    "Unsafe destination path",
                    "Choose a safe destination",
                ),
                StorageError::CorruptRecord => (
                    ErrorCode::CorruptRecord,
                    "Task record is corrupt",
                    "Preserve the record for inspection",
                ),
                StorageError::UnsupportedSchema(_) => (
                    ErrorCode::UnsupportedSchema,
                    "Task record version is unsupported",
                    "Use a compatible version",
                ),
                StorageError::Io(_) | StorageError::SizeMismatch => (
                    ErrorCode::Disk,
                    "Task storage operation failed",
                    "Check disk space and permissions",
                ),
            },
            Self::Engine(error) => return (**error).clone(),
            Self::InvalidState { .. } => (
                ErrorCode::InvalidState,
                "Command is unavailable in the current state",
                "Pause the current task first",
            ),
            Self::WorkerStopped => (
                ErrorCode::Internal,
                "Coordinator stopped unexpectedly",
                "Restart the application",
            ),
        };
        let detail = match self {
            Self::DestinationInUse {
                path,
                owner,
                same_repository,
            } => Some(ErrorDetail::Destination {
                path: path.clone(),
                owner: owner.clone(),
                same_repository: *same_repository,
            }),
            Self::Repository(RepositoryError::Http { endpoint, status }) => {
                Some(ErrorDetail::Http {
                    endpoint: endpoint.clone(),
                    status: *status,
                })
            }
            _ => None,
        };
        UiError {
            code,
            message: message.into(),
            hint: hint.into(),
            detail,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiPhase {
    Idle,
    Loading,
    Browsing,
    Preparing,
    Downloading,
    Verifying,
    Paused,
    Stopping,
    Closed,
}
#[derive(Clone, Debug)]
pub struct ActiveFile {
    pub index: usize,
    pub completed_bytes: Bytes,
    pub bytes_per_second: u64,
}
#[derive(Clone, Debug)]
pub struct TaskProgress {
    pub completed_files: usize,
    pub total_files: usize,
    pub acquired_bytes: Bytes,
    pub total_bytes: Bytes,
    pub bytes_per_second: u64,
    pub eta: Option<Duration>,
    pub active: Arc<[ActiveFile]>,
    pub failed: Arc<[(usize, FileFailure)]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerificationProgress {
    pub file_index: usize,
    pub file_bytes_done: u64,
    pub files_done: u64,
    pub files_total: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionOutcome {
    Verified,
    VerificationDisabled,
    Unverified,
    IntegrityFailed,
}
#[derive(Clone, Debug)]
pub struct CompletionNotice {
    pub epoch: u64,
    pub task_id: crate::storage::TaskId,
    pub outcome: CompletionOutcome,
    pub passed: u64,
    pub unavailable: u64,
    pub failed: u64,
}
pub type CompletionCallback = Arc<dyn Fn(CompletionNotice) + Send + Sync>;
#[derive(Clone, Debug)]
pub struct TaskView {
    pub record: Arc<TaskRecord>,
    pub destination: PathBuf,
    pub progress: TaskProgress,
    pub verification: Option<VerificationProgress>,
    pub restart_notice: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub current_log: Option<PathBuf>,
    pub log_directory: Option<PathBuf>,
    pub logging: Option<crate::logging::LogStatus>,
}
#[derive(Clone)]
pub struct AppSnapshot {
    pub revision: u64,
    pub phase: UiPhase,
    pub selection: SelectionView,
    pub task: Option<TaskView>,
    pub settings: Arc<Settings>,
    pub recent: Arc<[RecentTask]>,
    pub error: Option<UiError>,
    pub diagnostics: Diagnostics,
    pub queue: Option<Arc<super::QueueView>>,
}
pub struct CommandReceipt {
    pub snapshot_revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandSendError {
    Busy,
    Closed,
}
