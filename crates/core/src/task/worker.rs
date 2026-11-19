// allow: SIZE_OK - Worker lifecycle stays in the sole authorized worker file.
use super::*;
#[path = "queue_worker.rs"]
mod queue_worker;
#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod snapshot_tests;
use crate::{
    aria2::{Aria2, Aria2Launch, DownloadJob, EngineState, Gid, ProbeError, StopKind},
    logging::{EventKind, LogEvent, LogLevel, LogMetrics, LogOutcome, LogStatus, SessionLogger},
    repository::{
        CatalogId, CatalogPage, NetworkAccess, RepositoryClient, RepositoryError, RepositorySpec,
    },
    selection::{CatalogState, Selection},
    storage::{
        self, FailureKind, FileFailure, ResumeAction, Settings, StorageError, TaskRecord,
        TaskState, TaskStore, VerificationStatus,
    },
};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

const STATUS_OUTAGE_LIMIT: Duration = Duration::from_secs(60);

struct RunningTask {
    store: TaskStore,
    record: TaskRecord,
    engine: Option<Aria2>,
    jobs: BTreeMap<Gid, usize>,
    slots: BTreeMap<usize, super::budget::Slot>,
    pending: VecDeque<(usize, ResumeAction)>,
    active: Vec<ActiveFile>,
    restart_notice: Option<String>,
    cached_record: Arc<TaskRecord>,
    cached_progress: TaskProgress,
    verification: Option<VerificationProgress>,
}
pub(super) fn access_defaults(
    mut access: NetworkAccess,
    settings: &Settings,
    endpoint: &crate::Endpoint,
) -> Result<NetworkAccess, CoreError> {
    if access.token.is_none() {
        access.token = settings.token_for(endpoint);
        if access.token.is_none() && endpoint.url().as_str() == "https://huggingface.co/" {
            access.token = std::env::var("HF_TOKEN")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| crate::repository::Token::parse(&s))
                .transpose()?;
        }
    }
    if access.proxy.is_none() {
        let proxy = settings
            .proxy_address
            .as_ref()
            .map(|address| address.url().to_string())
            .or_else(|| {
                std::env::var("https_proxy")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| {
                std::env::var("HTTPS_PROXY")
                    .ok()
                    .filter(|value| !value.is_empty())
            });
        if let Some(proxy) = proxy {
            access.proxy = Some(crate::repository::ProxySpec::parse(&proxy)?);
        }
    }
    Ok(access)
}
pub(super) fn refresh_saved_token(
    access: &mut NetworkAccess,
    endpoint: &crate::Endpoint,
    previous: &Settings,
    current: &Settings,
) {
    if access.token.is_none()
        || access.token.as_ref().map(crate::Token::expose)
            == previous
                .token_for(endpoint)
                .as_ref()
                .map(crate::Token::expose)
    {
        access.token = current.token_for(endpoint);
    }
}
fn empty_progress() -> TaskProgress {
    TaskProgress {
        completed_files: 0,
        total_files: 0,
        acquired_bytes: crate::repository::Bytes::new(0),
        total_bytes: crate::repository::Bytes::new(0),
        bytes_per_second: 0,
        eta: None,
        active: Arc::from([]),
        failed: Arc::from([]),
    }
}
impl RunningTask {
    async fn persist(&mut self) -> Result<(), StorageError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| StorageError::Io(std::io::Error::other(error)))?;
        let millis = u64::try_from(now.as_millis())
            .map_err(|error| StorageError::Io(std::io::Error::other(error)))?;
        self.record.updated_unix_ms = self.record.updated_unix_ms.max(millis);
        self.store.persist(&self.record).await
    }
    fn refresh(&mut self) {
        let mut progress = empty_progress();
        let mut completed_bytes = 0u64;
        let mut total = 0u64;
        let mut failed = Vec::new();
        for (index, file) in self.record.files.iter().enumerate() {
            total = total.saturating_add(file.entry.size.get());
            if file.completed {
                progress.completed_files += 1;
                completed_bytes = completed_bytes.saturating_add(file.entry.size.get());
            }
            if let Some(error) = &file.failure {
                failed.push((index, error.clone()));
            }
        }
        progress.total_files = self.record.files.len();
        progress.total_bytes = crate::repository::Bytes::new(total);
        progress.acquired_bytes = crate::repository::Bytes::new(completed_bytes);
        progress.failed = Arc::from(failed);
        self.cached_record = Arc::new(self.record.clone());
        self.cached_progress = progress;
    }
}
enum LoadEvent {
    Resolved(RepositorySpec, oneshot::Sender<CatalogId>),
    Page(CatalogPage),
    Finished(Result<(), RepositoryError>),
}
struct VerificationRun {
    index: usize,
    _slot: Option<super::budget::Slot>,
    started: Instant,
    algorithm: Option<LogOutcome>,
    result: JoinHandle<std::io::Result<crate::integrity::IntegrityResult>>,
}
struct Worker {
    budget: Option<Arc<super::budget::Budget>>,
    config: CoreConfig,
    settings: Settings,
    recent: Arc<[storage::RecentTask]>,
    selection: Selection,
    phase: UiPhase,
    task: Option<RunningTask>,
    access: NetworkAccess,
    access_origin: Option<url::Origin>,
    snapshots: watch::Sender<Arc<AppSnapshot>>,
    revision: u64,
    error: Option<UiError>,
    load_cancel: CancellationToken,
    load: Option<JoinHandle<()>>,
    load_events: mpsc::Receiver<LoadEvent>,
    load_sender: mpsc::Sender<LoadEvent>,
    shutdown: CancellationToken,
    run_cancel: CancellationToken,
    status_unavailable_since: Option<Instant>,
    verification: Option<VerificationRun>,
    verification_bytes: watch::Receiver<u64>,
    completion: CompletionCallback,
    notice_epoch: u64,
    logger: Option<Arc<SessionLogger>>,
    logging_status: Option<LogStatus>,
    published_phase: UiPhase,
    #[cfg(test)]
    verification_hook: Option<Arc<dyn Fn(u64) + Send + Sync>>,
}
fn engine_error(operation: &'static str, error: ProbeError) -> CoreError {
    let reason = match error {
        ProbeError::UnsupportedProxy => EngineFailure::UnsupportedProxy,
        ProbeError::Io(_) => EngineFailure::Io,
        ProbeError::Protocol => EngineFailure::Protocol,
        ProbeError::Connection => EngineFailure::Connection,
        ProbeError::Response(part) => EngineFailure::Response(part),
        ProbeError::Exited(code) => EngineFailure::Exited(code),
        ProbeError::Rpc(code) => EngineFailure::Rpc(code),
        ProbeError::Timeout => EngineFailure::Timeout,
        ProbeError::Shutdown(_) => EngineFailure::Shutdown,
    };
    CoreError::Engine(Box::new(UiError {
        code: ErrorCode::Engine,
        message: format!("Download engine {operation} failed"),
        hint: format!("aria2: {reason:?}"),
        detail: Some(ErrorDetail::Engine { operation, reason }),
    }))
}
pub(super) struct Services {
    pub budget: Option<Arc<super::budget::Budget>>,
    pub completion: CompletionCallback,
    pub logger: Option<Arc<SessionLogger>>,
}
pub(super) async fn run(
    config: CoreConfig,
    settings: Settings,
    selection: Selection,
    snapshots: watch::Sender<Arc<AppSnapshot>>,
    mut commands: mpsc::Receiver<Envelope>,
    shutdown: CancellationToken,
    services: Services,
) -> Result<ShutdownReceipt, CoreError> {
    let Services {
        completion,
        logger,
        budget,
    } = services;
    let (load_sender, load_events) = mpsc::channel(2);
    let recent = Arc::from(storage::recent_tasks(&settings.recent_tasks).await);
    let mut worker = Worker {
        budget,
        config,
        settings,
        recent,
        selection,
        phase: UiPhase::Idle,
        task: None,
        access: NetworkAccess::default(),
        access_origin: None,
        snapshots,
        revision: 0,
        error: None,
        load_cancel: CancellationToken::new(),
        load: None,
        load_events,
        load_sender,
        run_cancel: shutdown.child_token(),
        status_unavailable_since: None,
        shutdown,
        verification: None,
        verification_bytes: watch::channel(0).1,
        completion,
        notice_epoch: 0,
        logger,
        logging_status: None,
        published_phase: UiPhase::Idle,
        #[cfg(test)]
        verification_hook: None,
    };
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { biased;
            () = worker.shutdown.cancelled() => break,
            event = worker.load_events.recv(), if worker.load.is_some() => if let Some(event) = event { worker.loaded(event).await; },
            message = commands.recv() => match message {
                Some(message) => {
                    let task_changes = matches!(&message.command, CoreCommand::Start(_) | CoreCommand::StartPrepared(_) | CoreCommand::AppendPrepared(_) | CoreCommand::OpenTask(_) | CoreCommand::Pause | CoreCommand::Resume | CoreCommand::RetryFailed | CoreCommand::VerifyExisting);
                    let recent_changes = matches!(&message.command, CoreCommand::Start(_) | CoreCommand::SaveSettings(_) | CoreCommand::ForgetRecent(_));
                    let was_active = matches!(worker.phase, UiPhase::Downloading | UiPhase::Verifying);
                    if !was_active && let Some(cancel) = message.run_cancel { worker.run_cancel = cancel; }
                    let result = worker.command(message.command).await;
                    if let Err(error) = &result { worker.event(LogLevel::Warn, (EventKind::TaskFailed, error_outcome(error.ui_error().code))); worker.error = Some(error.ui_error()); if task_changes && !was_active && matches!(worker.phase, UiPhase::Downloading | UiPhase::Preparing | UiPhase::Verifying) { worker.fail_running().await; } }
                    if result.is_ok() && task_changes && worker.phase != UiPhase::Verifying && let Some(task) = worker.task.as_mut() { task.refresh(); }
                    if recent_changes { worker.recent = Arc::from(storage::recent_tasks(&worker.settings.recent_tasks).await); }
                    worker.publish().await;
                    let _ignored = message.reply.send(result.map(|()| CommandReceipt { snapshot_revision: worker.revision }));
                },
                None => break,
            },
            result = async { match worker.verification.as_mut() { Some(run) => (&mut run.result).await, None => std::future::pending().await } }, if worker.verification.is_some() => {
                if let Err(error) = worker.verified(result).await { worker.event(LogLevel::Error, (EventKind::TaskFailed, error_outcome(error.ui_error().code))); worker.error = Some(error.ui_error()); worker.fail_running().await; }
                worker.publish().await;
            },
            changed = worker.verification_bytes.changed(), if worker.verification.is_some() => {
                if changed.is_ok() {
                    let done = *worker.verification_bytes.borrow_and_update();
                    if let Some(task) = worker.task.as_mut() && let Some(progress) = task.verification.as_mut() { progress.file_bytes_done = done; }
                    worker.publish().await;
                }
            },
            _ = interval.tick(), if worker.task.as_ref().is_some_and(|task| task.engine.is_some() || (worker.budget.is_some() && task.record.state == TaskState::Running)) && matches!(worker.phase, UiPhase::Downloading | UiPhase::Verifying) => {
                if let Err(error) = worker.tick().await {
                    let error = error.ui_error();
                    let metrics = match error.detail {
                        Some(ErrorDetail::Engine { operation, reason }) => LogMetrics {
                            engine_operation: Some(operation),
                            engine_failure: Some(reason),
                            ..LogMetrics::default()
                        },
                        _ => LogMetrics::default(),
                    };
                    worker.measured_event(LogLevel::Error, (EventKind::TaskFailed, error_outcome(error.code)), metrics, 0, None);
                    worker.error = Some(error);
                    worker.fail_running().await;
                }
                worker.publish().await;
            }
        }
    }
    worker.event(
        LogLevel::Debug,
        (EventKind::ExitCancellation, LogOutcome::Cancelled),
    );
    worker.phase = UiPhase::Stopping;
    let verification_result = worker.join_verification().await;
    worker.load_cancel.cancel();
    let load_result = match worker.load.take() {
        Some(load) => load.await.map_err(|_| CoreError::WorkerStopped),
        None => Ok(()),
    };
    worker.publish().await;
    let receipt = worker.stop_engine().await;
    if let Some(task) = worker.task.as_mut() {
        if task.record.state == TaskState::Running {
            task.record.state = TaskState::Paused;
        }
        task.persist().await?;
        task.refresh();
    }
    worker.phase = UiPhase::Closed;
    let outcome = if receipt.is_ok() && load_result.is_ok() && verification_result.is_ok() {
        LogOutcome::Succeeded
    } else {
        LogOutcome::Internal
    };
    worker.event(LogLevel::Info, (EventKind::Shutdown, outcome));
    worker.publish().await;
    load_result?;
    verification_result?;
    receipt
}
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
fn error_outcome(code: ErrorCode) -> LogOutcome {
    match code {
        ErrorCode::Authentication => LogOutcome::Authentication,
        ErrorCode::Network | ErrorCode::NotFound | ErrorCode::UnsupportedEndpoint => {
            LogOutcome::Network
        }
        ErrorCode::RateLimited => LogOutcome::RateLimited,
        ErrorCode::Disk
        | ErrorCode::UnsafePath
        | ErrorCode::DestinationConflict
        | ErrorCode::Locked
        | ErrorCode::CorruptRecord
        | ErrorCode::UnsupportedSchema => LogOutcome::Disk,
        ErrorCode::Engine => LogOutcome::Engine,
        ErrorCode::Input | ErrorCode::InvalidState => LogOutcome::InvalidState,
        ErrorCode::Cancelled => LogOutcome::Cancelled,
        ErrorCode::Internal => LogOutcome::Internal,
    }
}
impl Worker {
    fn event(&mut self, level: LogLevel, event: (EventKind, LogOutcome)) {
        self.measured_event(level, event, LogMetrics::default(), 0, None);
    }
    fn measured_event(
        &mut self,
        level: LogLevel,
        (event, outcome): (EventKind, LogOutcome),
        metrics: LogMetrics,
        elapsed_ms: u64,
        bytes: Option<u64>,
    ) {
        if let Some(logger) = &self.logger {
            let task = self.task.as_ref();
            let result = logger.record_with_metrics(
                level,
                LogEvent {
                    event,
                    outcome,
                    task_id: task.map(|task| &task.record.task_id),
                    count: match event {
                        EventKind::Metadata | EventKind::Pagination => {
                            self.selection.view().catalog.as_ref().map_or(0, |catalog| {
                                u64::try_from(catalog.len()).unwrap_or(u64::MAX)
                            })
                        }
                        _ => task.map_or(0, |task| {
                            u64::try_from(task.record.files.len()).unwrap_or(u64::MAX)
                        }),
                    },
                    bytes: bytes.unwrap_or_else(|| match event {
                        EventKind::Metadata | EventKind::Pagination | EventKind::Filter => 0,
                        _ => task.map_or(0, |task| task.cached_progress.acquired_bytes.get()),
                    }),
                    elapsed_ms,
                },
                metrics,
            );
            self.logging_status = Some(match result {
                Ok(()) => logger.status(),
                Err(error) => LogStatus::Failed(error),
            });
        }
    }
    fn filter_event(&mut self, started: Instant) {
        let view = self.selection.view();
        self.measured_event(
            LogLevel::Debug,
            (EventKind::Filter, LogOutcome::Succeeded),
            LogMetrics {
                input_bytes: Some(view.filter.len()),
                matched: Some(view.matches.len()),
                visible: Some(view.visible.len()),
                selected: Some(view.selected.len()),
                ..LogMetrics::default()
            },
            elapsed_ms(started),
            None,
        );
    }
    async fn publish(&mut self) {
        if self.published_phase != self.phase {
            let outcome = match self.phase {
                UiPhase::Idle | UiPhase::Browsing => LogOutcome::Succeeded,
                UiPhase::Loading | UiPhase::Preparing => LogOutcome::Started,
                UiPhase::Downloading => LogOutcome::Downloading,
                UiPhase::Verifying => LogOutcome::Verifying,
                UiPhase::Paused => LogOutcome::Paused,
                UiPhase::Stopping => LogOutcome::Cancelled,
                UiPhase::Closed => LogOutcome::Closed,
            };
            self.event(LogLevel::Info, (EventKind::PhaseChanged, outcome));
            self.published_phase = self.phase;
        }
        self.revision = self.revision.saturating_add(1);
        let task = self.task.as_ref().map(|t| {
            let mut total = 0u64;
            let mut acquired = 0u64;
            let mut completed = 0usize;
            total = total.saturating_add(t.cached_progress.total_bytes.get());
            acquired = acquired.saturating_add(t.cached_progress.acquired_bytes.get());
            completed += t.cached_progress.completed_files;
            let speed = t
                .active
                .iter()
                .fold(0u64, |a, f| a.saturating_add(f.bytes_per_second));
            acquired = t
                .active
                .iter()
                .fold(acquired, |a, f| a.saturating_add(f.completed_bytes.get()));
            let eta = total
                .saturating_sub(acquired)
                .checked_div(speed)
                .map(Duration::from_secs);
            TaskView {
                record: t.cached_record.clone(),
                destination: t.store.destination().to_path_buf(),
                restart_notice: t.restart_notice.clone(),
                verification: t.verification,
                progress: TaskProgress {
                    completed_files: completed,
                    total_files: t.record.files.len(),
                    acquired_bytes: crate::repository::Bytes::new(acquired),
                    total_bytes: crate::repository::Bytes::new(total),
                    bytes_per_second: speed,
                    eta,
                    active: Arc::from(t.active.clone()),
                    failed: t.cached_progress.failed.clone(),
                },
            }
        });
        self.snapshots.send_replace(Arc::new(AppSnapshot {
            revision: self.revision,
            phase: self.phase,
            selection: self.selection.view(),
            task,
            settings: Arc::new(self.settings.clone()),
            recent: self.recent.clone(),
            error: self.error.clone(),
            queue: None,
            diagnostics: Diagnostics {
                current_log: self.logger.as_ref().map(|l| l.current_log()),
                log_directory: self
                    .logger
                    .as_ref()
                    .map(|logger| logger.directory().to_path_buf()),
                logging: match self.logging_status {
                    Some(LogStatus::Failed(error)) => Some(LogStatus::Failed(error)),
                    _ => self.logger.as_ref().map(|logger| logger.status()),
                },
            },
        }));
    }
    async fn command(&mut self, command: CoreCommand) -> Result<(), CoreError> {
        self.error = None;
        let frozen_access = matches!(&command, CoreCommand::LoadFrozen(_));
        match command {
            CoreCommand::Load(request) | CoreCommand::LoadFrozen(request) => {
                if matches!(self.phase, UiPhase::Downloading | UiPhase::Verifying) {
                    return Err(CoreError::InvalidState { action: "load" });
                }
                self.cancel_load().await?;
                // Draft catalog changes do not replace the current task.
                let access = if frozen_access {
                    request.access
                } else {
                    access_defaults(request.access, &self.settings, &request.repository.endpoint)?
                };
                let mut client =
                    RepositoryClient::new(&request.repository.endpoint, access.clone())?;
                client.set_logger(self.logger.clone());
                self.access_origin = Some(request.repository.endpoint.url().origin());
                self.access = access;
                self.selection.reset();
                self.phase = UiPhase::Loading;
                self.load_cancel = self.shutdown.child_token();
                let cancel = self.load_cancel.clone();
                let sender = self.load_sender.clone();
                self.load = Some(tokio::spawn(async move {
                    let result = async {
                        let source = client.resolve(&request.repository, &cancel).await?;
                        let (id_tx, id_rx) = oneshot::channel();
                        tokio::select! { () = cancel.cancelled() => return Err(RepositoryError::Cancelled), result = sender.send(LoadEvent::Resolved(source.clone(), id_tx)) => result.map_err(|_| RepositoryError::Cancelled)? }
                        let (tx, mut rx) = mpsc::channel(2);
                        let id = tokio::select! { () = cancel.cancelled() => return Err(RepositoryError::Cancelled), value = id_rx => value.map_err(|_| RepositoryError::Cancelled)? };
                        let catalog_request = crate::repository::CatalogRequest { id, source };
                        let catalog = client.catalog(&catalog_request, tx, &cancel);
                        tokio::pin!(catalog);
                        loop { tokio::select! { biased; () = cancel.cancelled() => return Err(RepositoryError::Cancelled), page = rx.recv() => if let Some(page) = page { tokio::select! { () = cancel.cancelled() => return Err(RepositoryError::Cancelled), result = sender.send(LoadEvent::Page(page)) => result.map_err(|_| RepositoryError::Cancelled)? } }, result = &mut catalog => { while let Ok(page) = rx.try_recv() { tokio::select! { () = cancel.cancelled() => return Err(RepositoryError::Cancelled), sent = sender.send(LoadEvent::Page(page)) => sent.map_err(|_| RepositoryError::Cancelled)? } } return result; } } }
                    }.await;
                    tokio::select! { () = cancel.cancelled() => {}, _delivery = sender.send(LoadEvent::Finished(result)) => {} }
                }));
                Ok(())
            }
            CoreCommand::CancelLoad => {
                if self.phase != UiPhase::Loading {
                    return Err(CoreError::InvalidState {
                        action: "cancel load",
                    });
                }
                self.cancel_load().await?;
                self.selection.reset();
                self.phase = UiPhase::Idle;
                Ok(())
            }
            CoreCommand::NewDownload => {
                if matches!(
                    self.phase,
                    UiPhase::Loading | UiPhase::Downloading | UiPhase::Verifying
                ) {
                    return Err(CoreError::InvalidState {
                        action: "new download",
                    });
                }
                self.cancel_load().await?;
                // A paused task stays on disk; the draft must not keep its memory state.
                self.task = None;
                self.selection.reset();
                self.phase = UiPhase::Idle;
                Ok(())
            }
            CoreCommand::SetFilter(value) => {
                let started = Instant::now();
                self.selection.set_filter(&value)?;
                self.filter_event(started);
                Ok(())
            }
            CoreCommand::SetSelectedOnly(value) => {
                let started = Instant::now();
                self.selection.set_selected_only(value);
                self.filter_event(started);
                Ok(())
            }
            CoreCommand::ChangeProfile {
                catalog,
                index,
                selected,
            } => {
                let view = self.selection.view();
                let current = view
                    .catalog
                    .as_ref()
                    .filter(|c| c.id == catalog)
                    .ok_or(crate::selection::SelectionError::StaleEntry)?;
                let change = crate::profiles::change(current, index, selected, &view.selected)
                    .ok_or(crate::selection::SelectionError::StaleEntry)?;
                self.selection.change(change)?;
                Ok(())
            }
            CoreCommand::ChangeSelection(change) => {
                let started = Instant::now();
                self.selection.change(change)?;
                self.filter_event(started);
                Ok(())
            }
            CoreCommand::Start(request) => self.start(request).await,
            CoreCommand::StartPrepared(prepared) => self.start_prepared(*prepared).await,
            CoreCommand::AppendPrepared(prepared) => self.append_prepared(prepared).await,
            CoreCommand::OpenTask(path) => {
                if matches!(self.phase, UiPhase::Downloading | UiPhase::Verifying) {
                    return Err(CoreError::InvalidState { action: "open" });
                }
                self.cancel_load().await?;
                self.stop_engine().await?;
                let mut opened = TaskStore::open(&path).await?;
                opened.store.set_logger(self.logger.clone());
                let proxy = opened
                    .record
                    .proxy_address
                    .as_ref()
                    .map(|address| crate::repository::ProxySpec::parse(address.url().as_str()))
                    .transpose()?;
                self.access = if self.budget.is_some() {
                    NetworkAccess { token: None, proxy }
                } else {
                    access_defaults(
                        NetworkAccess { token: None, proxy },
                        &self.settings,
                        &opened.record.source.request.endpoint,
                    )?
                };
                if self.access.token.is_none() {
                    self.access.token = self
                        .settings
                        .token_for(&opened.record.source.request.endpoint);
                }
                self.access_origin = None;
                self.task = Some(RunningTask {
                    store: opened.store,
                    record: opened.record.clone(),
                    engine: None,
                    jobs: BTreeMap::new(),
                    slots: BTreeMap::new(),
                    pending: VecDeque::new(),
                    active: Vec::new(),
                    restart_notice: None,
                    cached_record: Arc::new(opened.record.clone()),
                    cached_progress: empty_progress(),
                    verification: None,
                });
                self.phase = UiPhase::Paused;
                Ok(())
            }
            CoreCommand::SetAccess { endpoint, access } => {
                if matches!(self.phase, UiPhase::Downloading | UiPhase::Verifying) {
                    return Err(CoreError::InvalidState {
                        action: "set access",
                    });
                }
                let task = self.task.as_ref().ok_or(CoreError::InvalidState {
                    action: "set access without task",
                })?;
                let origin = endpoint.url().origin();
                if origin != task.record.source.request.endpoint.url().origin() {
                    return Err(CoreError::InvalidState {
                        action: "credentials belong to another endpoint",
                    });
                }
                let mut access = access;
                if access.proxy.is_none() {
                    access.proxy = task
                        .record
                        .proxy_address
                        .as_ref()
                        .map(|address| crate::repository::ProxySpec::parse(address.url().as_str()))
                        .transpose()?;
                }
                self.access = access_defaults(access, &self.settings, &endpoint)?;
                self.access_origin = Some(origin);
                Ok(())
            }
            CoreCommand::Pause => self.pause().await,
            CoreCommand::Resume => self.resume(false).await,
            CoreCommand::RetryFailed => self.resume(true).await,
            CoreCommand::VerifyExisting => self.verify_existing().await,
            CoreCommand::SavePlatformTokens { hf, modelscope } => {
                let mut settings = self.settings.clone();
                settings.hf_token = hf;
                settings.modelscope_token = modelscope;
                storage::save_settings(&self.config.paths, &settings).await?;
                self.settings = settings;
                self.event(
                    LogLevel::Info,
                    (EventKind::SettingsChanged, LogOutcome::Succeeded),
                );
                Ok(())
            }
            CoreCommand::SaveSettings(settings) => {
                storage::save_settings(&self.config.paths, &settings).await?;
                if let Some(logger) = &self.logger {
                    logger.set_debug(settings.debug_logging);
                }
                self.settings = settings;
                self.event(
                    LogLevel::Info,
                    (EventKind::SettingsChanged, LogOutcome::Succeeded),
                );
                Ok(())
            }
            CoreCommand::ForgetRecent(path) => {
                let mut settings = self.settings.clone();
                settings.recent_tasks.retain(|p| p != &path);
                storage::save_settings(&self.config.paths, &settings).await?;
                self.settings = settings;
                Ok(())
            }
            _ => Err(CoreError::InvalidState {
                action: "queue command",
            }),
        }
    }
    async fn cancel_load(&mut self) -> Result<(), CoreError> {
        self.load_cancel.cancel();
        if let Some(load) = self.load.take() {
            load.await.map_err(|_| CoreError::WorkerStopped)?;
        }
        while self.load_events.try_recv().is_ok() {}
        Ok(())
    }
    async fn loaded(&mut self, event: LoadEvent) {
        match &event {
            LoadEvent::Resolved(_, _) => {
                self.event(LogLevel::Debug, (EventKind::Metadata, LogOutcome::Started))
            }
            LoadEvent::Page(..) => {}
            LoadEvent::Finished(result) => self.event(
                if result.is_ok() {
                    LogLevel::Info
                } else {
                    LogLevel::Warn
                },
                (
                    EventKind::Metadata,
                    match result {
                        Ok(()) => LogOutcome::Succeeded,
                        Err(error) => match error {
                            RepositoryError::Http {
                                status: 401 | 403, ..
                            } => LogOutcome::Authentication,
                            RepositoryError::Http { status: 429, .. } => LogOutcome::RateLimited,
                            RepositoryError::Cancelled => LogOutcome::Cancelled,
                            RepositoryError::Input(_) | RepositoryError::UnsafePath => {
                                LogOutcome::InvalidState
                            }
                            RepositoryError::Network
                            | RepositoryError::Http { .. }
                            | RepositoryError::InvalidResponse
                            | RepositoryError::UnsupportedEndpoint => LogOutcome::Network,
                        },
                    },
                ),
            ),
        }
        match event {
            LoadEvent::Resolved(source, reply) => match self.selection.begin(source) {
                Ok(request) => {
                    let _ignored = reply.send(request.id);
                }
                Err(error) => {
                    self.error = Some(CoreError::Selection(error).ui_error());
                    if let Err(error) = self.cancel_load().await {
                        self.error = Some(error.ui_error());
                    }
                    self.phase = UiPhase::Browsing;
                }
            },
            LoadEvent::Page(page) => {
                if let Err(error) = self.selection.append(page) {
                    self.error = Some(CoreError::Selection(error).ui_error());
                    if let Some(catalog) = self.selection.view().catalog {
                        self.selection.fail(catalog.id);
                    }
                    if let Err(error) = self.cancel_load().await {
                        self.error = Some(error.ui_error());
                    }
                    self.phase = UiPhase::Browsing;
                }
            }
            LoadEvent::Finished(result) => {
                if let Err(error) = result {
                    if let Some(catalog) = self.selection.view().catalog {
                        self.selection.fail(catalog.id);
                    }
                    self.error = Some(CoreError::Repository(error).ui_error());
                }
                self.phase = UiPhase::Browsing;
                if let Some(load) = self.load.take()
                    && load.await.is_err()
                {
                    self.error = Some(CoreError::WorkerStopped.ui_error());
                }
            }
        }
        self.publish().await;
    }
    async fn start(&mut self, request: StartRequest) -> Result<(), CoreError> {
        if matches!(self.phase, UiPhase::Downloading | UiPhase::Verifying)
            || self.selection.view().state != CatalogState::Ready
        {
            return Err(CoreError::InvalidState { action: "start" });
        }
        let view = self.selection.view();
        if view.catalog.as_ref().map(|c| c.id) != Some(request.catalog) {
            return Err(CoreError::InvalidState {
                action: "stale catalog",
            });
        }
        let frozen = self.selection.freeze()?;
        self.cancel_load().await?;
        self.stop_engine().await?;
        let mut record = storage::new_record(frozen.source, frozen.files, request.options)?;
        record.proxy_address = self.access.proxy.as_ref().map(|p| p.address());
        record.verify_after_download = Some(request.verify_after_download);
        let mut store = TaskStore::create(&request.destination, &record).await?;
        store.set_logger(self.logger.clone());
        let path = store.task_file();
        self.task = Some(RunningTask {
            store,
            record: record.clone(),
            engine: None,
            jobs: BTreeMap::new(),
            slots: BTreeMap::new(),
            pending: VecDeque::new(),
            active: Vec::new(),
            restart_notice: None,
            cached_record: Arc::new(record.clone()),
            cached_progress: empty_progress(),
            verification: None,
        });
        self.event(
            LogLevel::Info,
            (EventKind::TaskCreated, LogOutcome::Succeeded),
        );
        self.event(
            LogLevel::Info,
            (EventKind::TaskStarted, LogOutcome::Started),
        );
        self.phase = UiPhase::Paused;
        self.resume(false).await?;
        let mut settings = self.settings.clone();
        let base = request
            .destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        settings.recent_directories.retain(|p| p != base);
        settings.recent_directories.insert(0, base.to_path_buf());
        settings.recent_directories.truncate(10);
        settings.recent_tasks.retain(|p| p != &path);
        settings.recent_tasks.insert(0, path);
        settings.recent_tasks.truncate(10);
        settings.endpoint = record.source.request.endpoint.clone();
        settings.options = record.options;
        storage::save_settings(&self.config.paths, &settings).await?;
        self.settings = settings;
        Ok(())
    }
    async fn pause(&mut self) -> Result<(), CoreError> {
        if !matches!(self.phase, UiPhase::Downloading | UiPhase::Verifying) {
            return Err(CoreError::InvalidState { action: "pause" });
        }
        self.join_verification().await?;
        if let (Some(budget), Some(task)) = (&self.budget, &self.task) {
            budget.suspend(task.record.task_id.as_str());
        }
        let stopped = self.stop_engine().await;
        let task = self
            .task
            .as_mut()
            .ok_or(CoreError::InvalidState { action: "pause" })?;
        task.record.state = TaskState::Paused;
        task.persist().await?;
        self.phase = UiPhase::Paused;
        self.event(LogLevel::Info, (EventKind::TaskPaused, LogOutcome::Paused));
        stopped.map(|_| ())
    }
    async fn resume(&mut self, retry: bool) -> Result<(), CoreError> {
        if matches!(self.phase, UiPhase::Downloading | UiPhase::Verifying) {
            return Err(CoreError::InvalidState { action: "resume" });
        }
        self.event(
            LogLevel::Info,
            (
                if retry {
                    EventKind::Retry
                } else {
                    EventKind::TaskResumed
                },
                LogOutcome::Started,
            ),
        );
        let task = self
            .task
            .as_mut()
            .ok_or(CoreError::InvalidState { action: "resume" })?;
        if self
            .access_origin
            .as_ref()
            .is_some_and(|origin| *origin != task.record.source.request.endpoint.url().origin())
        {
            return Err(CoreError::InvalidState {
                action: "credentials belong to another endpoint",
            });
        }
        let endpoint = &task.record.source.request.endpoint;
        if endpoint.url().as_str() == "https://huggingface.co/" || endpoint.is_modelscope() {
            let current = storage::load_settings(&self.config.paths).await?;
            refresh_saved_token(&mut self.access, endpoint, &self.settings, &current);
            self.settings.hf_token = current.hf_token;
            self.settings.modelscope_token = current.modelscope_token;
        }
        task.store.resume_publications(&mut task.record).await?;
        let items = task.store.inspect(&task.record).await?;
        task.pending.clear();
        task.restart_notice = items
            .iter()
            .find_map(|item| item.action.reason().map(str::to_owned));
        for item in items {
            let file = &mut task.record.files[item.index];
            if item.action == ResumeAction::RetainLocal {
                file.completed = true;
                continue;
            }
            if item.action == ResumeAction::RestartIntegrityFailed {
                if retry {
                    task.pending.push_back((item.index, item.action));
                }
                continue;
            }
            file.completed = item.action == ResumeAction::SkipSizeChecked;
            if file.completed
                && retry
                && matches!(
                    file.failure,
                    Some(FileFailure {
                        kind: FailureKind::Disk,
                        ..
                    })
                )
            {
                file.failure = None;
                file.verification = VerificationStatus::Pending;
            } else if !file.completed && (retry || file.failure.is_none()) {
                file.failure = None;
                file.verification = VerificationStatus::Pending;
                task.pending.push_back((item.index, item.action));
            }
        }
        let needs_verification = task.record.verify_after_download == Some(true)
            && task.record.files.iter().any(|file| {
                file.completed
                    && file.failure.is_none()
                    && matches!(
                        file.verification,
                        VerificationStatus::Pending | VerificationStatus::LegacyUnverified
                    )
            });
        if task.pending.is_empty()
            && !needs_verification
            && matches!(task.record.state, TaskState::Completed | TaskState::Failed)
        {
            return Ok(());
        }
        task.record.state = TaskState::Running;
        task.persist().await?;
        if let Some(budget) = &self.budget {
            budget.register(
                task.record.task_id.as_str(),
                task.pending.iter().map(|(i, _)| *i),
            );
        }
        let no_pending = task.pending.is_empty();
        self.phase = UiPhase::Preparing;
        self.publish().await;
        if no_pending {
            self.finish_if_idle().await?;
            return Ok(());
        }
        if self.budget.is_some() {
            self.phase = UiPhase::Downloading;
            return self.fill().await;
        }
        self.event(
            LogLevel::Debug,
            (EventKind::EngineLaunch, LogOutcome::Started),
        );
        let task = self
            .task
            .as_mut()
            .ok_or(CoreError::InvalidState { action: "resume" })?;
        let mut engine = Aria2::launch(Aria2Launch {
            executable: self.config.aria2_executable.clone(),
        })
        .await
        .map_err(|error| engine_error("launch", error))?;
        if let Err(error) = engine.configure(&task.record.options).await {
            let started = Instant::now();
            if let Err(error) = engine.shutdown().await {
                self.log_shutdown_failure(&error, started);
                return Err(engine_error("shutdown", error));
            }
            return Err(engine_error("configure", error));
        }
        let child_pid = engine.pid();
        task.engine = Some(engine);
        self.measured_event(
            LogLevel::Debug,
            (EventKind::EngineLaunch, LogOutcome::Succeeded),
            LogMetrics {
                child_pid: Some(child_pid),
                ..LogMetrics::default()
            },
            0,
            None,
        );
        self.event(
            LogLevel::Debug,
            (EventKind::EngineConfigure, LogOutcome::Succeeded),
        );
        self.phase = UiPhase::Downloading;
        self.fill().await
    }
    fn log_shutdown_failure(&mut self, error: &ProbeError, started: Instant) {
        if let ProbeError::Shutdown(receipt) = error {
            self.measured_event(
                LogLevel::Debug,
                (EventKind::ProcessExit, LogOutcome::Engine),
                LogMetrics {
                    child_pid: receipt.child_pid,
                    child_exit_code: receipt.child_exit_code,
                    ..LogMetrics::default()
                },
                elapsed_ms(started),
                None,
            );
        }
    }
    async fn stop_engine(&mut self) -> Result<ShutdownReceipt, CoreError> {
        self.status_unavailable_since = None;
        if self.task.as_ref().is_some_and(|task| task.engine.is_some()) {
            self.event(
                LogLevel::Debug,
                (EventKind::EngineShutdown, LogOutcome::Started),
            );
        }
        if let Some(task) = self.task.as_mut()
            && let Some(engine) = task.engine.take()
        {
            let started = Instant::now();
            let stopped = engine.shutdown().await;
            task.jobs.clear();
            task.slots.clear();
            task.active.clear();
            task.pending.clear();
            task.refresh();
            let receipt = match stopped {
                Ok(receipt) => receipt,
                Err(error) => {
                    self.log_shutdown_failure(&error, started);
                    return Err(engine_error("shutdown", error));
                }
            };
            self.measured_event(
                LogLevel::Debug,
                (EventKind::ProcessExit, LogOutcome::Succeeded),
                LogMetrics {
                    child_pid: receipt.child_pid,
                    child_exit_code: receipt.child_exit_code,
                    ..LogMetrics::default()
                },
                0,
                None,
            );
            return Ok(receipt);
        }
        Ok(ShutdownReceipt {
            stop: StopKind::Graceful,
            child_pid: None,
            child_exit_code: None,
        })
    }
    // All results are already recorded when this engine has no jobs left.
    async fn stop_idle_engine(&mut self) {
        if let Err(error) = self.stop_engine().await {
            self.event(
                LogLevel::Warn,
                (EventKind::EngineShutdown, LogOutcome::Engine),
            );
            self.error = Some(error.ui_error());
        }
    }
    async fn fill(&mut self) -> Result<(), CoreError> {
        // Verification can finish while the engine is busy. Submit more files after polling recovers.
        if self.status_unavailable_since.is_some() {
            return Ok(());
        }
        // One hashing worker shares the same file slots as aria2 jobs.
        let ready = self.task.as_ref().is_some_and(|task| {
            (task.engine.is_some() || self.budget.is_some())
                && task.record.verify_after_download == Some(true)
                && task.jobs.len()
                    < self
                        .budget
                        .as_ref()
                        .map_or(usize::from(task.record.options.concurrent_files()), |b| {
                            b.usage().1
                        })
                && task.record.files.iter().any(|f| {
                    f.completed
                        && f.failure.is_none()
                        && matches!(
                            f.verification,
                            VerificationStatus::Pending | VerificationStatus::LegacyUnverified
                        )
                })
        });
        if self.verification.is_none() && ready {
            self.next_verification().await?;
        }

        if self
            .task
            .as_ref()
            .is_some_and(|task| !task.pending.is_empty())
        {
            self.event(LogLevel::Debug, (EventKind::EngineAdd, LogOutcome::Started));
        }
        let task = self
            .task
            .as_mut()
            .ok_or(CoreError::InvalidState { action: "fill" })?;
        let limit = self
            .budget
            .as_ref()
            .map_or(usize::from(task.record.options.concurrent_files()), |b| {
                b.usage().1
            })
            .saturating_sub(usize::from(self.verification.is_some()));
        let mut client =
            RepositoryClient::new(&task.record.source.request.endpoint, self.access.clone())?;
        client.set_logger(self.logger.clone());
        let mut changed = false;
        let cancel = self.run_cancel.clone();
        while task.jobs.len() < limit && !cancel.is_cancelled() {
            let Some(&(index, action)) = task.pending.front() else {
                break;
            };
            let slot = if let Some(budget) = &self.budget {
                let Some(slot) = budget.download(task.record.task_id.as_str(), index) else {
                    break;
                };
                Some(slot)
            } else {
                None
            };
            task.pending.pop_front();
            let entry = task.record.files[index].entry.clone();
            if entry.size.get() == 0 && action != ResumeAction::ResumeControl {
                let file = task.store.prepare(&entry, action).await?;
                task.record.files[index].completed = false;
                task.record.files[index].failure = None;
                task.record.files[index].verification = VerificationStatus::Pending;
                task.persist().await?;
                task.store.create_empty(file).await?;
                task.store.verify_size(&entry).await?;
                task.record.files[index].completed = true;
                task.persist().await?;
                changed = true;
                continue;
            }
            let url = match client
                .download_url(&task.record.source, &entry, &cancel)
                .await
            {
                Ok(url) => url,
                Err(RepositoryError::Cancelled) => {
                    task.pending.push_front((index, action));
                    return Ok(());
                }
                Err(error) => {
                    let http_status = match &error {
                        RepositoryError::Http { status, .. } => Some(*status),
                        _ => None,
                    };
                    let (kind, message) = match error {
                        RepositoryError::UnsupportedEndpoint => (
                            FailureKind::Authentication,
                            "Bearer-only downloads are unsupported",
                        ),
                        RepositoryError::Http {
                            status: 401 | 403, ..
                        } => (
                            FailureKind::Authentication,
                            "Download authentication failed",
                        ),
                        _ => (FailureKind::Network, "Unable to resolve download URL"),
                    };
                    if action != ResumeAction::RestartIntegrityFailed {
                        task.record.files[index].failure = Some(FileFailure {
                            kind,
                            message: message.into(),
                            aria2_code: None,
                            http_status,
                        });
                    }
                    task.persist().await?;
                    changed = true;
                    continue;
                }
            };
            let file = task.store.prepare(&entry, action).await?;
            task.record.files[index].completed = false;
            task.record.files[index].failure = None;
            task.record.files[index].verification = VerificationStatus::Pending;
            task.persist().await?;
            if task.engine.is_none() {
                let mut engine = Aria2::launch(Aria2Launch {
                    executable: self.config.aria2_executable.clone(),
                })
                .await
                .map_err(|e| engine_error("launch", e))?;
                if let Err(error) = engine
                    .configure(&crate::DownloadOptions::new(
                        task.record.options.connections_per_file(),
                        10,
                    )?)
                    .await
                {
                    let _ = engine.shutdown().await;
                    return Err(engine_error("configure", error));
                }
                task.engine = Some(engine);
            }
            let engine = task
                .engine
                .as_mut()
                .ok_or(CoreError::InvalidState { action: "engine" })?;
            let gid = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(()),
                gid = engine.add(DownloadJob {
                    url,
                    file,
                    proxy: self.access.proxy.clone(),
                }) => gid.map_err(|error| engine_error("add", error))?,
            };
            task.jobs.insert(gid, index);
            if let Some(slot) = slot {
                task.slots.insert(index, slot);
            }
        }
        if self.budget.is_none() {
            debug_assert!(
                task.jobs.len() + usize::from(self.verification.is_some())
                    <= usize::from(task.record.options.concurrent_files())
            );
        }
        if changed {
            task.refresh();
        }
        if self.budget.is_some()
            && task.engine.is_some()
            && task.jobs.is_empty()
            && !task.pending.is_empty()
        {
            // A repository waiting behind others must not retain an idle aria2 process.
            let pending = std::mem::take(&mut task.pending);
            self.stop_idle_engine().await;
            self.task.as_mut().expect("running task").pending = pending;
        }
        self.finish_if_idle().await
    }
    async fn tick(&mut self) -> Result<(), CoreError> {
        if self.task.as_ref().is_some_and(|t| t.engine.is_none()) {
            return self.fill().await;
        }
        let task = self
            .task
            .as_mut()
            .ok_or(CoreError::InvalidState { action: "poll" })?;
        let gids: Vec<_> = task.jobs.keys().copied().collect();
        let engine = task
            .engine
            .as_mut()
            .ok_or(CoreError::InvalidState { action: "poll" })?;
        let started = Instant::now();
        let result = tokio::select! {
            biased;
            () = self.run_cancel.cancelled() => return Ok(()),
            statuses = engine.statuses(&gids) => statuses,
        };
        let statuses = match result {
            Ok(statuses) => {
                if let Some(since) = self.status_unavailable_since.take() {
                    self.measured_event(
                        LogLevel::Debug,
                        (EventKind::EngineStatus, LogOutcome::Succeeded),
                        LogMetrics::default(),
                        elapsed_ms(since),
                        None,
                    );
                }
                statuses
            }
            Err(error @ (ProbeError::Timeout | ProbeError::Connection)) => {
                let first = self.status_unavailable_since.is_none();
                let since = *self.status_unavailable_since.get_or_insert(started);
                if since.elapsed() >= STATUS_OUTAGE_LIMIT {
                    return Err(engine_error("status", error));
                }
                if first {
                    self.measured_event(
                        LogLevel::Warn,
                        (EventKind::EngineStatus, LogOutcome::Unavailable),
                        LogMetrics {
                            engine_operation: Some("status"),
                            engine_failure: Some(if matches!(error, ProbeError::Timeout) {
                                EngineFailure::Timeout
                            } else {
                                EngineFailure::Connection
                            }),
                            ..LogMetrics::default()
                        },
                        elapsed_ms(since),
                        None,
                    );
                }
                return Ok(());
            }
            Err(error) => return Err(engine_error("status", error)),
        };
        let task = self.task.as_mut().expect("polled task");
        task.active.clear();
        let mut changed = false;
        for status in statuses {
            let index = *task.jobs.get(&status.gid).ok_or(CoreError::InvalidState {
                action: "unknown engine result",
            })?;
            if status.total_bytes.get() != 0
                && status.total_bytes != task.record.files[index].entry.size
            {
                return Err(CoreError::Storage(storage::StorageError::SizeMismatch));
            }
            match status.state {
                EngineState::Complete => {
                    changed = true;
                    task.store
                        .verify_size(&task.record.files[index].entry)
                        .await?;
                    task.record.files[index].completed = true;
                    if task.record.verify_after_download != Some(true) {
                        Self::publish_file(task, index).await?;
                    }
                    task.record.files[index].failure = None;
                    task.record.files[index].verification = VerificationStatus::Pending;
                    task.persist().await?;
                    task.jobs.remove(&status.gid);
                    task.slots.remove(&index);
                }
                EngineState::Failed => {
                    changed = true;
                    task.record.files[index].failure =
                        Some(status.failure.unwrap_or(FileFailure {
                            kind: FailureKind::Engine,
                            message: "Download failed".into(),
                            aria2_code: None,
                            http_status: None,
                        }));
                    task.persist().await?;
                    task.jobs.remove(&status.gid);
                    task.slots.remove(&index);
                }
                EngineState::Active | EngineState::Waiting | EngineState::Paused => {
                    task.active.push(ActiveFile {
                        index,
                        completed_bytes: status.completed_bytes,
                        bytes_per_second: status.bytes_per_second,
                    })
                }
            }
        }
        if changed {
            task.refresh();
            self.event(
                LogLevel::Debug,
                (EventKind::EngineStatus, LogOutcome::Succeeded),
            );
        }
        self.fill().await
    }
    async fn finish_if_idle(&mut self) -> Result<(), CoreError> {
        let task = self
            .task
            .as_mut()
            .ok_or(CoreError::InvalidState { action: "finish" })?;
        if !task.pending.is_empty() || !task.jobs.is_empty() {
            return Ok(());
        }
        self.stop_idle_engine().await;
        if self.run_cancel.is_cancelled() {
            return Ok(());
        }
        self.event(
            LogLevel::Info,
            (EventKind::TransferCompleted, LogOutcome::Succeeded),
        );
        if self.verification.is_some() {
            self.phase = UiPhase::Verifying;
            return Ok(());
        }
        self.next_verification().await
    }
    async fn verify_existing(&mut self) -> Result<(), CoreError> {
        if !matches!(
            self.phase,
            UiPhase::Paused | UiPhase::Browsing | UiPhase::Idle
        ) {
            return Err(CoreError::InvalidState {
                action: "verify existing",
            });
        }
        let task = self.task.as_mut().ok_or(CoreError::InvalidState {
            action: "verify existing",
        })?;
        if task.record.verify_after_download == Some(false)
            || !task.record.files.iter().all(|file| {
                file.completed
                    && file.failure.is_none()
                    && !matches!(
                        file.disposition,
                        storage::FileDisposition::KeptDifferent
                            | storage::FileDisposition::TimeUnknown
                            | storage::FileDisposition::Unchecked
                    )
            })
        {
            return Err(CoreError::InvalidState {
                action: "verify existing",
            });
        }
        for file in &task.record.files {
            task.store.verification_path(&file.entry).await?;
        }
        if task.record.verify_after_download.is_none() {
            task.store
                .enable_verification_for_legacy(&mut task.record)
                .await?;
            for file in &mut task.record.files {
                file.verification = VerificationStatus::Pending;
            }
            task.persist().await?;
        }
        self.resume(false).await
    }
    async fn next_verification(&mut self) -> Result<(), CoreError> {
        if self.run_cancel.is_cancelled() {
            return Ok(());
        }
        let task = self
            .task
            .as_mut()
            .ok_or(CoreError::InvalidState { action: "verify" })?;
        if task.record.verify_after_download == Some(true)
            && let Some(index) = task.record.files.iter().position(|file| {
                file.completed
                    && file.failure.is_none()
                    && matches!(
                        file.verification,
                        VerificationStatus::Pending | VerificationStatus::LegacyUnverified
                    )
            })
        {
            let slot = if let Some(budget) = &self.budget {
                let Some(slot) = budget.hash() else {
                    self.phase = if task.pending.is_empty() && task.engine.is_none() {
                        UiPhase::Verifying
                    } else {
                        UiPhase::Downloading
                    };
                    return Ok(());
                };
                Some(slot)
            } else {
                None
            };
            let entry = task.record.files[index].entry.clone();
            let path = task.store.verification_path(&entry).await?;
            if self.run_cancel.is_cancelled() {
                return Ok(());
            }
            task.record.files[index].verification = VerificationStatus::Pending;
            task.persist().await?;
            task.refresh();
            let files_done = task
                .record
                .files
                .iter()
                .filter(|file| {
                    matches!(
                        file.verification,
                        VerificationStatus::Passed
                            | VerificationStatus::Unavailable
                            | VerificationStatus::Failed
                    )
                })
                .count();
            task.verification = Some(VerificationProgress {
                file_index: index,
                file_bytes_done: 0,
                files_done: u64::try_from(files_done).map_err(|_| CoreError::WorkerStopped)?,
                files_total: u64::try_from(task.record.files.len())
                    .map_err(|_| CoreError::WorkerStopped)?,
            });
            let (bytes, receiver) = watch::channel(0);
            self.verification_bytes = receiver;
            let cancel = self.run_cancel.clone();
            let algorithm = entry.content_hash.as_ref().map(|hash| hash.algorithm);
            #[cfg(test)]
            let hook = self.verification_hook.clone();
            self.verification = Some(VerificationRun {
                index,
                _slot: slot,
                started: Instant::now(),
                algorithm: algorithm.map(|algorithm| match algorithm {
                    crate::repository::HashAlgorithm::Sha256 => LogOutcome::Sha256,
                    crate::repository::HashAlgorithm::GitSha1 => LogOutcome::GitSha1,
                }),
                result: tokio::task::spawn_blocking(move || {
                    crate::integrity::verify_file(
                        &path,
                        entry.content_hash.as_ref(),
                        &cancel,
                        |done, _total| {
                            bytes.send_replace(done);
                            #[cfg(test)]
                            if let Some(hook) = &hook {
                                hook(done);
                            }
                        },
                    )
                }),
            });
            self.phase = if task.engine.is_some() {
                UiPhase::Downloading
            } else {
                UiPhase::Verifying
            };
            self.event(
                LogLevel::Debug,
                (
                    EventKind::Verification,
                    match algorithm {
                        Some(crate::repository::HashAlgorithm::Sha256) => LogOutcome::Sha256,
                        Some(crate::repository::HashAlgorithm::GitSha1) => LogOutcome::GitSha1,
                        None => LogOutcome::Unavailable,
                    },
                ),
            );
            return Ok(());
        }
        if !task.pending.is_empty() || !task.jobs.is_empty() {
            self.phase = UiPhase::Downloading;
            return Ok(());
        }
        let mut passed = 0u64;
        let mut unavailable = 0u64;
        let mut failed = 0u64;
        for file in &task.record.files {
            match file.verification {
                VerificationStatus::Passed => passed += 1,
                VerificationStatus::Unavailable => unavailable += 1,
                VerificationStatus::Failed => failed += 1,
                VerificationStatus::LegacyUnverified | VerificationStatus::Pending => {}
            }
        }
        let complete = task
            .record
            .files
            .iter()
            .all(|file| file.completed && file.failure.is_none());
        task.record.state = if complete {
            TaskState::Completed
        } else {
            TaskState::Failed
        };
        task.verification = None;
        task.persist().await?;
        task.refresh();
        self.phase = UiPhase::Paused;
        let outcome = if failed != 0 {
            Some(CompletionOutcome::IntegrityFailed)
        } else if !complete {
            None
        } else {
            match task.record.verify_after_download {
                Some(false) => Some(CompletionOutcome::VerificationDisabled),
                Some(true) if unavailable == 0 => Some(CompletionOutcome::Verified),
                Some(true) => Some(CompletionOutcome::Unverified),
                None => None,
            }
        };
        if let Some(outcome) = outcome {
            let task_id = task.record.task_id.clone();
            self.event(
                if failed != 0 {
                    LogLevel::Warn
                } else {
                    LogLevel::Info
                },
                (
                    EventKind::Verification,
                    match outcome {
                        CompletionOutcome::Verified => LogOutcome::Succeeded,
                        CompletionOutcome::VerificationDisabled => LogOutcome::Disabled,
                        CompletionOutcome::Unverified => LogOutcome::Unavailable,
                        CompletionOutcome::IntegrityFailed => LogOutcome::IntegrityFailed,
                    },
                ),
            );
            self.notice_epoch = self.notice_epoch.saturating_add(1);
            (self.completion)(CompletionNotice {
                epoch: self.notice_epoch,
                task_id,
                outcome,
                passed,
                unavailable,
                failed,
            });
        }
        Ok(())
    }
    async fn publish_file(task: &mut RunningTask, index: usize) -> Result<(), CoreError> {
        if let Err(error) = task
            .store
            .publish_replacement(&mut task.record, index)
            .await
        {
            task.record.files[index].failure = Some(FileFailure {
                kind: FailureKind::Disk,
                message: "Unable to publish staged file; local data retained".into(),
                aria2_code: None,
                http_status: None,
            });
            task.persist().await?;
            return Err(error.into());
        }
        Ok(())
    }
    async fn verified(
        &mut self,
        result: Result<std::io::Result<crate::integrity::IntegrityResult>, tokio::task::JoinError>,
    ) -> Result<(), CoreError> {
        let run = self.verification.take().ok_or(CoreError::WorkerStopped)?;
        let outcome = match &result {
            Ok(Ok(crate::integrity::IntegrityResult::Matched)) => LogOutcome::Succeeded,
            Ok(Ok(crate::integrity::IntegrityResult::Mismatch { .. })) => {
                LogOutcome::IntegrityFailed
            }
            Ok(Ok(crate::integrity::IntegrityResult::Unavailable)) => LogOutcome::Unavailable,
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => {
                LogOutcome::Cancelled
            }
            Ok(Err(_)) => LogOutcome::Disk,
            Err(_) => LogOutcome::Internal,
        };
        let bytes = *self.verification_bytes.borrow();
        self.measured_event(
            LogLevel::Debug,
            (EventKind::Verification, outcome),
            LogMetrics {
                algorithm: run.algorithm,
                ..LogMetrics::default()
            },
            elapsed_ms(run.started),
            Some(bytes),
        );
        // The handle has already joined; cancellation wins even over a ready successful result.
        if self.run_cancel.is_cancelled() {
            return Ok(());
        }
        let result = result.map_err(|_| CoreError::WorkerStopped)?;
        let task = self.task.as_mut().ok_or(CoreError::WorkerStopped)?;
        let file = &mut task.record.files[run.index];
        file.verification = match result {
            Ok(crate::integrity::IntegrityResult::Matched) => VerificationStatus::Passed,
            Ok(crate::integrity::IntegrityResult::Unavailable) => VerificationStatus::Unavailable,
            Ok(crate::integrity::IntegrityResult::Mismatch { .. }) => {
                file.failure = Some(FileFailure {
                    kind: FailureKind::Integrity,
                    message: "Integrity check failed".into(),
                    aria2_code: None,
                    http_status: None,
                });
                VerificationStatus::Failed
            }
            Err(error) => {
                file.failure = Some(FileFailure {
                    kind: FailureKind::Disk,
                    message: "File verification I/O failed".into(),
                    aria2_code: None,
                    http_status: None,
                });
                return Err(StorageError::Io(error).into());
            }
        };
        if task.record.files[run.index].verification == VerificationStatus::Passed {
            Self::publish_file(task, run.index).await?;
        }
        task.verification = None;
        task.persist().await?;
        task.refresh();
        if task.engine.is_some() {
            // The next status poll will refill the slot once the engine is responsive.
            Ok(())
        } else {
            self.next_verification().await
        }
    }
    async fn join_verification(&mut self) -> Result<(), CoreError> {
        self.run_cancel.cancel();
        if let Some(run) = self.verification.take() {
            match run.result.await.map_err(|_| CoreError::WorkerStopped)? {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(StorageError::Io(error).into()),
            }
        }
        if let Some(task) = self.task.as_mut() {
            task.verification = None;
        }
        Ok(())
    }
    async fn fail_running(&mut self) {
        if let (Some(budget), Some(task)) = (&self.budget, &self.task) {
            budget.suspend(task.record.task_id.as_str());
        }
        if let Err(error) = self.join_verification().await {
            self.error = Some(error.ui_error());
        }
        if let Some(task) = self.task.as_mut() {
            for index in task.jobs.values() {
                let file = &mut task.record.files[*index];
                if !file.completed && file.failure.is_none() {
                    file.failure = Some(FileFailure {
                        kind: FailureKind::Engine,
                        message: "Download engine stopped".into(),
                        aria2_code: None,
                        http_status: None,
                    });
                }
            }
        }
        if let Err(error) = self.stop_engine().await {
            self.error.get_or_insert_with(|| error.ui_error());
        }
        if let Some(task) = self.task.as_mut() {
            task.record.state = TaskState::Failed;
            if let Err(error) = task.persist().await {
                self.error = Some(CoreError::Storage(error).ui_error());
            }
            task.refresh();
        }
        self.phase = UiPhase::Paused;
    }
}
