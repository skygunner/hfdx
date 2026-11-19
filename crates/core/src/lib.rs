//! Shared core for hfdx.

mod aria2;
pub mod display;
pub mod integrity;
pub mod locale;
pub mod logging;
pub mod notification;
pub mod repository;
pub mod selection;
pub mod storage;
pub mod task;

pub use aria2::{ProbeError, ProbeReceipt, ShutdownReceipt, StopKind, run_aria2_probe};
pub use repository::{
    Bytes, CatalogEntry, CatalogId, CatalogPage, CatalogRequest, CommitId, ContentHash,
    DownloadUrl, Endpoint, EntryId, HashAlgorithm, NetworkAccess, ProxyAddress, ProxySpec, RepoId,
    RepoKind, RepoPath, RepositoryClient, RepositoryError, RepositoryRequest, RepositorySpec,
    Revision, Token, parse_repository,
};
pub use selection::{
    Catalog, CatalogState, FrozenSelection, Selection, SelectionChange, SelectionError,
    SelectionSummary, SelectionView,
};
pub use storage::{
    DownloadOptions, FailureKind, FileFailure, Language, RecentTask, Settings, SettingsPaths,
    StorageError, TaskFile, TaskId, TaskRecord, TaskState, Theme, VerificationStatus,
};

pub mod profiles;

pub mod updates;
pub mod version;
