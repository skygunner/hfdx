use super::{TaskView, UiPhase};
use crate::{
    repository::CatalogEntry,
    storage::{Replacement, TaskFile, TaskId, TaskRecord},
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictPolicy {
    Overwrite,
    NewerOnly,
    Archive,
}
#[derive(Clone, Debug)]
pub struct ConflictFile {
    pub entry: CatalogEntry,
    pub local_hash: crate::ContentHash,
    pub local_size: u64,
    pub local_modified_ms: u64,
    pub remote_modified_ms: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct ConflictPlan {
    pub id: u64,
    pub destination: PathBuf,
    pub files: Arc<[ConflictFile]>,
    pub new_files: usize,
    pub identical: usize,
}
#[derive(Clone, Debug)]
pub struct QueuedTask {
    pub view: TaskView,
    pub phase: UiPhase,
    pub error: Option<super::UiError>,
}
#[derive(Clone, Debug, Default)]
pub struct SubmissionSummary {
    pub added: usize,
    pub identical: usize,
    pub already_queued: usize,
    pub unchecked: usize,
    pub kept: usize,
}
#[derive(Clone, Debug, Default)]
pub struct QueueView {
    pub tasks: Arc<[QueuedTask]>,
    pub used_slots: usize,
    pub slot_limit: usize,
    pub preparing: bool,
    pub cancellable: bool,
    pub checking_path: Option<String>,
    pub checking_bytes: u64,
    pub checking_total: u64,
    pub conflicts: Option<ConflictPlan>,
    pub message: Option<String>,
    pub last_submission: Option<SubmissionSummary>,
    pub draft_error: Option<super::UiError>,
}
#[derive(Clone, Debug)]
pub struct PreparedTask {
    pub access: crate::NetworkAccess,
    pub record: TaskRecord,
    pub destination: PathBuf,
    pub stamps: BTreeMap<String, crate::storage::FileStamp>,
}
#[derive(Clone, Debug)]
pub struct PreparedAppend {
    pub task_id: TaskId,
    pub files: Vec<TaskFile>,
    pub replacements: BTreeMap<String, Replacement>,
    pub stamps: BTreeMap<String, crate::storage::FileStamp>,
}

/// Keep explicit draft folders, but don't carry another repository's task folder into a new download.
pub fn draft_destination(
    snapshot: &super::AppSnapshot,
    request: &crate::RepositoryRequest,
    current: &std::path::Path,
) -> Result<PathBuf, crate::StorageError> {
    let different_repo = |source: &crate::RepositorySpec| {
        source.request.endpoint != request.endpoint
            || source.request.kind != request.kind
            || source.request.repo_id != request.repo_id
    };
    let previous_default = snapshot.selection.catalog.as_ref().is_some_and(|catalog| {
        different_repo(&catalog.source)
            && crate::storage::default_destination(
                &snapshot.settings,
                &catalog.source.request.repo_id,
            )
            .is_ok_and(|path| path == current)
    });
    let other_task = snapshot.queue.as_ref().is_some_and(|queue| {
        queue.tasks.iter().any(|task| {
            task.view.destination == current && different_repo(&task.view.record.source)
        })
    });
    if current.as_os_str().is_empty() || previous_default || other_task {
        crate::storage::default_destination(&snapshot.settings, &request.repo_id)
    } else {
        Ok(current.into())
    }
}
