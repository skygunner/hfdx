use hfdx_core::repository::{Endpoint, RepoId};
use hfdx_core::storage::{
    DownloadOptions, SettingsPaths, default_destination, load_settings, save_settings,
};
#[tokio::test]
async fn endpoint_environment_applies_only_without_saved_settings() {
    if let Some(root) = std::env::var_os("HFDX_ENDPOINT_TEST_ROOT") {
        let paths = SettingsPaths {
            settings_file: std::path::PathBuf::from(root).join("settings.json"),
        };
        let mode = std::env::var("HFDX_ENDPOINT_TEST_CASE").unwrap();
        match mode.as_str() {
            "https" => assert_eq!(
                load_settings(&paths).await.unwrap().endpoint,
                Endpoint::parse("https://mirror.example", false).unwrap()
            ),
            "default" => assert_eq!(
                load_settings(&paths).await.unwrap().endpoint,
                Endpoint::parse("https://huggingface.co", false).unwrap()
            ),
            "invalid" => assert!(load_settings(&paths).await.is_err()),
            "saved" => assert_eq!(
                load_settings(&paths).await.unwrap().endpoint,
                Endpoint::parse("https://saved.example", false).unwrap()
            ),
            _ => panic!("unknown test case"),
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let paths = SettingsPaths {
        settings_file: dir.path().join("settings.json"),
    };
    for (mode, endpoint) in [
        ("default", None),
        ("https", Some("https://mirror.example")),
        ("invalid", Some("http://mirror.example")),
        ("invalid", Some("not-a-url")),
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "endpoint_environment_applies_only_without_saved_settings",
                "--nocapture",
            ])
            .env("HFDX_ENDPOINT_TEST_ROOT", dir.path())
            .env("HFDX_ENDPOINT_TEST_CASE", mode)
            .env_remove("HF_ENDPOINT");
        if let Some(value) = endpoint {
            command.env("HF_ENDPOINT", value);
        }
        assert!(command.status().unwrap().success(), "child failed: {mode}");
    }
    let saved = hfdx_core::storage::Settings {
        hf_token: None,
        modelscope_token: None,
        endpoint: Endpoint::parse("https://saved.example", false).unwrap(),
        options: DownloadOptions::default(),
        proxy_address: None,
        recent_directories: vec![],
        recent_tasks: vec![],
        verify_after_download: true,
        notify_on_completion: true,
        sound_on_completion: false,
        auto_update_check: false,
        update_interval: Default::default(),
        debug_logging: false,
        theme: hfdx_core::storage::Theme::System,
        language: None,
    };
    save_settings(&paths, &saved).await.unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "endpoint_environment_applies_only_without_saved_settings",
            "--nocapture",
        ])
        .env("HFDX_ENDPOINT_TEST_ROOT", dir.path())
        .env("HFDX_ENDPOINT_TEST_CASE", "saved")
        .env("HF_ENDPOINT", "http://invalid.example")
        .status()
        .unwrap();
    assert!(status.success());
}
#[tokio::test]
async fn settings_are_replaced_and_recent_lists_are_limited() {
    let dir = tempfile::tempdir().unwrap();
    let paths = SettingsPaths {
        settings_file: dir.path().join("settings.json"),
    };
    let mut settings = load_settings(&paths).await.unwrap();
    assert_eq!(settings.options.connections_per_file(), 4);
    settings.recent_directories = (0..12)
        .map(|n| dir.path().join(format!("dir{n}")))
        .collect();
    settings.recent_tasks = (0..12)
        .map(|n| dir.path().join(format!("task{n}")))
        .collect();
    save_settings(&paths, &settings).await.unwrap();
    settings.options = DownloadOptions::new(7, 3).unwrap();
    save_settings(&paths, &settings).await.unwrap();
    let loaded = load_settings(&paths).await.unwrap();
    assert_eq!(loaded.options, settings.options);
    assert_eq!(loaded.recent_directories, settings.recent_directories[..10]);
    assert_eq!(loaded.recent_tasks, settings.recent_tasks[..10]);
    assert_eq!(
        default_destination(&loaded, &RepoId::parse("org/model").unwrap()).unwrap(),
        settings.recent_directories[0].join("model")
    );
}
