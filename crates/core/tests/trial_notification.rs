use hfdx_core::{
    notification::{
        Delivery, NotificationContent, NotificationError, NotificationPlatform,
        NotificationPreferences, PlatformRequest, dispatch, prepare_portable_identity,
    },
    storage::Language,
    task::{CompletionNotice, CompletionOutcome},
};
use std::sync::Mutex;

#[derive(Default)]
struct FakeDesktop {
    requests: Mutex<Vec<PlatformRequest>>,
    failure: Option<NotificationError>,
}
impl NotificationPlatform for FakeDesktop {
    async fn deliver(&self, request: PlatformRequest) -> Result<(), NotificationError> {
        self.requests.lock().unwrap().push(request);
        match self.failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
fn notice(outcome: CompletionOutcome) -> CompletionNotice {
    CompletionNotice {
        epoch: 9,
        task_id: serde_json::from_str("\"0123456789abcdef0123456789abcdef\"").unwrap(),
        outcome,
        passed: 17,
        unavailable: 23,
        failed: 31,
    }
}

#[tokio::test]
async fn independent_settings_deliver_at_most_one_bubble_and_one_sound() {
    for (notify, sound) in [(false, false), (false, true), (true, false), (true, true)] {
        let desktop = FakeDesktop::default();
        let completed = notice(CompletionOutcome::Unverified);

        let result = dispatch(
            &desktop,
            &completed,
            NotificationPreferences {
                notify,
                sound,
                language: Language::English,
            },
        )
        .await
        .unwrap();

        let requests = desktop.requests.lock().unwrap();
        match (notify, sound) {
            (false, false) => {
                assert_eq!(result, Delivery::Disabled);
                assert!(requests.is_empty());
            }
            (false, true) => {
                assert_eq!(result, Delivery::Accepted);
                assert_eq!(*requests, [PlatformRequest::SoundOnly]);
            }
            (true, expected_sound) => {
                assert_eq!(result, Delivery::Accepted);
                assert_eq!(requests.len(), 1);
                let PlatformRequest::Bubble { content, sound } = &requests[0] else {
                    panic!("notification enabled must deliver a bubble");
                };
                assert_eq!(*sound, expected_sound);
                assert!(content.body.contains(completed.task_id.as_str()));
                for count in ["17", "23", "31"] {
                    assert!(content.body.contains(count));
                }
            }
        }
    }
}

#[tokio::test]
async fn delivery_errors_are_reported_without_retry_or_changing_completion() {
    for error in [
        NotificationError::ServiceUnavailable,
        NotificationError::PermissionDenied,
        NotificationError::IdentityUnavailable,
        NotificationError::DeliveryFailed,
        NotificationError::UnsupportedSoundControl,
        NotificationError::Busy,
    ] {
        let desktop = FakeDesktop {
            failure: Some(error),
            ..FakeDesktop::default()
        };
        let completed = notice(CompletionOutcome::IntegrityFailed);

        let result = dispatch(
            &desktop,
            &completed,
            NotificationPreferences {
                notify: true,
                sound: true,
                language: Language::English,
            },
        )
        .await;

        assert_eq!(result, Err(error));
        assert_eq!(desktop.requests.lock().unwrap().len(), 1);
        assert_eq!(completed.outcome, CompletionOutcome::IntegrityFailed);
        assert_eq!((completed.epoch, completed.failed), (9, 31));
    }
}

#[test]
fn localized_completion_content_preserves_outcome_and_counts() {
    for (language, labels, meanings) in [
        (
            Language::English,
            ["Verified", "Unverified", "Failed"],
            [
                "verification passed",
                "verification disabled",
                "no supported digest",
                "integrity mismatch",
            ],
        ),
        (
            Language::ZhHans,
            ["通过", "未校验", "失败"],
            ["校验通过", "校验已关闭", "无支持的摘要", "校验不符"],
        ),
        (
            Language::ZhHant,
            ["通過", "未驗證", "失敗"],
            ["驗證通過", "驗證已關閉", "無支援的摘要", "驗證不符"],
        ),
    ] {
        for (outcome, meaning) in [
            CompletionOutcome::Verified,
            CompletionOutcome::VerificationDisabled,
            CompletionOutcome::Unverified,
            CompletionOutcome::IntegrityFailed,
        ]
        .into_iter()
        .zip(meanings)
        {
            let completed = notice(outcome);

            let content = NotificationContent::from_notice(&completed, language);

            assert!(content.title.contains(meaning), "{language:?} {outcome:?}");
            assert!(content.body.contains(completed.task_id.as_str()));
            for (label, count) in labels.into_iter().zip([17, 23, 31]) {
                assert!(content.body.contains(&format!("{label}: {count}")));
            }
        }
    }
}

#[tokio::test]
async fn desktop_policy_suppression_is_distinct_from_delivery_failure() {
    let desktop = FakeDesktop {
        failure: Some(NotificationError::Suppressed),
        ..FakeDesktop::default()
    };
    let result = dispatch(
        &desktop,
        &notice(CompletionOutcome::Verified),
        NotificationPreferences {
            notify: false,
            sound: true,
            language: Language::English,
        },
    )
    .await;
    assert_eq!(result, Ok(Delivery::Suppressed));
    assert_eq!(
        *desktop.requests.lock().unwrap(),
        [PlatformRequest::SoundOnly]
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_native_sound_obeys_policy_and_reports_player_failure() {
    use std::{
        os::unix::fs::PermissionsExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    if std::env::var_os("HFDX_SOUND_CHILD").is_none() {
        let dir = tempfile::tempdir().unwrap();
        let player = dir.path().join("canberra-gtk-play");
        std::fs::write(&player, "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$HFDX_SOUND_RECORD\"\ntest ! -f \"$HFDX_SOUND_FAIL\"\n").unwrap();
        std::fs::set_permissions(&player, std::fs::Permissions::from_mode(0o700)).unwrap();
        let status = tokio::process::Command::new("dbus-run-session")
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "linux_native_sound_obeys_policy_and_reports_player_failure",
                "--nocapture",
            ])
            .env("HFDX_SOUND_CHILD", "1")
            .env("HFDX_SOUND_RECORD", dir.path().join("calls"))
            .env("HFDX_SOUND_FAIL", dir.path().join("fail"))
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(dir.path().to_path_buf())
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )
                .unwrap(),
            )
            .kill_on_drop(true)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        return;
    }
    struct Policy(Arc<AtomicBool>);
    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl Policy {
        #[zbus(property)]
        fn inhibited(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
    let inhibited = Arc::new(AtomicBool::new(true));
    let service = zbus::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at("/org/freedesktop/Notifications", Policy(inhibited.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let native = prepare_portable_identity(false).await.unwrap();
    assert!(matches!(
        prepare_portable_identity(true).await,
        Err(NotificationError::UnsupportedPlatform)
    ));
    let completed = notice(CompletionOutcome::Verified);
    let preferences = NotificationPreferences {
        notify: false,
        sound: true,
        language: Language::English,
    };
    let record = std::path::PathBuf::from(std::env::var_os("HFDX_SOUND_RECORD").unwrap());

    assert_eq!(
        dispatch(&native, &completed, preferences).await,
        Ok(Delivery::Suppressed)
    );
    assert!(!record.exists());
    inhibited.store(false, Ordering::SeqCst);
    assert_eq!(
        dispatch(&native, &completed, preferences).await,
        Ok(Delivery::Accepted)
    );
    assert_eq!(
        std::fs::read_to_string(&record)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "--id=complete",
            "--description=hfdx download complete",
            "--loop=1"
        ]
    );
    std::fs::write(std::env::var_os("HFDX_SOUND_FAIL").unwrap(), b"fail").unwrap();
    assert_eq!(
        dispatch(&native, &completed, preferences).await,
        Err(NotificationError::DeliveryFailed)
    );
    service
        .object_server()
        .remove::<Policy, _>("/org/freedesktop/Notifications")
        .await
        .unwrap();
    assert_eq!(
        dispatch(&native, &completed, preferences).await,
        Err(NotificationError::PolicyUnavailable)
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_bubble_is_delivered_when_service_lacks_sound_capability() {
    use std::{collections::HashMap, future::poll_fn, pin::Pin};
    use zbus::{export::futures_core::Stream, zvariant::OwnedValue};

    if std::env::var_os("HFDX_BUBBLE_CHILD").is_none() {
        let status = tokio::process::Command::new("dbus-run-session")
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "linux_bubble_is_delivered_when_service_lacks_sound_capability",
                "--nocapture",
            ])
            .env("HFDX_BUBBLE_CHILD", "1")
            .kill_on_drop(true)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        return;
    }

    let service = zbus::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .build()
        .await
        .unwrap();
    let mut messages = zbus::MessageStream::from(&service);
    let received = tokio::spawn(async move {
        while let Some(message) = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await {
            let message = message.unwrap();
            let header = message.header();
            if header.message_type() != zbus::message::Type::MethodCall {
                continue;
            }
            match header.member().map(|member| member.as_str()) {
                Some("GetCapabilities") => {
                    service
                        .reply(&header, &vec!["body", "actions"])
                        .await
                        .unwrap();
                }
                Some("Notify") => {
                    type NotifyCall = (
                        String,
                        u32,
                        String,
                        String,
                        String,
                        Vec<String>,
                        HashMap<String, OwnedValue>,
                        i32,
                    );
                    let (_, _, _, title, body, _, hints, _): NotifyCall =
                        message.body().deserialize().unwrap();
                    service.reply(&header, &1u32).await.unwrap();
                    return (title, body, hints);
                }
                _ => {}
            }
        }
        panic!("notification service closed before Notify");
    });
    let native = prepare_portable_identity(false).await.unwrap();
    let completed = notice(CompletionOutcome::Verified);

    let result = dispatch(
        &native,
        &completed,
        NotificationPreferences {
            notify: true,
            sound: false,
            language: Language::English,
        },
    )
    .await;

    assert_eq!(result, Ok(Delivery::Accepted));
    let (title, body, hints) = tokio::time::timeout(std::time::Duration::from_secs(3), received)
        .await
        .unwrap()
        .unwrap();
    let expected = NotificationContent::from_notice(&completed, Language::English);
    assert_eq!(title, expected.title);
    assert_eq!(body, expected.body);
    assert!(!hints.contains_key("sound-name") && !hints.contains_key("suppress-sound"));
}

struct HungDesktop;
impl NotificationPlatform for HungDesktop {
    async fn deliver(&self, _: PlatformRequest) -> Result<(), NotificationError> {
        std::future::pending().await
    }
}
#[tokio::test]
async fn unresponsive_platform_is_bounded_by_the_delivery_deadline() {
    let completed = notice(CompletionOutcome::Verified);

    let result = dispatch(
        &HungDesktop,
        &completed,
        NotificationPreferences {
            notify: true,
            sound: false,
            language: Language::English,
        },
    )
    .await;

    assert_eq!(result, Err(NotificationError::TimedOut));
}
