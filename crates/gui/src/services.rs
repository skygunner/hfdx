use hfdx_core::{
    locale::{self, Message},
    logging::{
        self, EventKind, LogConfig, LogError, LogEvent, LogLevel, LogOutcome, SessionLogger,
    },
    notification::{NativePlatform, NotificationError},
    storage::{Language, SettingsPaths, load_settings},
    task::CompletionNotice,
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Diagnostic {
    Logging(LogError),
    Notification(NotificationError),
}
pub struct Services {
    pub logger: Option<Arc<SessionLogger>>,
    pub platform: NativePlatform,
    diagnostics: Mutex<Vec<Diagnostic>>,
    pub changed: tokio::sync::Notify,
}
impl Services {
    #[cfg(test)]
    pub async fn initialize(paths: &SettingsPaths) -> Self {
        Self::with_platform(
            paths,
            hfdx_core::notification::prepare_portable_identity(false).await,
        )
        .await
    }
    pub async fn with_platform(
        paths: &SettingsPaths,
        platform: Result<NativePlatform, NotificationError>,
    ) -> Self {
        let result = async {
            let settings = load_settings(paths)
                .await
                .map_err(|_| LogError::DirectoryUnavailable)?;
            SessionLogger::new(
                LogConfig::new(logging::default_log_directory()?)
                    .with_debug(settings.debug_logging),
            )
        }
        .await;
        let (logger, mut diagnostics) = match result {
            Ok(logger) => (Some(Arc::new(logger)), Vec::new()),
            Err(error) => (None, vec![Diagnostic::Logging(error)]),
        };
        if let Err(error) = &platform {
            diagnostics.push(Diagnostic::Notification(*error));
        }
        Self {
            logger,
            platform: platform.unwrap_or_else(|_| NativePlatform::new(None)),
            diagnostics: Mutex::new(diagnostics),
            changed: tokio::sync::Notify::new(),
        }
    }
    fn record(&self, value: Diagnostic) {
        match self.diagnostics.lock() {
            Ok(mut diagnostics) => {
                if !diagnostics.contains(&value) {
                    diagnostics.push(value);
                }
            }
            Err(error) => eprintln!("GUI diagnostics unavailable: {error}"),
        }
        self.changed.notify_one();
    }
    pub fn notification_failed(
        &self,
        notice: Option<&CompletionNotice>,
        category: NotificationError,
    ) {
        self.record(Diagnostic::Notification(category));
        if let Some(logger) = &self.logger
            && let Err(error) = logger.record(
                LogLevel::Warn,
                LogEvent {
                    event: EventKind::NotificationFailed,
                    task_id: notice.map(|n| &n.task_id),
                    outcome: LogOutcome::Unavailable,
                    count: 1,
                    bytes: 0,
                    elapsed_ms: 0,
                },
            )
        {
            self.record(Diagnostic::Logging(error));
        }
    }
    pub fn text(&self, language: Language) -> String {
        let diagnostics = match self.diagnostics.lock() {
            Ok(diagnostics) => diagnostics,
            Err(_) => return locale::text(language, Message::InternalError).to_owned(),
        };
        let language_index = match language {
            Language::English => 0,
            Language::ZhHans => 1,
            Language::ZhHant => 2,
        };
        diagnostics.iter().map(|value| {
            let messages = match value {
                Diagnostic::Logging(LogError::DirectoryUnavailable) => ["Logging unavailable: log directory unavailable; task continues", "日志不可用：日志目录不可用；任务继续运行", "記錄無法使用：記錄目錄無法使用；工作繼續執行"],
                Diagnostic::Logging(_) => ["Logging unavailable: write or synchronization failed; task continues", "日志不可用：写入或同步失败；任务继续运行", "記錄無法使用：寫入或同步失敗；工作繼續執行"],
                Diagnostic::Notification(NotificationError::IdentityUnavailable) => ["Windows notification identity unavailable. Run hfdx-gui --register-notifications explicitly to register; sound-only remains available.", "Windows 通知身份不可用。请显式运行 hfdx-gui --register-notifications 注册；仅声音仍可使用。", "Windows 通知身分無法使用。請明確執行 hfdx-gui --register-notifications 註冊；僅聲音仍可使用。"],
                Diagnostic::Notification(NotificationError::IdentityConflict) => ["Windows notification shortcut conflicts with this executable. Check the old installation and shortcut target; registration will not overwrite it.", "Windows 通知快捷方式与当前程序冲突。请检查旧安装和快捷方式目标；注册不会覆盖冲突。", "Windows 通知捷徑與目前程式衝突。請檢查舊安裝和捷徑目標；註冊不會覆寫衝突。"],
                Diagnostic::Notification(NotificationError::Busy) => ["Notification queue full; see task result", "通知队列已满；请查看任务结果", "通知佇列已滿；請查看工作結果"],
                Diagnostic::Notification(NotificationError::TimedOut) => ["Notification timed out; see task result", "通知服务超时；请查看任务结果", "通知服務逾時；請查看工作結果"],
                Diagnostic::Notification(NotificationError::PermissionDenied | NotificationError::Suppressed) => ["System policy or permission blocked notification; see task result", "系统策略或权限阻止通知；请查看任务结果", "系統原則或權限封鎖通知；請查看工作結果"],
                Diagnostic::Notification(NotificationError::UnsupportedSoundControl | NotificationError::UnsupportedPlatform) => ["Requested notification capability unavailable; see task result", "所需通知能力不可用；请查看任务结果", "所需通知能力無法使用；請查看工作結果"],
                Diagnostic::Notification(NotificationError::ServiceUnavailable | NotificationError::PolicyUnavailable | NotificationError::DeliveryFailed) => ["Notification service unavailable; see task result", "通知服务不可用；请查看任务结果", "通知服務無法使用；請查看工作結果"],
            };
            messages[language_index]
        }).collect::<Vec<_>>().join("\n")
    }
    pub fn flush(&self) {
        if let Some(logger) = &self.logger
            && let Err(error) = logger.flush()
        {
            self.record(Diagnostic::Logging(error));
        }
    }
}
