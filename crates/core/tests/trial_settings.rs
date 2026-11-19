use hfdx_core::storage::{Language, SettingsPaths, Theme, load_settings, save_settings};

#[tokio::test]
async fn platform_tokens_survive_restart_are_redacted_and_do_not_follow_other_origins() {
    use hfdx_core::{Endpoint, Token};
    let dir = tempfile::tempdir().unwrap();
    let paths = SettingsPaths {
        settings_file: dir.path().join("settings.json"),
    };
    let mut settings = load_settings(&paths).await.unwrap();
    settings.theme = Theme::Dark;
    save_settings(&paths, &settings).await.unwrap();
    let config = hfdx_core::task::CoreConfig {
        paths: paths.clone(),
        aria2_executable: "aria2c".into(),
    };
    let session = hfdx_core::task::spawn_queue_with_services(
        config.clone(),
        std::sync::Arc::new(|_| {}),
        None,
    )
    .await
    .unwrap();
    session
        .handle
        .submit(hfdx_core::task::CoreCommand::SavePlatformTokens {
            hf: Some(Token::parse("hf_fake_private").unwrap()),
            modelscope: Some(Token::parse("ms_fake_private").unwrap()),
        })
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    let reopened =
        hfdx_core::task::spawn_queue_with_services(config, std::sync::Arc::new(|_| {}), None)
            .await
            .unwrap();
    assert_eq!(reopened.snapshots.borrow().settings.theme, Theme::Dark);
    assert_eq!(
        reopened
            .snapshots
            .borrow()
            .settings
            .hf_token
            .as_ref()
            .map(Token::expose),
        Some("hf_fake_private")
    );
    assert_eq!(
        reopened
            .snapshots
            .borrow()
            .settings
            .modelscope_token
            .as_ref()
            .map(Token::expose),
        Some("ms_fake_private")
    );
    reopened.handle.request_shutdown().unwrap();
    reopened.join().await.unwrap();
    let loaded = load_settings(&paths).await.unwrap();
    assert!(!format!("{loaded:?}").contains("fake_private"));
    for (url, expected) in [
        ("https://huggingface.co", Some("hf_fake_private")),
        ("https://modelscope.ai", Some("ms_fake_private")),
        ("https://www.modelscope.ai", Some("ms_fake_private")),
        ("https://modelscope.cn", None),
        ("https://mirror.example", None),
        ("https://modelscope.ai.evil.test", None),
        ("http://huggingface.co", None),
        ("https://huggingface.co:8443", None),
    ] {
        assert_eq!(
            loaded
                .token_for(&Endpoint::parse(url, true).unwrap())
                .as_ref()
                .map(Token::expose),
            expected,
            "{url}"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&paths.settings_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let mut settings = loaded;
    settings.hf_token = None;
    save_settings(&paths, &settings).await.unwrap();
    assert!(load_settings(&paths).await.unwrap().hf_token.is_none());
    assert!(
        load_settings(&paths)
            .await
            .unwrap()
            .modelscope_token
            .is_some()
    );
    assert!(
        !std::fs::read_to_string(&paths.settings_file)
            .unwrap()
            .contains("hf_fake_private")
    );
}

#[tokio::test]
async fn old_settings_get_defaults_without_losing_existing_values() {
    let dir = tempfile::tempdir().unwrap();
    let paths = SettingsPaths {
        settings_file: dir.path().join("settings.json"),
    };
    std::fs::write(
        &paths.settings_file,
        r#"{
        "endpoint": {"url": "https://huggingface.co/", "allow_http": false},
        "options": {"connections_per_file": 3, "concurrent_files": 2},
        "proxy_address": null,
        "recent_directories": ["/existing"],
        "recent_tasks": []
    }"#,
    )
    .unwrap();

    let settings = load_settings(&paths).await.unwrap();
    assert!(settings.verify_after_download);
    assert!(settings.notify_on_completion);
    assert!(!settings.sound_on_completion);
    assert!(!settings.debug_logging);
    assert_eq!(settings.theme, Theme::System);
    assert_eq!(settings.language, None);
    assert_eq!(settings.options.connections_per_file(), 3);
    assert_eq!(
        settings.recent_directories,
        [std::path::PathBuf::from("/existing")]
    );
}

#[tokio::test]
async fn settings_choices_survive_save_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let paths = SettingsPaths {
        settings_file: dir.path().join("settings.json"),
    };
    let mut settings = load_settings(&paths).await.unwrap();
    settings.verify_after_download = false;
    settings.notify_on_completion = false;
    settings.sound_on_completion = true;
    settings.debug_logging = true;
    settings.theme = Theme::Dark;
    settings.language = Some(Language::ZhHant);
    save_settings(&paths, &settings).await.unwrap();

    let loaded = load_settings(&paths).await.unwrap();
    assert!(!loaded.verify_after_download);
    assert!(!loaded.notify_on_completion);
    assert!(loaded.sound_on_completion);
    assert!(loaded.debug_logging);
    assert_eq!(loaded.theme, Theme::Dark);
    assert_eq!(loaded.language, Some(Language::ZhHant));
}

#[test]
fn locale_resolution_uses_supported_chinese_variants_only() {
    for (locale, expected) in [
        ("zh_TW.UTF-8", Language::ZhHant),
        ("zh-Hant-HK", Language::ZhHant),
        ("zh-HK", Language::ZhHant),
        ("zh-MO", Language::ZhHant),
        ("zh_CN.UTF-8", Language::ZhHans),
        ("zh-SG", Language::ZhHans),
        ("zh-Hans-CN", Language::ZhHans),
        ("zh", Language::English),
        ("zh-MY", Language::English),
        ("en-US", Language::English),
    ] {
        assert_eq!(Language::from_locale(locale), expected, "{locale}");
    }
}
