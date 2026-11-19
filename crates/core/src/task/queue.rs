use super::*;
#[path = "queue_files.rs"]
mod files;
use crate::{
    repository::NetworkAccess,
    storage::{self, TaskState},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

type Reply = oneshot::Sender<Result<CommandReceipt, CoreError>>;
struct Child {
    task_file: PathBuf,
    session: CoreSession,
    snapshot: Arc<AppSnapshot>,
    access: NetworkAccess,
}
enum Event {
    Changed(Option<String>, Arc<AppSnapshot>),
    Ack(
        Option<String>,
        Result<CommandReceipt, CoreError>,
        Reply,
        bool,
    ),
}
struct Pending {
    work: JoinHandle<Result<files::Submission, CoreError>>,
    cancel: CancellationToken,
    reply: Reply,
    access: NetworkAccess,
}
#[derive(Default, Serialize, Deserialize)]
struct Index {
    version: u32,
    tasks: Vec<PathBuf>,
    order: Vec<(String, usize, u64)>,
}
struct Queue {
    config: CoreConfig,
    draft: CoreSession,
    draft_view: Arc<AppSnapshot>,
    children: BTreeMap<String, Child>,
    current: Option<String>,
    budget: Arc<super::budget::Budget>,
    access: NetworkAccess,
    shutdown: CancellationToken,
    snapshots: watch::Sender<Arc<AppSnapshot>>,
    events: mpsc::Sender<Event>,
    revision: u64,
    error: Option<UiError>,
    message: Option<String>,
    summary: Option<SubmissionSummary>,
    pending: Option<Pending>,
    admitting: bool,
    conflict: Option<(u64, files::Submission, ConflictPlan, NetworkAccess)>,
    plan_id: u64,
    checking: watch::Receiver<files::Checking>,
    completion: CompletionCallback,
    logger: Option<Arc<SessionLogger>>,
    _lock: std::fs::File,
    admission_indices: Vec<usize>,
}
fn monitor(session: &CoreSession, id: Option<String>, events: mpsc::Sender<Event>) {
    let mut snapshots = session.snapshots.clone();
    tokio::spawn(async move {
        while snapshots.changed().await.is_ok() {
            let snapshot = snapshots.borrow_and_update().clone();
            if events
                .send(Event::Changed(id.clone(), snapshot))
                .await
                .is_err()
            {
                break;
            }
        }
    });
}
pub async fn spawn_queue_with_services(
    config: CoreConfig,
    completion: CompletionCallback,
    logger: Option<Arc<SessionLogger>>,
) -> Result<CoreSession, CoreError> {
    let lock_path = config.paths.settings_file.with_file_name("queue.lock");
    if let Some(parent) = lock_path.parent() {
        storage::check_path(parent)?;
        std::fs::create_dir_all(parent).map_err(storage::StorageError::Io)?;
        storage::check_path(parent)?;
    }
    storage::check_path(&lock_path)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(storage::StorageError::Io)?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| storage::StorageError::Locked)?;
    let shutdown = CancellationToken::new();
    let draft = spawn_worker(
        config.clone(),
        Arc::new(|_| {}),
        logger.clone(),
        None,
        Some(shutdown.child_token()),
    )
    .await?;
    let initial = draft.snapshots.borrow().clone();
    let budget =
        super::budget::Budget::new(usize::from(initial.settings.options.concurrent_files()));
    let (snapshots_tx, snapshots) = watch::channel(initial.clone());
    let (commands, receiver) = mpsc::channel(32);
    let (events, updates) = mpsc::channel(64);
    monitor(&draft, None, events.clone());
    let mut queue = Queue {
        config,
        draft,
        draft_view: initial,
        children: BTreeMap::new(),
        current: None,
        budget,
        access: NetworkAccess::default(),
        shutdown: shutdown.clone(),
        snapshots: snapshots_tx,
        events,
        revision: 0,
        error: None,
        message: None,
        summary: None,
        pending: None,
        admitting: false,
        conflict: None,
        plan_id: 0,
        checking: watch::channel(files::Checking::default()).1,
        completion,
        logger,
        _lock: lock,
        admission_indices: Vec::new(),
    };
    queue.restore().await?;
    queue.publish();
    let worker = tokio::spawn(queue.run(receiver, updates));
    Ok(CoreSession {
        handle: CoreHandle {
            commands,
            shutdown: shutdown.clone(),
            interrupt: Arc::new(std::sync::Mutex::new(shutdown.child_token())),
        },
        snapshots,
        worker,
    })
}
impl Queue {
    fn index_path(&self) -> PathBuf {
        self.config.paths.settings_file.with_file_name("queue.json")
    }
    async fn persist(&self) -> Result<(), CoreError> {
        let index = Index {
            version: 1,
            tasks: self
                .children
                .values()
                .map(|c| c.task_file.clone())
                .collect(),
            order: self
                .budget
                .order()
                .into_iter()
                .filter(|(id, _, _)| self.children.contains_key(id))
                .collect(),
        };
        let path = self.index_path();
        tokio::task::spawn_blocking(move || storage::atomic_write(&path, &index))
            .await
            .map_err(|_| CoreError::WorkerStopped)??;
        Ok(())
    }
    async fn child(&self) -> Result<CoreSession, CoreError> {
        spawn_worker(
            self.config.clone(),
            self.completion.clone(),
            self.logger.clone(),
            Some(self.budget.clone()),
            Some(self.shutdown.child_token()),
        )
        .await
    }
    async fn restore(&mut self) -> Result<(), CoreError> {
        let path = self.index_path();
        if storage::check_path(&path)?.is_none() {
            return Ok(());
        }
        let wire = tokio::fs::read(path)
            .await
            .map_err(storage::StorageError::Io)?;
        let index: Index =
            serde_json::from_slice(&wire).map_err(|_| storage::StorageError::CorruptRecord)?;
        if index.version != 1 {
            return Err(storage::StorageError::UnsupportedSchema(index.version).into());
        }
        self.budget.restore(index.order);
        for path in index.tasks {
            let session = self.child().await?;
            let result = session
                .handle
                .submit(CoreCommand::OpenTask(path.clone()))
                .map_err(|_| CoreError::WorkerStopped)?
                .await
                .map_err(|_| CoreError::WorkerStopped)?;
            if let Err(error) = result {
                self.message = Some(error.ui_error().message);
                session.handle.request_shutdown().ok();
                session.join().await?;
                continue;
            }
            let snapshot = session.snapshots.borrow().clone();
            let Some(task) = &snapshot.task else { continue };
            let id = task.record.task_id.as_str().to_owned();
            monitor(&session, Some(id.clone()), self.events.clone());
            let access = NetworkAccess {
                token: self
                    .draft_view
                    .settings
                    .token_for(&task.record.source.request.endpoint),
                proxy: task
                    .record
                    .proxy_address
                    .as_ref()
                    .map(|p| crate::ProxySpec::parse(p.url().as_str()))
                    .transpose()?,
            };
            self.current.get_or_insert(id.clone());
            self.children.insert(
                id,
                Child {
                    task_file: path.clone(),
                    session,
                    snapshot,
                    access,
                },
            );
        }
        Ok(())
    }
    async fn remember(&mut self, id: &str) -> Result<(), CoreError> {
        let Some(child) = self.children.get(id) else {
            return Ok(());
        };
        let Some(task) = &child.snapshot.task else {
            return Ok(());
        };
        let mut settings = (*self.draft_view.settings).clone();
        let base = task
            .destination
            .parent()
            .unwrap_or(&task.destination)
            .to_path_buf();
        settings.recent_directories.retain(|p| p != &base);
        settings.recent_directories.insert(0, base);
        settings.recent_directories.truncate(10);
        settings.recent_tasks.retain(|p| p != &child.task_file);
        settings.recent_tasks.insert(0, child.task_file.clone());
        settings.recent_tasks.truncate(10);
        self.draft
            .handle
            .submit(CoreCommand::SaveSettings(settings))
            .map_err(|_| CoreError::WorkerStopped)?
            .await
            .map_err(|_| CoreError::WorkerStopped)??;
        self.draft_view = self.draft.snapshots.borrow().clone();
        Ok(())
    }
    fn publish(&mut self) {
        self.revision += 1;
        let mut view = (*self.draft_view).clone();
        view.revision = self.revision;
        if let Some(child) = self.current.as_ref().and_then(|id| self.children.get(id)) {
            view.task = child.snapshot.task.clone();
            view.phase = child.snapshot.phase;
            view.error = child.snapshot.error.clone();
        }
        if self.shutdown.is_cancelled() {
            view.phase = UiPhase::Stopping;
        }
        if let Some(error) = &self.error {
            view.error = Some(error.clone());
        }
        let (used, limit, _) = self.budget.usage();
        let check = self.checking.borrow();
        let mut tasks = self
            .children
            .values()
            .filter_map(|c| {
                c.snapshot.task.as_ref().map(|task| QueuedTask {
                    view: task.clone(),
                    phase: c.snapshot.phase,
                    error: c.snapshot.error.clone(),
                })
            })
            .collect::<Vec<_>>();
        tasks.sort_by_key(|t| t.view.record.created_unix_ms);
        view.queue = Some(Arc::new(QueueView {
            tasks: tasks.into(),
            used_slots: used,
            slot_limit: limit,
            preparing: self.pending.is_some() || self.admitting,
            cancellable: self.pending.is_some() || self.conflict.is_some(),
            checking_path: (!check.path.is_empty()).then(|| check.path.clone()),
            checking_bytes: check.done,
            checking_total: check.total,
            conflicts: self.conflict.as_ref().map(|(_, _, plan, _)| plan.clone()),
            message: self.message.clone(),
            last_submission: self.summary.clone(),
            draft_error: self.draft_view.error.clone(),
        }));
        self.snapshots.send_replace(Arc::new(view));
    }
    fn forward(
        &self,
        id: Option<String>,
        commands: Vec<CoreCommand>,
        reply: Reply,
        admission: bool,
    ) {
        let handle = id
            .as_ref()
            .and_then(|id| self.children.get(id))
            .map_or_else(|| self.draft.handle.clone(), |c| c.session.handle.clone());
        let sender = self.events.clone();
        // Submit in coordinator order; detached receipt waiters must not reorder edits.
        let receipts = commands
            .into_iter()
            .map(|command| handle.submit(command))
            .collect::<Vec<_>>();
        tokio::spawn(async move {
            let mut result = Ok(CommandReceipt {
                snapshot_revision: 0,
            });
            for receipt in receipts {
                result = match receipt {
                    Ok(receipt) => receipt.await.unwrap_or(Err(CoreError::WorkerStopped)),
                    Err(_) => Err(CoreError::WorkerStopped),
                };
                if result.is_err() {
                    break;
                }
            }
            let _ = sender.send(Event::Ack(id, result, reply, admission)).await;
        });
    }

