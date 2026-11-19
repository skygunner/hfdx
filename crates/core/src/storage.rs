#[cfg(test)]
mod tests;

use crate::repository::{CatalogEntry, Endpoint, ProxyAddress, RepoId, RepositorySpec};
use fs2::FileExt;
use rand::TryRngCore;
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TaskId(String);
impl TaskId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for TaskId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() != 32
            || !s
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(serde::de::Error::custom("invalid task id"));
        }
        Ok(Self(s))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DownloadOptions {
    connections_per_file: u8,
    concurrent_files: u8,
}
impl DownloadOptions {
    pub fn new(connections_per_file: u8, concurrent_files: u8) -> Result<Self, StorageError> {
        if !(1..=10).contains(&connections_per_file) || !(1..=10).contains(&concurrent_files) {
            return Err(StorageError::InvalidOptions);
        }
        Ok(Self {
            connections_per_file,
            concurrent_files,
        })
    }
    pub const fn connections_per_file(&self) -> u8 {
        self.connections_per_file
    }
    pub const fn concurrent_files(&self) -> u8 {
        self.concurrent_files
    }
}
impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            connections_per_file: 4,
            concurrent_files: 5,
        }
    }
}
impl<'de> Deserialize<'de> for DownloadOptions {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Values {
            connections_per_file: u8,
            concurrent_files: u8,
        }
        let v = Values::deserialize(d)?;
        Self::new(v.connections_per_file, v.concurrent_files).map_err(serde::de::Error::custom)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Paused,
    Running,
    Completed,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Network,
    Authentication,
    Disk,
    Engine,
    Integrity,
    Interrupted,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct FileFailure {
    pub kind: FailureKind,
    pub message: String,
    #[serde(default)]
    pub aria2_code: Option<u32>,
    #[serde(default)]
    pub http_status: Option<u16>,
}
impl Serialize for FileFailure {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let message = match self.kind {
            FailureKind::Network => "Network error",
            FailureKind::Authentication => "Authentication error",
            FailureKind::Disk => "Disk error",
            FailureKind::Engine => "Download engine error",
            FailureKind::Integrity => "Integrity check failed",
            FailureKind::Interrupted => "Download interrupted",
        };
        let mut value = serializer.serialize_struct("FileFailure", 4)?;
        value.serialize_field("kind", &self.kind)?;
        value.serialize_field("message", message)?;
        value.serialize_field("aria2_code", &self.aria2_code)?;
        value.serialize_field("http_status", &self.http_status)?;
        value.end()
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    #[default]
    LegacyUnverified,
    Pending,
    Passed,
    Unavailable,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFile {
    pub entry: CatalogEntry,
    pub completed: bool,
    pub failure: Option<FileFailure>,
    #[serde(default)]
    pub verification: VerificationStatus,
    #[serde(default)]
    pub disposition: FileDisposition,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDisposition {
    #[default]
    Download,
    ExistingMatch,
    KeptDifferent,
    TimeUnknown,
    Unchecked,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub schema_version: u32,
    pub task_id: TaskId,
    pub source: RepositorySpec,
    pub files: Vec<TaskFile>,
    pub options: DownloadOptions,
    pub proxy_address: Option<ProxyAddress>,
    #[serde(default)]
    pub verify_after_download: Option<bool>,
    pub state: TaskState,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub replacements: std::collections::BTreeMap<String, Replacement>,
}
#[path = "storage/queue.rs"]
mod queue;
pub use queue::{FileStamp, Replacement};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    English,
    ZhHans,
    ZhHant,
}

impl Language {
    pub fn from_locale(locale: &str) -> Self {
        let normalized = locale
            .split(['.', '@'])
            .next()
            .unwrap_or("")
            .replace('_', "-")
            .to_ascii_lowercase();
        let mut parts = normalized.split('-');
        if parts.next() != Some("zh") {
            return Self::English;
        }
        match parts.next() {
            Some("tw" | "hk" | "mo" | "hant") => Self::ZhHant,
            Some("cn" | "sg" | "hans") => Self::ZhHans,
            _ => Self::English,
        }
    }
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub auto_update_check: bool,
    #[serde(default)]
    pub update_interval: crate::updates::Interval,
    #[serde(
        default,
        serialize_with = "serialize_saved_token",
        deserialize_with = "deserialize_saved_token"
    )]
    pub hf_token: Option<crate::repository::Token>,
    #[serde(
        default,
        serialize_with = "serialize_saved_token",
        deserialize_with = "deserialize_saved_token"
    )]
    pub modelscope_token: Option<crate::repository::Token>,
    pub endpoint: Endpoint,
    pub options: DownloadOptions,
    pub proxy_address: Option<ProxyAddress>,
    pub recent_directories: Vec<PathBuf>,
    pub recent_tasks: Vec<PathBuf>,
    #[serde(default = "enabled_by_default")]
    pub verify_after_download: bool,
    #[serde(default = "enabled_by_default")]
    pub notify_on_completion: bool,
    #[serde(default)]
    pub sound_on_completion: bool,
    #[serde(default)]
    pub debug_logging: bool,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub language: Option<Language>,
}
impl Settings {
    pub fn token_for(&self, endpoint: &Endpoint) -> Option<crate::repository::Token> {
        if endpoint.url().scheme() != "https" || endpoint.url().port().is_some() {
            return None;
        }
        if endpoint.is_modelscope() {
            self.modelscope_token.clone()
        } else if endpoint.url().host_str() == Some("huggingface.co") {
            self.hf_token.clone()
        } else {
            None
        }
    }
    pub fn effective_language(&self) -> Language {
        self.language
            .unwrap_or_else(|| Language::from_locale(&sys_locale::get_locale().unwrap_or_default()))
    }
}
fn serialize_saved_token<S: serde::Serializer>(
    token: &Option<crate::repository::Token>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    token
        .as_ref()
        .map(crate::repository::Token::expose)
        .serialize(serializer)
}
fn deserialize_saved_token<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<crate::repository::Token>, D::Error> {
    Option::<String>::deserialize(deserializer)?
        .map(|s| crate::repository::Token::parse(&s))
        .transpose()
        .map_err(serde::de::Error::custom)
}
#[derive(Clone)]
pub struct SettingsPaths {
    pub settings_file: PathBuf,
}
#[derive(Debug)]
pub struct RecentTask {
    pub task_file: PathBuf,
    pub destination_exists: bool,
    pub state: Option<TaskState>,
    pub issue: Option<String>,
}
#[derive(Debug)]
pub enum StorageError {
    InvalidOptions,
    Locked,
    ExistingTask(PathBuf),
    Conflict(PathBuf),
    UnsafePath(PathBuf),
    CorruptRecord,
    UnsupportedSchema(u32),
    Io(io::Error),
    SizeMismatch,
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for StorageError {}
impl From<io::Error> for StorageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
fn join_error(e: tokio::task::JoinError) -> StorageError {
    StorageError::Io(io::Error::other(e))
}
pub fn settings_paths() -> Result<SettingsPaths, StorageError> {
    Ok(SettingsPaths {
        settings_file: dirs::config_dir()
            .ok_or_else(|| {
                StorageError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    "config directory unavailable",
                ))
            })?
            .join("hfdx/settings.json"),
    })
}
fn defaults() -> Result<Settings, StorageError> {
    let endpoint = match std::env::var("HF_ENDPOINT") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "https://huggingface.co".to_owned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(StorageError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HF_ENDPOINT must be valid Unicode",
            )));
        }
    };
    Ok(Settings {
        auto_update_check: false,
        update_interval: Default::default(),
        hf_token: None,
        modelscope_token: None,
        endpoint: Endpoint::parse(&endpoint, false).map_err(|_| {
            StorageError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HF_ENDPOINT must be a valid HTTPS endpoint; HTTP requires explicit consent",
            ))
        })?,
        options: DownloadOptions::default(),
        proxy_address: None,
        recent_directories: Vec::new(),
        recent_tasks: Vec::new(),
        verify_after_download: true,
        notify_on_completion: true,
        sound_on_completion: false,
        debug_logging: false,
        theme: Theme::System,
        language: None,
    })
}
pub(crate) fn atomic_write<T: Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::UnsafePath(path.into()))?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut tmp, value).map_err(|_| StorageError::CorruptRecord)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| StorageError::Io(e.error))?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}
pub async fn load_settings(paths: &SettingsPaths) -> Result<Settings, StorageError> {
    let path = paths.settings_file.clone();
    tokio::task::spawn_blocking(move || match fs::read(path) {
        Ok(bytes) => {
            let mut s: Settings =
                serde_json::from_slice(&bytes).map_err(|_| StorageError::CorruptRecord)?;
            s.recent_directories.truncate(10);
            s.recent_tasks.truncate(10);
            Ok(s)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => defaults(),
        Err(e) => Err(e.into()),
    })
    .await
    .map_err(join_error)?
}
pub async fn save_settings(paths: &SettingsPaths, settings: &Settings) -> Result<(), StorageError> {
    let path = paths.settings_file.clone();
    let mut settings = settings.clone();
    settings.recent_directories.truncate(10);
    settings.recent_tasks.truncate(10);
    tokio::task::spawn_blocking(move || atomic_write(&path, &settings))
        .await
        .map_err(join_error)?
}
pub async fn recent_tasks(paths: &[PathBuf]) -> Vec<RecentTask> {
    let paths = paths.to_vec();
    let failed_paths = paths.clone();
    match tokio::task::spawn_blocking(move || {
        paths
            .into_iter()
            .map(|task_file| {
                let destination_exists = task_file
                    .parent()
                    .and_then(Path::parent)
                    .is_some_and(Path::exists);
                match read_record(&task_file) {
                    Ok(record) => RecentTask {
                        task_file,
                        destination_exists,
                        state: Some(if record.state == TaskState::Running {
                            TaskState::Paused
                        } else {
                            record.state
                        }),
                        issue: None,
                    },
                    Err(e) => RecentTask {
                        task_file,
                        destination_exists,
                        state: None,
                        issue: Some(e.to_string()),
                    },
                }
            })
            .collect()
    })
    .await
    {
        Ok(v) => v,
        Err(error) => failed_paths
            .into_iter()
            .map(|task_file| RecentTask {
                task_file,
                destination_exists: false,
                state: None,
                issue: Some(format!("Storage worker failed: {error}")),
            })
            .collect(),
    }
}
pub fn default_destination(settings: &Settings, repo: &RepoId) -> Result<PathBuf, StorageError> {
    let base = settings
        .recent_directories
        .first()
        .cloned()
        .or_else(dirs::download_dir)
        .ok_or_else(|| {
            StorageError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "download directory unavailable",
            ))
        })?;
    Ok(base.join(
        repo.as_str()
            .rsplit('/')
            .next()
            .ok_or(StorageError::CorruptRecord)?,
    ))
}
pub(crate) fn new_record(
    source: RepositorySpec,
    files: Vec<CatalogEntry>,
    options: DownloadOptions,
) -> Result<TaskRecord, StorageError> {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|e| StorageError::Io(io::Error::other(e)))?;
    let id = bytes.iter().map(|v| format!("{v:02x}")).collect();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| StorageError::Io(io::Error::other(e)))?
            .as_millis(),
    )
    .map_err(|e| StorageError::Io(io::Error::other(e)))?;
    let record = TaskRecord {
        schema_version: 1,
        task_id: TaskId(id),
        source,
        files: files
            .into_iter()
            .map(|entry| TaskFile {
                entry,
                completed: false,
                failure: None,
                verification: VerificationStatus::Pending,
                disposition: FileDisposition::Download,
            })
            .collect(),
        options,
        proxy_address: None,
        verify_after_download: Some(true),
        state: TaskState::Paused,
        created_unix_ms: now,
        updated_unix_ms: now,
        replacements: Default::default(),
    };
    validate_record(&record)?;
    Ok(record)
}
fn validate_record(record: &TaskRecord) -> Result<(), StorageError> {
    if !matches!(record.schema_version, 1 | 2) {
        return Err(StorageError::UnsupportedSchema(record.schema_version));
    }
    if record.files.is_empty() || record.updated_unix_ms < record.created_unix_ms {
        return Err(StorageError::CorruptRecord);
    }
    if record.schema_version == 1
        && (!record.replacements.is_empty()
            || record
                .files
                .iter()
                .any(|f| f.disposition != FileDisposition::Download))
    {
        return Err(StorageError::UnsupportedSchema(2));
    }
    let mut names = BTreeSet::new();
    for path in record.replacements.keys() {
        validate_relative(path)?;
        if record.replacements[path]
            .archive_dir
            .as_ref()
            .is_some_and(|dir| {
                dir.is_empty() || !dir.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err(StorageError::CorruptRecord);
        }
        if !record.files.iter().any(|f| f.entry.path.as_str() == path) {
            return Err(StorageError::CorruptRecord);
        }
    }
    for file in &record.files {
        let p = file.entry.path.as_str();
        validate_relative(p)?;
        if !names.insert(p.to_owned()) {
            return Err(StorageError::Conflict(p.into()));
        }
    }
    let mut occupied = BTreeSet::new();
    for name in &names {
        for path in [name.clone(), format!("{name}.aria2")] {
            #[cfg(windows)]
            let path = path.to_lowercase();
            if !occupied.insert(path.clone()) {
                return Err(StorageError::Conflict(path.into()));
            }
        }
    }
    for path in &occupied {
        for (index, _) in path.match_indices('/') {
            if occupied.contains(&path[..index]) {
                return Err(StorageError::Conflict(path.into()));
            }
        }
    }
    Ok(())
}
fn validate_relative(p: &str) -> Result<(), StorageError> {
    if p.is_empty()
        || p.starts_with('/')
        || p.contains(['\\', ':'])
        || p.chars().any(char::is_control)
        || p.split('/')
            .any(|c| c.is_empty() || c == "." || c == ".." || c.eq_ignore_ascii_case(".hfdx"))
    {
        return Err(StorageError::UnsafePath(p.into()));
    }
    #[cfg(windows)]
    for c in p.split('/') {
        let stem = c.split('.').next().unwrap_or("").to_ascii_uppercase();
        if c.ends_with(['.', ' '])
            || c.contains(['<', '>', '"', '|', '?', '*'])
            || matches!(
                stem.as_str(),
                "CON"
                    | "PRN"
                    | "AUX"
                    | "NUL"
                    | "COM1"
                    | "COM2"
                    | "COM3"
                    | "COM4"
                    | "COM5"
                    | "COM6"
                    | "COM7"
                    | "COM8"
                    | "COM9"
                    | "LPT1"
                    | "LPT2"
                    | "LPT3"
                    | "LPT4"
                    | "LPT5"
                    | "LPT6"
                    | "LPT7"
                    | "LPT8"
                    | "LPT9"
            )
        {
            return Err(StorageError::UnsafePath(p.into()));
        }
    }
    Ok(())
}
pub(crate) fn check_path(path: &Path) -> Result<Option<fs::Metadata>, StorageError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(m) => {
                if m.file_type().is_symlink() {
                    return Err(StorageError::UnsafePath(current));
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if m.file_attributes() & 0x400 != 0 {
                        return Err(StorageError::UnsafePath(current));
                    }
                }
                if current != path && !m.is_dir() {
                    return Err(StorageError::Conflict(current));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    }
    match fs::symlink_metadata(path) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub(crate) fn read_record(path: &Path) -> Result<TaskRecord, StorageError> {
    check_path(path)?;
    let bytes = fs::read(path)?;
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| StorageError::CorruptRecord)?;
    let schema = json
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or(StorageError::CorruptRecord)?;
    if !matches!(schema, 1 | 2) {
        return Err(StorageError::UnsupportedSchema(
            u32::try_from(schema).unwrap_or(u32::MAX),
        ));
    }
    let record = serde_json::from_value(json).map_err(|_| StorageError::CorruptRecord)?;
    validate_record(&record)?;
    Ok(record)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResumeAction {
    SkipSizeChecked,
    RetainLocal,
    ResumeControl,
    RestartOwned,
    RestartIntegrityFailed,
    Create,
}
pub fn restart_owned_reason() -> &'static str {
    "The file has no resume control data or its saved size is incorrect; it will be downloaded again."
}
impl ResumeAction {
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::RestartOwned | Self::RestartIntegrityFailed => Some(restart_owned_reason()),
            Self::SkipSizeChecked | Self::RetainLocal | Self::ResumeControl | Self::Create => None,
        }
    }
}
#[derive(Debug)]
pub(crate) struct ResumeItem {
    pub index: usize,
    pub action: ResumeAction,
}
pub(crate) struct PreparedFile {
    directory: PathBuf,
    filename: OsString,
}
impl PreparedFile {
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }
    pub(crate) fn filename(&self) -> &OsStr {
        &self.filename
    }
}
struct LockGuard(File, Option<Arc<crate::logging::SessionLogger>>);
impl Drop for LockGuard {
    fn drop(&mut self) {
        if FileExt::unlock(&self.0).is_err()
            && let Some(logger) = &self.1
        {
            use crate::logging::{EventKind, LogEvent, LogLevel, LogOutcome};
            // The logger retains write failures; unavailable logging is already a UI diagnostic.
            let _result = logger.record(
                LogLevel::Error,
                LogEvent {
                    event: EventKind::LockRelease,
                    task_id: None,
                    outcome: LogOutcome::Disk,
                    count: 0,
                    bytes: 0,
                    elapsed_ms: 0,
                },
            );
        }
    }
}
pub(crate) struct TaskStore {
    destination: PathBuf,
    lock: Arc<LockGuard>,
    identity: TaskRecord,
}
pub(crate) struct OpenedTask {
    pub store: TaskStore,
    pub record: TaskRecord,
}
fn lock_destination(destination: &Path) -> Result<Arc<LockGuard>, StorageError> {
    check_path(destination)?;
    fs::create_dir_all(destination)?;
    check_path(destination)?;
    let meta = destination.join(".hfdx");
    check_path(&meta)?;
    fs::create_dir_all(&meta)?;
    check_path(&meta)?;
    let lock = meta.join("lock");
    check_path(&lock)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock)?;
    f.try_lock_exclusive().map_err(|e| {
        if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
            StorageError::Locked
        } else {
            StorageError::Io(e)
        }
    })?;
    Ok(Arc::new(LockGuard(f, None)))
}
fn control_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".aria2");
    PathBuf::from(name)
}
fn preflight(destination: &Path, record: &TaskRecord, new: bool) -> Result<(), StorageError> {
    validate_record(record)?;
    for file in &record.files {
        let p = queue::file_path(destination, record, &file.entry);
        let meta = check_path(&p)?;
        let c = control_path(&p);
        let control = check_path(&c)?;
        if meta.as_ref().is_some_and(|m| !m.is_file())
            || control.as_ref().is_some_and(|m| !m.is_file())
            || (new && (meta.is_some() || control.is_some()))
        {
            return Err(StorageError::Conflict(p));
        }
    }
    Ok(())
}
impl TaskStore {
    pub(crate) fn set_logger(&mut self, logger: Option<Arc<crate::logging::SessionLogger>>) {
        // Called immediately after create/open, before any blocking operation shares the lock.
        Arc::get_mut(&mut self.lock)
            .expect("new task store exclusively owns lock")
            .1 = logger;
    }
    pub(crate) async fn create(
        destination: &Path,
        record: &TaskRecord,
    ) -> Result<Self, StorageError> {
        let destination = destination.to_path_buf();
        let record = record.clone();
        tokio::task::spawn_blocking(move || {
            validate_record(&record)?;
            let lock = lock_destination(&destination)?;
            let task = destination.join(".hfdx/task.json");
            if check_path(&task)?.is_some() {
                return Err(StorageError::ExistingTask(task));
            }
            preflight(&destination, &record, true)?;
            atomic_write(&task, &record)?;
            Ok(Self {
                destination,
                lock,
                identity: record,
            })
        })
        .await
        .map_err(join_error)?
    }
    pub(crate) async fn open(task_file: &Path) -> Result<OpenedTask, StorageError> {
        let task_file = task_file.to_path_buf();
        tokio::task::spawn_blocking(move || {
            if task_file.file_name() != Some(OsStr::new("task.json"))
                || task_file.parent().and_then(Path::file_name) != Some(OsStr::new(".hfdx"))
            {
                return Err(StorageError::UnsafePath(task_file));
            }
            let destination = task_file
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| StorageError::UnsafePath(task_file.clone()))?
                .to_path_buf();
            check_path(&task_file)?;
            let lock = lock_destination(&destination)?;
            let mut record = read_record(&task_file)?;
            preflight(&destination, &record, false)?;
            if record.state == TaskState::Running {
                record.state = TaskState::Paused;
            }
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
    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }
    pub(crate) fn task_file(&self) -> PathBuf {
        self.destination.join(".hfdx/task.json")
    }
    fn check_identity(&self, record: &TaskRecord) -> Result<(), StorageError> {
        validate_record(record)?;
        if self.identity.task_id != record.task_id
            || self.identity.source != record.source
            || self.identity.options != record.options
            || self.identity.verify_after_download != record.verify_after_download
            || self.identity.files.len() != record.files.len()
            || self
                .identity
                .files
                .iter()
                .zip(&record.files)
                .any(|(a, b)| a.entry != b.entry)
        {
            return Err(StorageError::CorruptRecord);
        }
        Ok(())
    }
    pub(crate) async fn persist(&mut self, record: &TaskRecord) -> Result<(), StorageError> {
        self.check_identity(record)?;
        let path = self.task_file();
        let record = record.clone();
        let lock = Arc::clone(&self.lock);
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            check_path(&path)?;
            atomic_write(&path, &record)
        })
        .await
        .map_err(join_error)?
    }
    pub(crate) async fn enable_verification_for_legacy(
        &mut self,
        record: &mut TaskRecord,
    ) -> Result<(), StorageError> {
        self.check_identity(record)?;
        if record.verify_after_download.is_some()
            || !matches!(record.state, TaskState::Paused | TaskState::Completed)
        {
            return Err(StorageError::CorruptRecord);
        }
        let path = self.task_file();
        let original = record.clone();
        let mut upgraded = original.clone();
        upgraded.verify_after_download = Some(true);
        let saved = upgraded.clone();
        let lock = Arc::clone(&self.lock);
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            let mut disk = read_record(&path)?;
            if disk.state == TaskState::Running {
                disk.state = TaskState::Paused;
            }
            if disk != original {
                return Err(StorageError::CorruptRecord);
            }
            atomic_write(&path, &saved)
        })
        .await
        .map_err(join_error)??;
        self.identity.verify_after_download = Some(true);
        *record = upgraded;
        Ok(())
    }
    pub(crate) async fn inspect(
        &self,
        record: &TaskRecord,
    ) -> Result<Vec<ResumeItem>, StorageError> {
        self.check_identity(record)?;
        let dest = self.destination.clone();
        let record = record.clone();
        let lock = Arc::clone(&self.lock);
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            preflight(&dest, &record, false)?;
            record
                .files
                .iter()
                .enumerate()
                .map(|(index, f)| {
                    let path = queue::file_path(&dest, &record, &f.entry);
                    let data = check_path(&path)?;
                    let control = check_path(&control_path(&path))?;
                    let action = if matches!(
                        f.disposition,
                        FileDisposition::KeptDifferent
                            | FileDisposition::TimeUnknown
                            | FileDisposition::Unchecked
                    ) {
                        ResumeAction::RetainLocal
                    } else if f.verification == VerificationStatus::Failed {
                        ResumeAction::RestartIntegrityFailed
                    } else if control.is_some() {
                        ResumeAction::ResumeControl
                    } else if let Some(meta) = data {
                        if f.completed && meta.len() == f.entry.size.get() {
                            ResumeAction::SkipSizeChecked
                        } else {
                            ResumeAction::RestartOwned
                        }
                    } else {
                        ResumeAction::Create
                    };
                    Ok(ResumeItem { index, action })
                })
                .collect()
        })
        .await
        .map_err(join_error)?
    }
    pub(crate) async fn prepare(
        &mut self,
        entry: &CatalogEntry,
        action: ResumeAction,
    ) -> Result<PreparedFile, StorageError> {
        if !self.identity.files.iter().any(|f| f.entry == *entry) {
            return Err(StorageError::Conflict(entry.path.as_str().into()));
        }
        let dest = self.destination.clone();
        let entry = entry.clone();
        let task_id = self.identity.task_id.clone();
        let physical = queue::file_path(&dest, &self.identity, &entry);
        let lock = Arc::clone(&self.lock);
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            validate_relative(entry.path.as_str())?;
            let path = physical;
            let control = control_path(&path);
            let data = check_path(&path)?;
            let ctl = check_path(&control)?;
            if data.as_ref().is_some_and(|m| !m.is_file())
                || ctl.as_ref().is_some_and(|m| !m.is_file())
            {
                return Err(StorageError::Conflict(path));
            }
            match action {
                ResumeAction::RestartOwned if ctl.is_none() => {
                    let record = read_record(&dest.join(".hfdx/task.json"))?;
                    if record.task_id != task_id {
                        return Err(StorageError::Conflict(path));
                    }
                    let owned = record
                        .files
                        .iter()
                        .find(|file| file.entry == entry)
                        .ok_or_else(|| StorageError::Conflict(path.clone()))?;
                    if owned.verification == VerificationStatus::Failed
                        || (owned.completed
                            && data.as_ref().is_some_and(|m| m.len() == entry.size.get()))
                    {
                        return Err(StorageError::Conflict(path));
                    }
                    if data.is_some() {
                        if check_path(&control)?.is_some() {
                            return Err(StorageError::Conflict(control));
                        }
                        check_path(&path)?;
                        fs::remove_file(&path)?;
                    }
                }
                ResumeAction::RestartIntegrityFailed if ctl.is_none() => {
                    let record = read_record(&dest.join(".hfdx/task.json"))?;
                    let owned = record.files.iter().find(|file| file.entry == entry);
                    if record.task_id != task_id
                        || !owned.is_some_and(|file| {
                            file.completed
                                && file.verification == VerificationStatus::Failed
                                && file
                                    .failure
                                    .as_ref()
                                    .is_some_and(|failure| failure.kind == FailureKind::Integrity)
                        })
                        || data
                            .as_ref()
                            .is_some_and(|meta| meta.len() != entry.size.get())
                    {
                        return Err(StorageError::Conflict(path));
                    }
                    if data.is_some() {
                        if check_path(&control)?.is_some() {
                            return Err(StorageError::Conflict(control));
                        }
                        check_path(&path)?;
                        fs::remove_file(&path)?;
                    }
                }
                ResumeAction::Create if data.is_none() && ctl.is_none() => {}
                ResumeAction::ResumeControl if ctl.is_some() => {}
                ResumeAction::SkipSizeChecked
                    if ctl.is_none()
                        && data.as_ref().is_some_and(|m| m.len() == entry.size.get()) => {}
                _ => return Err(StorageError::Conflict(path)),
            };
            let directory = path
                .parent()
                .ok_or_else(|| StorageError::UnsafePath(path.clone()))?
                .to_path_buf();
            check_path(&directory)?;
            fs::create_dir_all(&directory)?;
            check_path(&directory)?;
            let filename = path
                .file_name()
                .ok_or_else(|| StorageError::UnsafePath(path.clone()))?
                .to_os_string();
            Ok(PreparedFile {
                directory,
                filename,
            })
        })
        .await
        .map_err(join_error)?
    }
    pub(crate) async fn verify_size(&self, entry: &CatalogEntry) -> Result<(), StorageError> {
        if !self.identity.files.iter().any(|f| f.entry == *entry) {
            return Err(StorageError::Conflict(entry.path.as_str().into()));
        }
        let path = queue::file_path(&self.destination, &self.identity, entry);
        let size = entry.size.get();
        let lock = Arc::clone(&self.lock);
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            let m = check_path(&path)?.ok_or(StorageError::SizeMismatch)?;
            if !m.is_file() || m.len() != size {
                return Err(StorageError::SizeMismatch);
            }
            Ok(())
        })
        .await
        .map_err(join_error)?
    }
    pub(crate) async fn verification_path(
        &self,
        entry: &CatalogEntry,
    ) -> Result<PathBuf, StorageError> {
        if !self.identity.files.iter().any(|file| file.entry == *entry) {
            return Err(StorageError::Conflict(entry.path.as_str().into()));
        }
        let path = queue::file_path(&self.destination, &self.identity, entry);
        let size = entry.size.get();
        let lock = Arc::clone(&self.lock);
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            let control = control_path(&path);
            if check_path(&control)?.is_some() {
                return Err(StorageError::Conflict(control));
            }
            let metadata = check_path(&path)?.ok_or(StorageError::SizeMismatch)?;
            if !metadata.is_file() || metadata.len() != size {
                return Err(StorageError::SizeMismatch);
            }
            Ok(path)
        })
        .await
        .map_err(join_error)?
    }
    pub(crate) async fn create_empty(&mut self, file: PreparedFile) -> Result<(), StorageError> {
        let path = file.directory.join(file.filename);
        if !self.identity.files.iter().any(|f| {
            f.entry.size.get() == 0
                && queue::file_path(&self.destination, &self.identity, &f.entry) == path
        }) {
            return Err(StorageError::Conflict(path));
        }
        let lock = Arc::clone(&self.lock);
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            if check_path(&control_path(&path))?.is_some() {
                return Err(StorageError::Conflict(path));
            }
            check_path(&path)?;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?
                .sync_all()?;
            Ok(())
        })
        .await
        .map_err(join_error)?
    }
}
