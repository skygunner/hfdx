//! Best-effort completion delivery; the caller owns final-transition deduplication.
use crate::{
    storage::Language,
    task::{CompletionNotice, CompletionOutcome},
};
use std::{fmt, future::Future, time::Duration};

pub mod windows_identity;
pub use windows_identity::prepare_portable_identity;

#[derive(Clone, Copy, Debug)]
pub struct NotificationPreferences {
    pub notify: bool,
    pub sound: bool,
    pub language: Language,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationContent {
    pub title: String,
    pub body: String,
}
impl NotificationContent {
    pub fn from_notice(notice: &CompletionNotice, language: Language) -> Self {
        let outcome = match (language, notice.outcome) {
            (Language::English, CompletionOutcome::Verified) => {
                "Download complete; verification passed"
            }
            (Language::English, CompletionOutcome::VerificationDisabled) => {
                "Download complete; verification disabled"
            }
            (Language::English, CompletionOutcome::Unverified) => {
                "Download complete; some files could not be verified (no supported digest)"
            }
            (Language::English, CompletionOutcome::IntegrityFailed) => {
                "Download failed: integrity mismatch"
            }
            (Language::ZhHans, CompletionOutcome::Verified) => "下载完成，校验通过",
            (Language::ZhHans, CompletionOutcome::VerificationDisabled) => "下载完成，校验已关闭",
            (Language::ZhHans, CompletionOutcome::Unverified) => {
                "下载完成，部分文件未能校验（无支持的摘要）"
            }
            (Language::ZhHans, CompletionOutcome::IntegrityFailed) => {
                "下载失败：文件完整性校验不符"
            }
            (Language::ZhHant, CompletionOutcome::Verified) => "下載完成，驗證通過",
            (Language::ZhHant, CompletionOutcome::VerificationDisabled) => "下載完成，驗證已關閉",
            (Language::ZhHant, CompletionOutcome::Unverified) => {
                "下載完成，部分檔案未能驗證（無支援的摘要）"
            }
            (Language::ZhHant, CompletionOutcome::IntegrityFailed) => {
                "下載失敗：檔案完整性驗證不符"
            }
        };
        let (task, passed, unavailable, failed) = match language {
            Language::English => ("Task", "Verified", "Unverified", "Failed"),
            Language::ZhHans => ("任务", "通过", "未校验", "失败"),
            Language::ZhHant => ("任務", "通過", "未驗證", "失敗"),
        };
        Self {
            title: format!("hfdx: {outcome}"),
            body: format!(
                "{task}: {}\n{passed}: {} · {unavailable}: {} · {failed}: {}",
                notice.task_id.as_str(),
                notice.passed,
                notice.unavailable,
                notice.failed
            ),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformRequest {
    Bubble {
        content: NotificationContent,
        sound: bool,
    },
    SoundOnly,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Disabled,
    Suppressed,
    /// The OS accepted the request; policy may still suppress display/audio.
    Accepted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationError {
    ServiceUnavailable,
    PolicyUnavailable,
    Suppressed,
    UnsupportedSoundControl,
    IdentityUnavailable,
    IdentityConflict,
    PermissionDenied,
    DeliveryFailed,
    TimedOut,
    Busy,
    UnsupportedPlatform,
}
impl fmt::Display for NotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for NotificationError {}

/// A single native action: bubble audio belongs to the bubble, never a second call.
pub trait NotificationPlatform: Sync {
    fn deliver(
        &self,
        request: PlatformRequest,
    ) -> impl Future<Output = Result<(), NotificationError>> + Send;
}
/// Does not spawn detached work or mutate a task. Call once per finalized attempt.
/// Cancellation/timeout cannot retract an OS request already accepted; do not retry automatically.
pub async fn dispatch(
    platform: &impl NotificationPlatform,
    notice: &CompletionNotice,
    preferences: NotificationPreferences,
) -> Result<Delivery, NotificationError> {
    let request = match (preferences.notify, preferences.sound) {
        (false, false) => return Ok(Delivery::Disabled),
        (false, true) => PlatformRequest::SoundOnly,
        (true, sound) => PlatformRequest::Bubble {
            content: NotificationContent::from_notice(notice, preferences.language),
            sound,
        },
    };
    match tokio::time::timeout(Duration::from_secs(3), platform.deliver(request)).await {
        Ok(Ok(())) => Ok(Delivery::Accepted),
        Ok(Err(NotificationError::Suppressed)) => Ok(Delivery::Suppressed),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(NotificationError::TimedOut),
    }
}

pub struct NativePlatform {
    #[cfg(windows)]
    app_id: Option<String>,
    #[cfg(windows)]
    gate: std::sync::Arc<tokio::sync::Semaphore>,
}
impl NativePlatform {
    /// Windows bubbles require a registered AUMID; sound-only accepts None.
    /// This backend never writes registration, shortcuts, or system settings.
    pub fn new(_registered_windows_app_id: Option<String>) -> Self {
        Self {
            #[cfg(windows)]
            app_id: _registered_windows_app_id,
            #[cfg(windows)]
            gate: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}
impl NotificationPlatform for NativePlatform {
    async fn deliver(&self, request: PlatformRequest) -> Result<(), NotificationError> {
        #[cfg(target_os = "linux")]
        {
            match request {
                PlatformRequest::SoundOnly => linux_sound().await,
                PlatformRequest::Bubble { content, sound } => linux_bubble(&content, sound).await,
            }
        }
        #[cfg(windows)]
        {
            let app_id = self.app_id.clone().filter(|id| !id.is_empty());
            let permit = self
                .gate
                .clone()
                .try_acquire_owned()
                .map_err(|_| NotificationError::Busy)?;
            // A timed-out COM call retains its permit: no unbounded blocking threads.
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                match request {
                    PlatformRequest::SoundOnly => windows_sound(),
                    PlatformRequest::Bubble { content, sound } => {
                        let app_id = app_id
                            .as_deref()
                            .ok_or(NotificationError::IdentityUnavailable)?;
                        windows_bubble(app_id, &content, sound)
                    }
                }
            })
            .await
            .map_err(|_| NotificationError::DeliveryFailed)?
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = request;
            Err(NotificationError::UnsupportedPlatform)
        }
    }
}
#[cfg(windows)]
fn windows_sound() -> Result<(), NotificationError> {
    use windows::{
        Win32::{
            Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT, SND_SYSTEM},
            UI::Shell::{QUNS_ACCEPTS_NOTIFICATIONS, SHQueryUserNotificationState},
        },
        core::w,
    };
    // SAFETY: generated wrapper initializes its output; no app registration is queried or written.
    let state = unsafe { SHQueryUserNotificationState() }
        .map_err(|_| NotificationError::PolicyUnavailable)?;
    if state != QUNS_ACCEPTS_NOTIFICATIONS {
        return Err(NotificationError::Suppressed);
    }
    // SAFETY: static terminated alias; system notification session, no loop, file, or memory playback.
    let played = unsafe {
        PlaySoundW(
            w!("Notification.Default"),
            None,
            SND_ALIAS | SND_ASYNC | SND_NODEFAULT | SND_SYSTEM,
        )
    };
    if played.as_bool() {
        Ok(())
    } else {
        Err(NotificationError::DeliveryFailed)
    }
}
#[cfg(target_os = "linux")]
async fn linux_sound() -> Result<(), NotificationError> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|_| NotificationError::ServiceUnavailable)?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    .map_err(|_| NotificationError::ServiceUnavailable)?;
    let inhibited: bool = proxy
        .get_property("Inhibited")
        .await
        .map_err(|_| NotificationError::PolicyUnavailable)?;
    if inhibited {
        return Err(NotificationError::Suppressed);
    }
    let status = tokio::process::Command::new("canberra-gtk-play")
        .args([
            "--id=complete",
            "--description=hfdx download complete",
            "--loop=1",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|_| NotificationError::ServiceUnavailable)?;
    if status.success() {
        Ok(())
    } else {
        Err(NotificationError::DeliveryFailed)
    }
}
#[cfg(target_os = "linux")]
async fn linux_bubble(content: &NotificationContent, sound: bool) -> Result<(), NotificationError> {
    use std::collections::HashMap;
    use zbus::{Connection, Proxy, zvariant::Value};
    let connection = Connection::session()
        .await
        .map_err(|_| NotificationError::ServiceUnavailable)?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    .map_err(|_| NotificationError::ServiceUnavailable)?;
    // Bubble and sound are independent. Missing "sound" must not cancel Notify.
    let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await.unwrap_or_default();
    let sound_capable = capabilities.iter().any(|cap| cap == "sound");
    let mut hints = HashMap::from([("transient", Value::from(true))]);
    if sound && sound_capable {
        hints.insert("sound-name", Value::from("complete"));
    } else if sound_capable {
        hints.insert("suppress-sound", Value::from(true));
    }
    let _: u32 = proxy
        .call(
            "Notify",
            &(
                "hfdx",
                0u32,
                "",
                &content.title,
                &content.body,
                Vec::<&str>::new(),
                hints,
                -1i32,
            ),
        )
        .await
        .map_err(|_| NotificationError::DeliveryFailed)?;
    Ok(())
}
#[cfg(windows)]
fn windows_bubble(
    app_id: &str,
    content: &NotificationContent,
    sound: bool,
) -> Result<(), NotificationError> {
    use windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{NotificationSetting, ToastNotification, ToastNotificationManager},
        core::HSTRING,
    };
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))
        .map_err(|_| NotificationError::IdentityUnavailable)?;
    if notifier
        .Setting()
        .map_err(|_| NotificationError::IdentityUnavailable)?
        != NotificationSetting::Enabled
    {
        return Err(NotificationError::PermissionDenied);
    }
    let send = || -> windows::core::Result<()> {
        let xml = XmlDocument::new()?;
        let audio = if sound {
            "<audio src=\"ms-winsoundevent:Notification.Default\"/>"
        } else {
            "<audio silent=\"true\"/>"
        };
        xml.LoadXml(&HSTRING::from(format!(
            "<toast duration=\"short\"><visual><binding template=\"ToastGeneric\"><text/><text/></binding></visual>{audio}</toast>"
        )))?;
        for (xpath, text) in [
            ("/toast/visual/binding/text[1]", &content.title),
            ("/toast/visual/binding/text[2]", &content.body),
        ] {
            xml.SelectSingleNode(&HSTRING::from(xpath))?
                .SetInnerText(&HSTRING::from(text))?;
        }
        notifier.Show(&ToastNotification::CreateToastNotification(&xml)?)
    };
    send().map_err(|_| NotificationError::DeliveryFailed)
}