    async fn admit(
        &mut self,
        mut submission: files::Submission,
        access: NetworkAccess,
        reply: Reply,
    ) -> Result<(), (CoreError, Reply)> {
        self.summary = Some(SubmissionSummary {
            added: submission.files.iter().filter(|f| !f.completed).count(),
            identical: submission.identical,
            already_queued: submission.duplicate,
            unchecked: submission.unknown,
            kept: submission
                .files
                .iter()
                .filter(|f| {
                    matches!(
                        f.disposition,
                        storage::FileDisposition::KeptDifferent
                            | storage::FileDisposition::TimeUnknown
                    )
                })
                .count(),
        });
        self.message = Some(format!(
            "{} identical · {} already queued · {} could not be checked",
            submission.identical, submission.duplicate, submission.unknown
        ));
        if submission.files.is_empty() {
            let _ = reply.send(Ok(CommandReceipt {
                snapshot_revision: self.revision,
            }));
            return Ok(());
        }
        let id = submission.record.task_id.as_str().to_owned();
        let superseded = self
            .children
            .iter()
            .filter_map(|(old_id, child)| {
                child
                    .snapshot
                    .task
                    .as_ref()
                    .filter(|t| {
                        old_id != &id
                            && t.destination == submission.destination
                            && t.record.state == TaskState::Completed
                    })
                    .map(|_| old_id.clone())
            })
            .collect::<Vec<_>>();
        for old_id in superseded {
            if let Some(child) = self.children.remove(&old_id) {
                child.session.handle.request_shutdown().ok();
                let _ = child.session.join().await;
                self.budget.forget(&old_id);
            }
        }

        if let Some(child) = self.children.get_mut(&id) {
            child.access = access;
            let mut next = child
                .snapshot
                .task
                .as_ref()
                .map_or(0, |t| t.record.files.len());
            self.admission_indices = submission
                .files
                .iter()
                .filter_map(|f| {
                    let index = child
                        .snapshot
                        .task
                        .as_ref()
                        .and_then(|t| {
                            t.record
                                .files
                                .iter()
                                .position(|old| old.entry.path == f.entry.path)
                        })
                        .unwrap_or_else(|| {
                            let index = next;
                            next += 1;
                            index
                        });
                    (!f.completed).then_some(index)
                })
                .collect();
            self.budget.renew(&id, &self.admission_indices);
            if let Err(error) = self.persist().await {
                self.budget.discard(&id, &self.admission_indices);
                return Err((error, reply));
            }
            let prepared = PreparedAppend {
                task_id: submission.record.task_id,
                files: submission.files,
                replacements: submission.replacements,
                stamps: submission.stamps,
            };
            self.current = Some(id.clone());
            self.admitting = true;
            self.forward(
                Some(id),
                vec![CoreCommand::AppendPrepared(prepared)],
                reply,
                true,
            );
        } else {
            for file in submission.files {
                if let Some(old) = submission
                    .record
                    .files
                    .iter_mut()
                    .find(|f| f.entry.path == file.entry.path)
                {
                    *old = file;
                } else {
                    submission.record.files.push(file);
                }
            }
            submission
                .record
                .replacements
                .extend(submission.replacements);
            let session = match self.child().await {
                Ok(s) => s,
                Err(e) => return Err((e, reply)),
            };
            let snapshot = session.snapshots.borrow().clone();
            monitor(&session, Some(id.clone()), self.events.clone());
            self.children.insert(
                id.clone(),
                Child {
                    task_file: submission.destination.join(".hfdx/task.json"),
                    session,
                    snapshot,
                    access: access.clone(),
                },
            );
            self.current = Some(id.clone());
            self.admitting = true;
            self.admission_indices = submission
                .record
                .files
                .iter()
                .enumerate()
                .filter_map(|(i, f)| (!f.completed).then_some(i))
                .collect();
            self.budget.renew(&id, &self.admission_indices);
            if let Err(error) = self.persist().await {
                self.budget.discard(&id, &self.admission_indices);
                if let Some(child) = self.children.remove(&id) {
                    child.session.handle.request_shutdown().ok();
                    let _ = child.session.join().await;
                }
                self.admitting = false;
                return Err((error, reply));
            }
            let prepared = PreparedTask {
                access,
                record: submission.record,
                destination: submission.destination,
                stamps: submission.stamps,
            };
            self.forward(
                Some(id),
                vec![CoreCommand::StartPrepared(Box::new(prepared))],
                reply,
                true,
            );
        }
        Ok(())
    }
    fn draft_loading(&mut self) {
        let mut snapshot = (*self.draft_view).clone();
        snapshot.revision = snapshot.revision.saturating_add(1);
        snapshot.phase = UiPhase::Loading;
        snapshot.selection.state = crate::CatalogState::Loading;
        self.draft_view = Arc::new(snapshot);
    }
    fn reject(&mut self, reply: Reply, error: CoreError) {
        self.error = Some(error.ui_error());
        let _ = reply.send(Err(error));
    }
    async fn remove_task(&mut self, id: &str, confirmed: bool) -> Result<(), CoreError> {
        let invalid = |action| CoreError::InvalidState { action };
        if self.pending.is_some() || self.admitting || self.conflict.is_some() {
            return Err(invalid("submission in progress"));
        }
        let child = self
            .children
            .get(id)
            .ok_or_else(|| invalid("unknown task"))?;
        let latest = child.session.snapshots.borrow().clone();
        if !confirmed
            && !latest
                .task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed)
        {
            return Err(invalid("unfinished task removal needs confirmation"));
        }
        let child = self.children.remove(id).expect("checked task");
        if let Err(error) = self.persist().await {
            self.children.insert(id.to_owned(), child);
            return Err(error);
        }
        if self.current.as_deref() == Some(id) {
            self.current = self.children.keys().next().cloned();
        }
        let _ = child.session.handle.request_shutdown();
        let stopped = child.session.join().await;
        self.budget.forget(id);
        self.draft
            .handle
            .submit(CoreCommand::ForgetRecent(child.task_file))
            .map_err(|_| CoreError::WorkerStopped)?
            .await
            .map_err(|_| CoreError::WorkerStopped)??;
        self.draft_view = self.draft.snapshots.borrow().clone();
        stopped.map(|_| ())
    }
    async fn command(&mut self, command: CoreCommand, reply: Reply) {
        self.error = None;
        match command {
            command @ CoreCommand::SavePlatformTokens { .. } => {
                let result = match self.draft.handle.submit(command) {
                    Ok(receipt) => receipt.await.unwrap_or(Err(CoreError::WorkerStopped)),
                    Err(_) => Err(CoreError::WorkerStopped),
                };
                if let Err(error) = result {
                    self.reject(reply, error);
                } else {
                    let previous = self.draft_view.clone();
                    self.draft_view = self.draft.snapshots.borrow().clone();
                    for child in self.children.values_mut() {
                        if let Some(task) = &child.snapshot.task {
                            worker::refresh_saved_token(
                                &mut child.access,
                                &task.record.source.request.endpoint,
                                &previous.settings,
                                &self.draft_view.settings,
                            );
                        }
                    }
                    self.publish();
                    let _ = reply.send(Ok(CommandReceipt {
                        snapshot_revision: self.revision,
                    }));
                }
            }
            CoreCommand::Start(request) => {
                if self.pending.is_some() || self.admitting || self.conflict.is_some() {
                    self.reject(
                        reply,
                        CoreError::InvalidState {
                            action: "submission already being checked",
                        },
                    );
                    return;
                }
                let selected = &self.draft_view.selection;
                let Some(catalog) = selected.catalog.as_ref().filter(|c| {
                    c.id == request.catalog && selected.state == crate::CatalogState::Ready
                }) else {
                    self.reject(reply, crate::SelectionError::NotReady.into());
                    return;
                };
                if selected.selected.is_empty() {
                    self.reject(reply, crate::SelectionError::EmptySelection.into());
                    return;
                }
                let frozen = crate::FrozenSelection {
                    source: catalog.source.clone(),
                    files: selected
                        .selected
                        .iter()
                        .filter_map(|id| catalog.entry(*id).cloned())
                        .collect(),
                    total_bytes: selected.summary.selected_bytes,
                };
                let dest = match files::destination(&request.destination) {
                    Ok(d) => d,
                    Err(e) => {
                        self.reject(reply, e);
                        return;
                    }
                };
                if let Some(task) = self
                    .children
                    .values()
                    .filter_map(|c| c.snapshot.task.as_ref())
                    .find(|t| {
                        t.destination == dest
                            && t.record.state != TaskState::Completed
                            && !files::same_source(&t.record.source, &frozen.source)
                    })
                {
                    let a = &task.record.source.request;
                    let b = &frozen.source.request;
                    let same_repository =
                        a.endpoint == b.endpoint && a.kind == b.kind && a.repo_id == b.repo_id;
                    self.reject(
                        reply,
                        CoreError::DestinationInUse {
                            path: dest,
                            owner: Box::new(task.record.source.clone()),
                            same_repository,
                        },
                    );
                    return;
                }
                let existing = self.children.values().find_map(|c| {
                    c.snapshot
                        .task
                        .as_ref()
                        .filter(|t| t.destination == dest)
                        .map(|t| (*t.record).clone())
                });
                let cancel = self.shutdown.child_token();
                let (sender, receiver) = watch::channel(files::Checking::default());
                self.checking = receiver;
                self.pending = Some(Pending {
                    work: tokio::spawn(files::inspect(
                        frozen,
                        request,
                        existing,
                        self.budget.clone(),
                        cancel.clone(),
                        sender,
                    )),
                    cancel,
                    reply,
                    access: self.access.clone(),
                });
            }
            CoreCommand::ConfirmConflicts { plan, policy } => {
                let Some((id, submission, view, access)) = self.conflict.take() else {
                    self.reject(
                        reply,
                        CoreError::InvalidState {
                            action: "no conflict plan",
                        },
                    );
                    return;
                };
                if id != plan {
                    self.conflict = Some((id, submission, view, access));
                    self.reject(
                        reply,
                        CoreError::InvalidState {
                            action: "stale conflict plan",
                        },
                    );
                    return;
                }
                let cancel = self.shutdown.child_token();
                self.pending = Some(Pending {
                    work: tokio::spawn(files::decide(
                        submission,
                        policy,
                        access.clone(),
                        cancel.clone(),
                    )),
                    cancel,
                    reply,
                    access,
                });
            }
            CoreCommand::CancelSubmission => {
                if self.admitting {
                    self.reject(
                        reply,
                        CoreError::InvalidState {
                            action: "already admitted; pause the task",
                        },
                    );
                    return;
                }
                self.conflict = None;
                if let Some(pending) = self.pending.take() {
                    pending.cancel.cancel();
                    let _ = pending.work.await;
                    let _ = pending
                        .reply
                        .send(Err(crate::RepositoryError::Cancelled.into()));
                }
                self.message = Some("Submission cancelled; existing tasks are unchanged".into());
                let _ = reply.send(Ok(CommandReceipt {
                    snapshot_revision: self.revision,
                }));
            }
            CoreCommand::Load(request) => {
                self.access = match worker::access_defaults(
                    request.access.clone(),
                    &self.draft_view.settings,
                    &request.repository.endpoint,
                ) {
                    Ok(access) => access,
                    Err(error) => {
                        self.reject(reply, error);
                        return;
                    }
                };
                self.draft_loading();
                self.forward(None, vec![CoreCommand::Load(request)], reply, false);
            }
            CoreCommand::LoadActive(id) => {
                if let Some(child) = self.children.get(id.as_str())
                    && let Some(task) = &child.snapshot.task
                {
                    let mut repository = task.record.source.request.clone();
                    repository.requested_revision =
                        crate::Revision::parse(task.record.source.resolved_commit.as_str())
                            .expect("validated commit");
                    self.access = child.access.clone();
                    self.draft_loading();
                    self.forward(
                        None,
                        vec![CoreCommand::LoadFrozen(LoadRequest {
                            repository,
                            access: self.access.clone(),
                        })],
                        reply,
                        false,
                    );
                    return;
                }
                self.reject(
                    reply,
                    CoreError::InvalidState {
                        action: "unknown task",
                    },
                );
            }
            CoreCommand::RemoveTask { id, confirmed } => {
                let result = self.remove_task(id.as_str(), confirmed).await;
                if let Err(error) = result {
                    self.reject(reply, error);
                } else {
                    self.publish();
                    let _ = reply.send(Ok(CommandReceipt {
                        snapshot_revision: self.revision,
                    }));
                }
            }
            CoreCommand::SelectTask(id) => {
                if self.children.contains_key(id.as_str()) {
                    self.current = Some(id.as_str().into());
                    let _ = reply.send(Ok(CommandReceipt {
                        snapshot_revision: self.revision,
                    }));
                } else {
                    self.reject(
                        reply,
                        CoreError::InvalidState {
                            action: "unknown task",
                        },
                    );
                }
            }
            CoreCommand::PauseTask(id) => self.forward(
                Some(id.as_str().into()),
                vec![CoreCommand::Pause],
                reply,
                false,
            ),
            CoreCommand::ResumeTask(id) => self.forward(
                Some(id.as_str().into()),
                vec![CoreCommand::Resume],
                reply,
                false,
            ),
            CoreCommand::PauseAll | CoreCommand::ResumeAll => {
                let pause = matches!(command, CoreCommand::PauseAll);
                let handles: Vec<_> = self
                    .children
                    .values()
                    .filter(|c| {
                        if pause {
                            matches!(
                                c.snapshot.phase,
                                UiPhase::Preparing | UiPhase::Downloading | UiPhase::Verifying
                            )
                        } else {
                            c.snapshot
                                .task
                                .as_ref()
                                .is_some_and(|t| t.record.state != TaskState::Completed)
                                && c.snapshot.phase == UiPhase::Paused
                        }
                    })
                    .map(|c| c.session.handle.clone())
                    .collect();
                let sender = self.events.clone();
                tokio::spawn(async move {
                    let mut first = None;
                    let mut receipts = Vec::new();
                    for handle in handles {
                        match handle.submit(if pause {
                            CoreCommand::Pause
                        } else {
                            CoreCommand::Resume
                        }) {
                            Ok(r) => receipts.push(r),
                            Err(_) => {
                                first.get_or_insert(CoreError::WorkerStopped);
                            }
                        }
                    }
                    for r in receipts {
                        if let Err(e) = r.await.unwrap_or(Err(CoreError::WorkerStopped)) {
                            first.get_or_insert(e);
                        }
                    }
                    let _ = sender
                        .send(Event::Ack(
                            None,
                            first.map_or(
                                Ok(CommandReceipt {
                                    snapshot_revision: 0,
                                }),
                                Err,
                            ),
                            reply,
                            false,
                        ))
                        .await;
                });
            }
            CoreCommand::OpenTask(path) => {
                let dest = path.parent().and_then(|p| p.parent()).map(PathBuf::from);
                if let Some(id) = self.children.iter().find_map(|(id, c)| {
                    c.snapshot
                        .task
                        .as_ref()
                        .filter(|t| Some(&t.destination) == dest.as_ref())
                        .map(|_| id.clone())
                }) {
                    self.current = Some(id);
                    let _ = reply.send(Ok(CommandReceipt {
                        snapshot_revision: self.revision,
                    }));
                    return;
                }
                let session = match self.child().await {
                    Ok(v) => v,
                    Err(e) => {
                        self.reject(reply, e);
                        return;
                    }
                };
                let record = match storage::read_record(&path) {
                    Ok(r) => r,
                    Err(e) => {
                        session.handle.request_shutdown().ok();
                        let _ = session.join().await;
                        self.reject(reply, e.into());
                        return;
                    }
                };
                let id = record.task_id.as_str().to_owned();
                let access = NetworkAccess {
                    token: self
                        .draft_view
                        .settings
                        .token_for(&record.source.request.endpoint),
                    proxy: record
                        .proxy_address
                        .as_ref()
                        .map(|p| crate::ProxySpec::parse(p.url().as_str()))
                        .transpose()
                        .expect("validated proxy address"),
                };
                let snapshot = session.snapshots.borrow().clone();
                monitor(&session, Some(id.clone()), self.events.clone());
                self.children.insert(
                    id.clone(),
                    Child {
                        task_file: path.clone(),
                        session,
                        snapshot,
                        access,
                    },
                );
                self.current = Some(id.clone());
                self.forward(Some(id), vec![CoreCommand::OpenTask(path)], reply, true);
                self.admitting = true;
            }
            CoreCommand::Pause
            | CoreCommand::Resume
            | CoreCommand::RetryFailed
            | CoreCommand::VerifyExisting => {
                self.forward(self.current.clone(), vec![command], reply, false)
            }
            CoreCommand::SetAccess { endpoint, access } => {
                self.access = access.clone();
                if let Some(id) = &self.current
                    && let Some(child) = self.children.get_mut(id)
                    && child
                        .snapshot
                        .task
                        .as_ref()
                        .is_some_and(|t| t.record.source.request.endpoint == endpoint)
                    && child.snapshot.phase == UiPhase::Paused
                {
                    child.access = access.clone();
                    self.forward(
                        Some(id.clone()),
                        vec![CoreCommand::SetAccess { endpoint, access }],
                        reply,
                        false,
                    );
                    return;
                }
                self.forward(
                    None,
                    vec![CoreCommand::SetAccess { endpoint, access }],
                    reply,
                    false,
                );
            }
            CoreCommand::StartPrepared(_) | CoreCommand::AppendPrepared(_) => self.reject(
                reply,
                CoreError::InvalidState {
                    action: "executor command",
                },
            ),
            _ => self.forward(None, vec![command], reply, false),
        }
    }
    async fn updated(&mut self, event: Event) {
        match event {
            Event::Changed(Some(id), snapshot) => {
                if let Some(child) = self.children.get_mut(&id)
                    && snapshot.revision >= child.snapshot.revision
                {
                    child.snapshot = snapshot;
                }
            }
            Event::Changed(None, snapshot) => {
                if snapshot.revision >= self.draft_view.revision {
                    self.budget
                        .limit(usize::from(snapshot.settings.options.concurrent_files()));
                    self.draft_view = snapshot;
                }
            }
            Event::Ack(id, mut result, reply, admission) => {
                if let Some(child) = id.as_ref().and_then(|id| self.children.get_mut(id)) {
                    child.snapshot = child.session.snapshots.borrow().clone();
                } else {
                    for child in self.children.values_mut() {
                        child.snapshot = child.session.snapshots.borrow().clone();
                    }
                    let latest = self.draft.snapshots.borrow().clone();
                    if latest.revision >= self.draft_view.revision {
                        self.draft_view = latest;
                    }
                    self.budget.limit(usize::from(
                        self.draft_view.settings.options.concurrent_files(),
                    ));
                }
                if admission {
                    self.admitting = false;
                    if result.is_ok()
                        && let Some(id) = &id
                        && let Err(error) = self.remember(id).await
                    {
                        result = Err(error);
                    }
                    if result.is_err()
                        && let Some(id) = &id
                    {
                        self.budget.discard(id, &self.admission_indices);
                        if self
                            .children
                            .get(id)
                            .is_some_and(|c| c.snapshot.task.is_none())
                            && let Some(child) = self.children.remove(id)
                        {
                            child.session.handle.request_shutdown().ok();
                            let _ = child.session.join().await;
                            if self.current.as_ref() == Some(id) {
                                self.current = self.children.keys().next().cloned();
                            }
                        }
                    }
                    if let Err(error) = self.persist().await {
                        result = Err(error);
                    }
                }
                if let Err(error) = &result {
                    self.error = Some(error.ui_error());
                }
                self.publish();
                let _ = reply.send(result.map(|_| CommandReceipt {
                    snapshot_revision: self.revision,
                }));
            }
        }
    }
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Envelope>,
        mut events: mpsc::Receiver<Event>,
    ) -> Result<ShutdownReceipt, CoreError> {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {biased;
                ()=self.shutdown.cancelled()=>break,
                Some(envelope)=commands.recv()=>{self.command(envelope.command,envelope.reply).await;self.publish();},
                Some(event)=events.recv()=>{
                    self.updated(event).await;
                    self.publish();
                },
                result=async{match self.pending.as_mut(){Some(p)=>(&mut p.work).await,None=>std::future::pending().await}},if self.pending.is_some()=>{
                    let pending=self.pending.take().expect("pending preparation");
                    match result.unwrap_or(Err(CoreError::WorkerStopped)) {
                        Ok(submission) if !submission.conflicts.is_empty()=>{self.plan_id+=1;let plan=ConflictPlan{id:self.plan_id,destination:submission.destination.clone(),files:submission.conflicts.clone().into(),new_files:submission.files.iter().filter(|f|!f.completed).count(),identical:submission.identical};self.conflict=Some((self.plan_id,submission,plan,pending.access));self.publish();let _=pending.reply.send(Ok(CommandReceipt{snapshot_revision:self.revision}));},
                        Ok(submission)=>{if let Err((error,reply))=self.admit(submission,pending.access,pending.reply).await{self.error=Some(error.ui_error());let _=reply.send(Err(error));}},
                        Err(error)=>{self.error=Some(error.ui_error());let _=pending.reply.send(Err(error));},
                    }
                    self.publish();
                },
                _=interval.tick()=>{if self.pending.is_some(){self.publish();}}
            }
        }
        if let Some(pending) = self.pending.take() {
            pending.cancel.cancel();
            let _ = pending.work.await;
            let _ = pending
                .reply
                .send(Err(crate::RepositoryError::Cancelled.into()));
        }
        let persisted = self.persist().await;
        self.draft.handle.request_shutdown().ok();
        for child in self.children.values() {
            child.session.handle.request_shutdown().ok();
        }
        let mut result = self.draft.join().await;
        for (_, child) in self.children {
            match child.session.join().await {
                Ok(receipt) => {
                    if result.is_ok() {
                        result = Ok(receipt);
                    }
                }
                Err(e) => result = Err(e),
            }
        }
        persisted?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restored_tasks_keep_saved_credentials_for_frozen_append() {
        let root = tempfile::tempdir().unwrap();
        let config = CoreConfig {
            paths: crate::SettingsPaths {
                settings_file: root.path().join("settings.json"),
            },
            aria2_executable: "aria2c".into(),
        };
        let mut settings = storage::load_settings(&config.paths).await.unwrap();
        settings.hf_token = Some(crate::Token::parse("hf_fake_saved").unwrap());
        settings.modelscope_token = Some(crate::Token::parse("ms_fake_saved").unwrap());
        storage::save_settings(&config.paths, &settings)
            .await
            .unwrap();
        let mut tasks = Vec::new();
        for (name, endpoint) in [
            ("hf", "https://huggingface.co"),
            ("ms", "https://modelscope.ai"),
        ] {
            let source = crate::RepositorySpec {
                request: crate::RepositoryRequest {
                    endpoint: crate::Endpoint::parse(endpoint, false).unwrap(),
                    kind: crate::RepoKind::Model,
                    repo_id: crate::RepoId::parse("org/model").unwrap(),
                    requested_revision: crate::Revision::parse("main").unwrap(),
                },
                resolved_commit: crate::CommitId::parse(&"a".repeat(40)).unwrap(),
            };
            let record = storage::new_record(
                source,
                vec![crate::CatalogEntry {
                    path: crate::RepoPath::parse("config.json").unwrap(),
                    size: crate::Bytes::new(4),
                    content_hash: None,
                }],
                crate::DownloadOptions::default(),
            )
            .unwrap();
            let destination = root.path().join(name);
            drop(
                storage::TaskStore::create(&destination, &record)
                    .await
                    .unwrap(),
            );
            tasks.push(destination.join(".hfdx/task.json"));
        }
        storage::atomic_write(
            &config.paths.settings_file.with_file_name("queue.json"),
            &Index {
                version: 1,
                tasks,
                order: Vec::new(),
            },
        )
        .unwrap();
        let draft = super::super::spawn(config.clone()).await.unwrap();
        let initial = draft.snapshots.borrow().clone();
        let (snapshots, _receiver) = watch::channel(initial.clone());
        let (events, _updates) = mpsc::channel(64);
        let shutdown = CancellationToken::new();
        let mut queue = Queue {
            config,
            draft,
            draft_view: initial,
            children: BTreeMap::new(),
            current: None,
            budget: super::super::budget::Budget::new(2),
            access: NetworkAccess::default(),
            shutdown,
            snapshots,
            events,
            revision: 0,
            error: None,
            message: None,
            summary: None,
            pending: None,
            admitting: false,
            conflict: None,
            plan_id: 0,
            checking: watch::channel(files::Checking::default()).1,
            completion: Arc::new(|_| {}),
            logger: None,
            _lock: std::fs::File::create(root.path().join("queue.lock")).unwrap(),
            admission_indices: Vec::new(),
        };
        queue.restore().await.unwrap();
        assert_eq!(queue.children.len(), 2);
        // LoadActive forwards this access to the frozen catalog load after a restart.
        for child in queue.children.values() {
            let endpoint = &child
                .snapshot
                .task
                .as_ref()
                .unwrap()
                .record
                .source
                .request
                .endpoint;
            let expected = if endpoint.is_modelscope() {
                "ms_fake_saved"
            } else {
                "hf_fake_saved"
            };
            assert_eq!(
                child.access.token.as_ref().map(crate::Token::expose),
                Some(expected)
            );
        }
        queue.draft.handle.request_shutdown().unwrap();
        for child in queue.children.values() {
            child.session.handle.request_shutdown().unwrap();
        }
        queue.draft.join().await.unwrap();
        for (_, child) in queue.children {
            child.session.join().await.unwrap();
        }
    }
}
