use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub size: u64,
    pub modified_ns: u64,
    pub created_ns: u64,
    pub identity: (u64, u64),
}
impl FileStamp {
    pub(crate) fn read(path: &Path) -> Result<Option<Self>, StorageError> {
        let Some(meta) = check_path(path)? else {
            return Ok(None);
        };
        if !meta.is_file() {
            return Err(StorageError::Conflict(path.into()));
        }
        let nanos = |time: io::Result<SystemTime>| {
            time.ok()
                .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
                .and_then(|v| u64::try_from(v.as_nanos()).ok())
                .unwrap_or(0)
        };
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino())
        };
        #[cfg(not(unix))]
        let identity = (0, 0);
        Ok(Some(Self {
            size: meta.len(),
            modified_ns: nanos(meta.modified()),
            created_ns: nanos(meta.created()),
            identity,
        }))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    pub archive: bool,
    pub original: FileStamp,
    #[serde(default)]
    pub ready: Option<FileStamp>,
    #[serde(default)]
    pub archive_dir: Option<String>,
}
pub(super) fn file_path(dest: &Path, record: &TaskRecord, entry: &CatalogEntry) -> PathBuf {
    if record.replacements.contains_key(entry.path.as_str()) {
        dest.join(".hfdx/incoming")
            .join(record.task_id.as_str())
            .join(entry.path.as_str())
    } else {
        dest.join(entry.path.as_str())
    }
}
fn stamps_match(dest: &Path, stamps: &BTreeMap<String, FileStamp>) -> Result<(), StorageError> {
    for (path, expected) in stamps {
        validate_relative(path)?;
        if FileStamp::read(&dest.join(path))?.as_ref() != Some(expected) {
            return Err(StorageError::Conflict(dest.join(path)));
        }
    }
    Ok(())
}
impl TaskStore {
    pub(crate) async fn adopt(
        prepared: crate::task::PreparedTask,
    ) -> Result<OpenedTask, StorageError> {
        tokio::task::spawn_blocking(move || {
            let crate::task::PreparedTask {
                record,
                destination,
                stamps,
                ..
            } = prepared;
            validate_record(&record)?;
            let lock = lock_destination(&destination)?;
            stamps_match(&destination, &stamps)?;
            let path = destination.join(".hfdx/task.json");
            if check_path(&path)?.is_some() {
                let old = read_record(&path)?;
                if old.source.request.endpoint != record.source.request.endpoint
                    || old.source.request.repo_id != record.source.request.repo_id
                    || old.source.request.kind != record.source.request.kind
                {
                    return Err(StorageError::Conflict(path));
                }
                if old.task_id != record.task_id {
                    let history = destination.join(".hfdx/history");
                    check_path(&history)?;
                    fs::create_dir_all(&history)?;
                    check_path(&history)?;
                    let saved = history.join(format!(
                        "{}-{}.json",
                        old.task_id.as_str(),
                        record.created_unix_ms
                    ));
                    if check_path(&saved)?.is_some() {
                        return Err(StorageError::Conflict(saved));
                    }
                    atomic_write(&saved, &old)?;
                }
            }
            for file in &record.files {
                let logical = destination.join(file.entry.path.as_str());
                if check_path(&logical)?.is_some() && !stamps.contains_key(file.entry.path.as_str())
                {
                    // Existing owned files are accepted only when extending their same record.
                    let old = read_record(&path)?;
                    if old.task_id != record.task_id
                        || !old.files.iter().any(|f| f.entry == file.entry)
                    {
                        return Err(StorageError::Conflict(logical));
                    }
                }
            }
            preflight(&destination, &record, false)?;
            atomic_write(&path, &record)?;
            Ok(OpenedTask {
                store: Self {
                    destination,
                    lock,
                    identity: record.clone(),
                },
                record,
            })
        })
        .await
        .map_err(join_error)?
    }
    pub(crate) async fn append(
        &mut self,
        record: &TaskRecord,
        stamps: BTreeMap<String, FileStamp>,
    ) -> Result<(), StorageError> {
        validate_record(record)?;
        if record.task_id != self.identity.task_id
            || record.source != self.identity.source
            || record.options != self.identity.options
            || record.verify_after_download != self.identity.verify_after_download
            || record.files.len() < self.identity.files.len()
            || self
                .identity
                .files
                .iter()
                .zip(&record.files)
                .any(|(a, b)| a.entry != b.entry)
        {
            return Err(StorageError::CorruptRecord);
        }
        let dest = self.destination.clone();
        let saved = record.clone();
        let prefix = self.identity.files.len();
        let lock = self.lock.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            stamps_match(&dest, &stamps)?;
            for file in &saved.files[prefix..] {
                if !stamps.contains_key(file.entry.path.as_str())
                    && (check_path(&dest.join(file.entry.path.as_str()))?.is_some()
                        || check_path(&control_path(&dest.join(file.entry.path.as_str())))?
                            .is_some())
                {
                    return Err(StorageError::Conflict(dest.join(file.entry.path.as_str())));
                }
            }
            preflight(&dest, &saved, false)?;
            atomic_write(&dest.join(".hfdx/task.json"), &saved)
        })
        .await
        .map_err(join_error)??;
        self.identity = record.clone();
        Ok(())
    }
    pub(crate) async fn publish_replacement(
        &mut self,
        record: &mut TaskRecord,
        index: usize,
    ) -> Result<(), StorageError> {
        let name = record.files[index].entry.path.as_str().to_owned();
        let Some(replacement) = record.replacements.get_mut(&name) else {
            return Ok(());
        };
        let staged = self
            .destination
            .join(".hfdx/incoming")
            .join(record.task_id.as_str())
            .join(&name);
        replacement.ready = FileStamp::read(&staged)?;
        if replacement.ready.is_none() {
            return Err(StorageError::Conflict(staged));
        }
        if replacement.archive && replacement.archive_dir.is_none() {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| StorageError::Io(io::Error::other(e)))?
                .as_millis();
            replacement.archive_dir = Some(format!("{stamp}-{}", record.task_id.as_str()));
        }
        let replacement = replacement.clone();
        self.identity.replacements = record.replacements.clone();
        self.persist(record).await?;
        let dest = self.destination.clone();
        let saved = record.clone();
        let path = file_path(&dest, &saved, &record.files[index].entry);
        let lock = self.lock.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            let target = dest.join(&name);
            let archived = replacement
                .archive_dir
                .as_ref()
                .map(|dir| dest.join("archive").join(dir).join(&name));
            let already_archived = FileStamp::read(&target)?.is_none()
                && archived.as_ref().is_some_and(|old| {
                    FileStamp::read(old).ok().flatten().as_ref() == Some(&replacement.original)
                });
            if !already_archived
                && FileStamp::read(&target)?.as_ref() != Some(&replacement.original)
            {
                return Err(StorageError::Conflict(target));
            }
            check_path(&path)?.ok_or_else(|| StorageError::Conflict(path.clone()))?;
            if replacement.archive {
                let archive = dest.join("archive").join(
                    replacement
                        .archive_dir
                        .as_ref()
                        .ok_or(StorageError::CorruptRecord)?,
                );
                let old = archive.join(&name);
                let parent = old
                    .parent()
                    .ok_or_else(|| StorageError::UnsafePath(old.clone()))?;
                check_path(parent)?;
                fs::create_dir_all(parent)?;
                check_path(parent)?;
                if !already_archived {
                    if check_path(&old)?.is_some() {
                        return Err(StorageError::Conflict(old));
                    }
                    fs::rename(&target, &old)?;
                }
                let control = control_path(&target);
                if check_path(&control)?.is_some() {
                    fs::rename(control, control_path(&old))?;
                }
            } else {
                let control = control_path(&target);
                if check_path(&control)?.is_some() {
                    fs::remove_file(control)?;
                }
            }
            fs::rename(&path, &target)?;
            Ok::<_, StorageError>(())
        })
        .await
        .map_err(join_error)??;
        record
            .replacements
            .remove(record.files[index].entry.path.as_str());
        self.identity.replacements = record.replacements.clone();
        self.persist(record).await
    }
    pub(crate) async fn resume_publications(
        &mut self,
        record: &mut TaskRecord,
    ) -> Result<(), StorageError> {
        for index in 0..record.files.len() {
            let entry = &record.files[index].entry;
            let name = entry.path.as_str();
            let Some(replacement) = record.replacements.get(name) else {
                continue;
            };
            let Some(ready) = &replacement.ready else {
                continue;
            };
            let staging = file_path(&self.destination, record, entry);
            if FileStamp::read(&staging)?.is_none()
                && FileStamp::read(&self.destination.join(name))?.as_ref() == Some(ready)
            {
                record.replacements.remove(name);
                self.identity.replacements = record.replacements.clone();
                self.persist(record).await?;
            } else {
                self.publish_replacement(record, index).await?;
            }
        }
        Ok(())
    }
}
