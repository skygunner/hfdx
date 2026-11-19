mod budget;
mod model;
mod queue;
mod queue_model;
mod worker;
pub use model::*;
pub use queue::spawn_queue_with_services;
pub use queue_model::*;

use crate::{
    aria2::ShutdownReceipt,
    logging::SessionLogger,
    storage::{load_settings, recent_tasks},
};
use std::sync::Arc;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

struct Envelope {
    command: CoreCommand,
    run_cancel: Option<CancellationToken>,
    reply: oneshot::Sender<Result<CommandReceipt, CoreError>>,
}

#[derive(Clone)]
pub struct CoreHandle {
    commands: mpsc::Sender<Envelope>,
    shutdown: CancellationToken,
    interrupt: Arc<std::sync::Mutex<CancellationToken>>,
}
impl CoreHandle {
    pub fn submit(
        &self,
        command: CoreCommand,
    ) -> Result<oneshot::Receiver<Result<CommandReceipt, CoreError>>, CommandSendError> {
        let pause = matches!(&command, CoreCommand::Pause);
        let starts = matches!(
            &command,
            CoreCommand::Start(_)
                | CoreCommand::StartPrepared(_)
                | CoreCommand::AppendPrepared(_)
                | CoreCommand::Resume
                | CoreCommand::RetryFailed
                | CoreCommand::VerifyExisting
        );
        let (reply, receiver) = oneshot::channel();
        let mut interrupt = self
            .interrupt
            .lock()
            .map_err(|_| CommandSendError::Closed)?;
        let run_cancel = starts.then(|| interrupt.child_token());
        self.commands
            .try_send(Envelope {
                command,
                run_cancel,
                reply,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => CommandSendError::Busy,
                mpsc::error::TrySendError::Closed(_) => CommandSendError::Closed,
            })?;
        if pause {
            // Every command queued before Pause retains a child of the cancelled run group.
            interrupt.cancel();
            *interrupt = self.shutdown.child_token();
        }
        Ok(receiver)
    }
    pub fn request_shutdown(&self) -> Result<(), CommandSendError> {
        if self.commands.is_closed() {
            return Err(CommandSendError::Closed);
        }
        self.shutdown.cancel();
        Ok(())
    }
}
pub struct CoreSession {
    pub handle: CoreHandle,
    pub snapshots: watch::Receiver<Arc<AppSnapshot>>,
    worker: JoinHandle<Result<ShutdownReceipt, CoreError>>,
}
impl CoreSession {
    pub async fn join(self) -> Result<ShutdownReceipt, CoreError> {
        self.worker.await.map_err(|_| CoreError::WorkerStopped)?
    }
}
pub async fn spawn(config: CoreConfig) -> Result<CoreSession, CoreError> {
    spawn_with_completion(config, Arc::new(|_| {})).await
}
/// Registers a nonblocking final-transition sink, called after the terminal record is durable.
pub async fn spawn_with_completion(
    config: CoreConfig,
    completion: CompletionCallback,
) -> Result<CoreSession, CoreError> {
    spawn_with_services(config, completion, None).await
}
pub async fn spawn_with_services(
    config: CoreConfig,
    completion: CompletionCallback,
    logger: Option<Arc<SessionLogger>>,
) -> Result<CoreSession, CoreError> {
    spawn_worker(config, completion, logger, None, None).await
}
async fn spawn_worker(
    config: CoreConfig,
    completion: CompletionCallback,
    logger: Option<Arc<SessionLogger>>,
    budget: Option<Arc<budget::Budget>>,
    parent: Option<CancellationToken>,
) -> Result<CoreSession, CoreError> {
    let settings = load_settings(&config.paths).await?;
    if let Some(logger) = &logger {
        logger.set_debug(settings.debug_logging);
    }
    let recent = recent_tasks(&settings.recent_tasks).await;
    let selection = crate::selection::Selection::new();
    let initial = AppSnapshot {
        revision: 0,
        phase: UiPhase::Idle,
        selection: selection.view(),
        task: None,
        settings: Arc::new(settings.clone()),
        recent: Arc::from(recent),
        error: None,
        diagnostics: Diagnostics {
            log_directory: logger
                .as_ref()
                .map(|logger| logger.directory().to_path_buf()),
            logging: logger.as_ref().map(|logger| logger.status()),
        },
        queue: None,
    };
    let (snapshots_tx, snapshots) = watch::channel(Arc::new(initial));
    let (commands, receiver) = mpsc::channel(32);
    let shutdown = parent.unwrap_or_default();
    let interrupt = Arc::new(std::sync::Mutex::new(shutdown.child_token()));
    let worker = tokio::spawn(worker::run(
        config,
        settings,
        selection,
        snapshots_tx,
        receiver,
        shutdown.clone(),
        worker::Services {
            completion,
            logger,
            budget,
        },
    ));
    Ok(CoreSession {
        handle: CoreHandle {
            commands,
            shutdown,
            interrupt,
        },
        snapshots,
        worker,
    })
}
