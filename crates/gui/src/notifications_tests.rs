use super::*;
use hfdx_core::{
    notification::{NotificationContent, PlatformRequest},
    selection::Selection,
    storage::{Language, Settings, SettingsPaths},
    task::{CompletionOutcome, UiPhase},
};
use std::sync::Mutex;

fn notice() -> CompletionNotice {
    CompletionNotice {
        epoch: 1,
        task_id: serde_json::from_str("\"0123456789abcdef0123456789abcdef\"").unwrap(),
        outcome: CompletionOutcome::Verified,
        passed: 1,
        unavailable: 0,
        failed: 0,
    }
}
#[test]
fn bounded_completion_reports_overflow_and_drains_after_sender_drops() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let root = tempfile::tempdir().unwrap();
        let paths = SettingsPaths {
            settings_file: root.path().join("invalid.json"),
        };
        std::fs::write(&paths.settings_file, "invalid settings").unwrap();
        let services = Arc::new(Services::initialize(&paths).await);
        let initial = services.text(Language::English);
        assert!(services.logger.is_none());
        assert!(!initial.is_empty());
        let (callback, mut receiver) = callback(services.clone());
        for _ in 0..8 {
            callback(notice());
        }
        assert!(services.text(Language::English) == initial);
        callback(notice());
        let diagnostic = services.text(Language::English);
        assert!(diagnostic.contains(&initial));
        assert_eq!(diagnostic.lines().count(), initial.lines().count() + 1);
        callback(notice());
        assert_eq!(services.text(Language::English), diagnostic);
        assert_eq!(services.text(Language::English), diagnostic);
        drop(callback);
        let mut drained = 0;
        while receiver.recv().await.is_some() {
            drained += 1;
        }
        assert_eq!(drained, 8);
    });
}

#[derive(Default)]
struct Desktop(Mutex<Vec<PlatformRequest>>);
impl NotificationPlatform for Desktop {
    async fn deliver(&self, request: PlatformRequest) -> Result<(), NotificationError> {
        self.0.lock().unwrap().push(request);
        Ok(())
    }
}
#[test]
fn queued_notice_uses_current_watch_preferences() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let mut settings: Settings = serde_json::from_str(r#"{"endpoint":{"url":"https://huggingface.co","allow_http":false},"options":{"connections_per_file":4,"concurrent_files":5},"proxy_address":null,"recent_directories":[],"recent_tasks":[]}"#).unwrap();
        let snapshot = |settings| {
            Arc::new(AppSnapshot {
                revision: 0,
                phase: UiPhase::Idle,
                selection: Selection::new().view(),
                task: None,
                settings: Arc::new(settings),
                recent: Arc::from([]),
                error: None,
                diagnostics: Default::default(),
                queue: None,
            })
        };
        let (sender, receiver) = watch::channel(snapshot(settings.clone()));
        let desktop = Desktop::default();
        settings.language = Some(Language::ZhHant);
        settings.notify_on_completion = true;
        settings.sound_on_completion = true;
        sender.send_replace(snapshot(settings.clone()));
        let completed = notice();
        deliver(&desktop, &completed, &receiver).await.unwrap();
        assert_eq!(
            *desktop.0.lock().unwrap(),
            vec![PlatformRequest::Bubble {
                content: NotificationContent::from_notice(&completed, Language::ZhHant),
                sound: true,
            }]
        );
        settings.notify_on_completion = false;
        settings.sound_on_completion = false;
        sender.send_replace(snapshot(settings));
        deliver(&desktop, &completed, &receiver).await.unwrap();
        assert_eq!(desktop.0.lock().unwrap().len(), 1);
    });
}
#[test]
fn shutdown_aborts_stuck_notification_task_with_explicit_timeout() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (ready, entered) = tokio::sync::oneshot::channel();
        let mut worker = tokio::spawn(async move {
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        entered.await.unwrap();
        assert_eq!(finish(&mut worker).await, Err(NotificationError::TimedOut));
        assert!(worker.is_finished());
    });
}
#[test]
fn identity_failure_keeps_sound_only_and_reports_only_a_bubble() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let root = tempfile::tempdir().unwrap();
        let paths = SettingsPaths {
            settings_file: root.path().join("settings.json"),
        };
        let services = Arc::new(
            Services::with_platform(&paths, Err(NotificationError::IdentityUnavailable)).await,
        );
        let initial = services.text(Language::English);
        assert!(!initial.is_empty());
        let receiver = watch::channel(snapshot(settings(false, false))).1;
        assert!(
            deliver(&services.platform, &notice(), &receiver)
                .await
                .is_ok()
        );
        assert_eq!(services.text(Language::English), initial);
        let (sender, receiver) = watch::channel(snapshot(settings(false, true)));
        let desktop = Desktop::default();
        deliver(&desktop, &notice(), &receiver).await.unwrap();
        assert_eq!(*desktop.0.lock().unwrap(), vec![PlatformRequest::SoundOnly]);
        sender.send_replace(snapshot(settings(true, false)));
        let before = services.text(Language::English);
        #[cfg(windows)]
        {
            let Err(NotificationError::IdentityUnavailable) =
                deliver(&services.platform, &notice(), &receiver).await
            else {
                panic!("bubble needs registered identity")
            };
        }
        #[cfg(not(windows))]
        {
            deliver(&desktop, &notice(), &receiver).await.unwrap();
            assert!(matches!(
                desktop.0.lock().unwrap().last(),
                Some(PlatformRequest::Bubble { .. })
            ));
        }
        assert_eq!(services.text(Language::English), before);
    });
}
fn settings(notify: bool, sound: bool) -> Settings {
    let mut settings: Settings = serde_json::from_str(r#"{"endpoint":{"url":"https://huggingface.co","allow_http":false},"options":{"connections_per_file":4,"concurrent_files":5},"proxy_address":null,"recent_directories":[],"recent_tasks":[]}"#).unwrap();
    settings.notify_on_completion = notify;
    settings.sound_on_completion = sound;
    settings
}
fn snapshot(settings: Settings) -> Arc<AppSnapshot> {
    Arc::new(AppSnapshot {
        revision: 0,
        phase: UiPhase::Idle,
        selection: Selection::new().view(),
        task: None,
        settings: Arc::new(settings),
        recent: Arc::from([]),
        error: None,
        diagnostics: Default::default(),
        queue: None,
    })
}
