use super::*;
use crate::{
    integrity::{IntegrityResult, verify_file},
    storage::{self, FileDisposition, FileStamp, Replacement, TaskFile, VerificationStatus},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Default)]
pub(super) struct Checking {
    pub path: String,
    pub done: u64,
    pub total: u64,
}
pub(super) struct Submission {
    pub record: crate::TaskRecord,
    pub destination: PathBuf,
    pub files: Vec<TaskFile>,
    pub conflicts: Vec<ConflictFile>,
    pub stamps: BTreeMap<String, FileStamp>,
    pub replacements: BTreeMap<String, Replacement>,
    pub identical: usize,
    pub unknown: usize,
    pub duplicate: usize,
}
pub(super) fn same_source(a: &crate::RepositorySpec, b: &crate::RepositorySpec) -> bool {
    a.request.endpoint == b.request.endpoint
        && a.request.kind == b.request.kind
        && a.request.repo_id == b.request.repo_id
        && a.resolved_commit == b.resolved_commit
}
pub(super) fn destination(path: &Path) -> Result<PathBuf, CoreError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(crate::StorageError::Io)?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component),
        }
    }
    storage::check_path(&normalized)?;
    #[cfg(windows)]
    let normalized = {
        // Resolve filesystem casing and aliases, retaining the spelling of not-yet-created folders.
        let mut parent = normalized.as_path();
        let mut missing = Vec::new();
        while !parent.exists() {
            missing.push(
                parent
                    .file_name()
                    .ok_or(crate::StorageError::CorruptRecord)?
                    .to_owned(),
            );
            parent = parent.parent().ok_or(crate::StorageError::CorruptRecord)?;
        }
        let canonical = std::fs::canonicalize(parent).map_err(crate::StorageError::Io)?;
        let mut components = canonical.components();
        let mut result = match components.next() {
            Some(std::path::Component::Prefix(prefix)) => match prefix.kind() {
                std::path::Prefix::VerbatimDisk(drive) => {
                    PathBuf::from(format!("{}:", char::from(drive)))
                }
                std::path::Prefix::VerbatimUNC(server, share) => {
                    let mut value = std::ffi::OsString::from(r"\\");
                    value.push(server);
                    value.push(r"\");
                    value.push(share);
                    PathBuf::from(value)
                }
                _ => PathBuf::from(prefix.as_os_str()),
            },
            _ => return Err(crate::StorageError::CorruptRecord.into()),
        };
        for component in components {
            result.push(component.as_os_str());
        }
        for component in missing.into_iter().rev() {
            result.push(component);
        }
        result
    };
    Ok(normalized)
}
pub(super) async fn inspect(
    frozen: crate::FrozenSelection,
    request: StartRequest,
    existing: Option<crate::TaskRecord>,
    budget: Arc<super::budget::Budget>,
    cancel: CancellationToken,
    progress: watch::Sender<Checking>,
) -> Result<Submission, CoreError> {
    let in_session = existing.is_some();
    let dest = destination(&request.destination)?;
    let existing = if let Some(record) = existing {
        Some(record)
    } else {
        let path = dest.join(".hfdx/task.json");
        if storage::check_path(&path)?.is_some() {
            Some(storage::read_record(&path)?)
        } else {
            None
        }
    };
    if let Some(record) = &existing
        && (record.source.request.endpoint != frozen.source.request.endpoint
            || record.source.request.repo_id != frozen.source.request.repo_id
            || record.source.request.kind != frozen.source.request.kind)
    {
        return Err(CoreError::DestinationInUse {
            path: dest,
            owner: Box::new(record.source.clone()),
            same_repository: false,
        });
    }
    let base = existing.filter(|r| same_source(&r.source, &frozen.source));
    let record = match &base {
        Some(record) => record.clone(),
        None => storage::new_record(frozen.source.clone(), frozen.files.clone(), request.options)?,
    };
    let mut submission = Submission {
        record,
        destination: dest.clone(),
        files: Vec::new(),
        conflicts: Vec::new(),
        stamps: BTreeMap::new(),
        replacements: BTreeMap::new(),
        identical: 0,
        unknown: 0,
        duplicate: 0,
    };
    submission.record.schema_version = 2;
    if base.is_none() {
        submission.record.verify_after_download = Some(request.verify_after_download);
        submission.record.files.clear();
    }
    for entry in frozen.files {
        if cancel.is_cancelled() {
            return Err(crate::RepositoryError::Cancelled.into());
        }
        if let Some(record) = &base
            && let Some(owned) = record.files.iter().find(|f| f.entry == entry)
        {
            let in_flight = !owned.completed
                || (owned.failure.is_none()
                    && record.replacements.contains_key(entry.path.as_str()))
                || (record.state == crate::TaskState::Running
                    && owned.verification == VerificationStatus::Pending);
            if in_flight {
                if in_session {
                    submission.duplicate += 1;
                } else {
                    submission.files.push(owned.clone());
                }
                continue;
            }
        }
        let path = dest.join(entry.path.as_str());
        let stamp = FileStamp::read(&path)?;
        let mut file = TaskFile {
            entry: entry.clone(),
            completed: false,
            failure: None,
            verification: VerificationStatus::Pending,
            disposition: FileDisposition::Download,
        };
        if let Some(stamp) = stamp {
            if entry.content_hash.is_none() {
                file.completed = true;
                file.verification = VerificationStatus::Unavailable;
                file.disposition = FileDisposition::Unchecked;
                submission.unknown += 1;
            } else {
                let slot = loop {
                    if cancel.is_cancelled() {
                        return Err(crate::RepositoryError::Cancelled.into());
                    }
                    if let Some(slot) = budget.hash() {
                        break slot;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                };
                let cancel_hash = cancel.clone();
                let hash = entry.content_hash.clone();
                let name = entry.path.as_str().to_owned();
                let sender = progress.clone();
                let read_path = path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    verify_file(&read_path, hash.as_ref(), &cancel_hash, |done, total| {
                        sender.send_replace(Checking {
                            path: name.clone(),
                            done,
                            total,
                        });
                    })
                })
                .await
                .map_err(|_| CoreError::WorkerStopped)?;
                drop(slot);
                if FileStamp::read(&path)?.as_ref() != Some(&stamp) {
                    return Err(crate::StorageError::Conflict(path).into());
                }
                match result {
                    Ok(IntegrityResult::Matched) => {
                        file.completed = true;
                        file.verification = VerificationStatus::Passed;
                        file.disposition = FileDisposition::ExistingMatch;
                        submission.identical += 1;
                    }
                    Ok(IntegrityResult::Mismatch { actual }) => {
                        submission.conflicts.push(ConflictFile {
                            entry,
                            local_hash: actual,
                            local_size: stamp.size,
                            local_modified_ms: stamp.modified_ns / 1_000_000,
                            remote_modified_ms: None,
                        });
                        submission
                            .stamps
                            .insert(file.entry.path.as_str().to_owned(), stamp);
                        continue;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(crate::RepositoryError::Cancelled.into());
                    }
                    _ => {
                        file.completed = true;
                        file.verification = VerificationStatus::Unavailable;
                        file.disposition = FileDisposition::Unchecked;
                        submission.unknown += 1;
                    }
                }
            }
            submission
                .stamps
                .insert(file.entry.path.as_str().to_owned(), stamp);
        }
        submission.files.push(file);
    }
    Ok(submission)
}
pub(super) async fn decide(
    mut submission: Submission,
    policy: ConflictPolicy,
    access: crate::NetworkAccess,
    cancel: CancellationToken,
) -> Result<Submission, CoreError> {
    let client = crate::RepositoryClient::new(&submission.record.source.request.endpoint, access)?;
    for conflict in std::mem::take(&mut submission.conflicts) {
        let name = conflict.entry.path.as_str().to_owned();
        let remote = if policy == ConflictPolicy::NewerOnly {
            client
                .file_modified_ms(&submission.record.source, &conflict.entry.path, &cancel)
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        let replace = policy != ConflictPolicy::NewerOnly
            || remote.is_some_and(|time| time > conflict.local_modified_ms);
        let file = TaskFile {
            entry: conflict.entry,
            completed: !replace,
            failure: None,
            verification: if replace {
                VerificationStatus::Pending
            } else {
                VerificationStatus::Unavailable
            },
            disposition: if replace {
                FileDisposition::Download
            } else if remote.is_none() {
                FileDisposition::TimeUnknown
            } else {
                FileDisposition::KeptDifferent
            },
        };
        if replace {
            submission.replacements.insert(
                name.clone(),
                Replacement {
                    archive: policy == ConflictPolicy::Archive,
                    original: submission.stamps[&name].clone(),
                    ready: None,
                    archive_dir: None,
                },
            );
        }
        submission.files.push(file);
    }
    Ok(submission)
}
