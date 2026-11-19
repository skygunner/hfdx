use super::services::Services;
use hfdx_core::{
    notification::{self, NotificationError, NotificationPlatform, NotificationPreferences},
    task::{AppSnapshot, CompletionCallback, CompletionNotice},
};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

pub fn callback(services: Arc<Services>) -> (CompletionCallback, mpsc::Receiver<CompletionNotice>) {
    let (sender, receiver) = mpsc::channel(8);
    let callback = Arc::new(move |notice| {
        if let Err(error) = sender.try_send(notice) {
            let (notice, reason) = match error {
                mpsc::error::TrySendError::Full(notice) => (notice, NotificationError::Busy),
                mpsc::error::TrySendError::Closed(notice) => {
                    (notice, NotificationError::ServiceUnavailable)
                }
            };
            services.notification_failed(Some(&notice), reason);
        }
    });
    (callback, receiver)
}
pub async fn receive(
    mut receiver: mpsc::Receiver<CompletionNotice>,
    snapshots: watch::Receiver<Arc<AppSnapshot>>,
    services: Arc<Services>,
) {
    while let Some(notice) = receiver.recv().await {
        if let Err(error) = deliver(&services.platform, &notice, &snapshots).await {
            services.notification_failed(Some(&notice), error);
        }
    }
}
async fn deliver(
    platform: &impl NotificationPlatform,
    notice: &CompletionNotice,
    snapshots: &watch::Receiver<Arc<AppSnapshot>>,
) -> Result<(), NotificationError> {
    let preferences = {
        let snapshot = snapshots.borrow();
        NotificationPreferences {
            notify: snapshot.settings.notify_on_completion,
            sound: snapshot.settings.sound_on_completion,
            language: snapshot.settings.effective_language(),
        }
    };
    notification::dispatch(platform, notice, preferences)
        .await
        .map(|_| ())
}
pub async fn finish(worker: &mut JoinHandle<()>) -> Result<(), NotificationError> {
    // One active delivery plus eight queued; core dispatch bounds each to three seconds.
    match tokio::time::timeout(Duration::from_secs(28), &mut *worker).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(NotificationError::DeliveryFailed),
        Err(_) => {
            worker.abort();
            match worker.await {
                Err(error) if error.is_cancelled() => {}
                Ok(()) => {}
                Err(_) => return Err(NotificationError::DeliveryFailed),
            }
            Err(NotificationError::TimedOut)
        }
    }
}

#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests;
