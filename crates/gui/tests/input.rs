#[path = "../src/input.rs"]
mod input;
slint::include_modules!();

use hfdx_core::storage::{DownloadOptions, Settings, Theme};
fn saved_platform_tokens_are_independent_and_modelscope_urls_load_without_old_hf_credentials(
    ui: &AppWindow,
) {
    ui.set_token("".into());
    ui.set_revision("main".into());
    ui.set_allow_http(false);
    ui.set_endpoint("https://huggingface.co".into());
    ui.set_hf_token("hf_fake_saved".into());
    ui.set_modelscope_token("ms_fake_saved".into());
    assert_eq!(
        input::access(ui).unwrap().token.unwrap().expose(),
        "hf_fake_saved"
    );
    ui.set_repository("https://modelscope.ai/models/Qwen/model".into());
    let hfdx_core::task::CoreCommand::Load(load) = input::load(ui).unwrap() else {
        panic!("load")
    };
    assert_eq!(load.access.token.unwrap().expose(), "ms_fake_saved");
    assert_eq!(load.repository.requested_revision.as_str(), "master");
    ui.set_endpoint("https://mirror.example".into());
    assert!(input::access(ui).unwrap().token.is_none());
}
#[test]
fn endpoint_change_clears_token_before_load() {
    let ui = AppWindow::new().expect("native window");
    ui.set_endpoint("https://huggingface.co/".into());
    ui.set_token("old-source-secret".into());
    ui.set_repository("fixture/repo".into());
    let kept = input::load(&ui).expect("trailing slash is the same endpoint");
    assert!(
        matches!(kept, hfdx_core::task::CoreCommand::Load(request) if request.access.token.is_some())
    );
    assert!(!ui.get_token().is_empty());
    ui.set_allow_http(true);
    ui.set_repository("http://127.0.0.1:1/fixture/repo".into());
    ui.set_token("old-source-secret".into());
    let Err(error) = input::load(&ui) else {
        panic!("old token must not travel");
    };
    assert!(!error.is_empty());
    assert!(ui.get_token().is_empty());
    ui.set_token("new-source-secret".into());
    let command = input::load(&ui).expect("explicit replacement token allowed");
    assert!(
        matches!(command, hfdx_core::task::CoreCommand::Load(request) if request.access.token.is_some())
    );
    let current = Settings {
        hf_token: None,
        modelscope_token: None,
        endpoint: hfdx_core::repository::Endpoint::parse("https://huggingface.co", false)
            .expect("default endpoint"),
        options: DownloadOptions::default(),
        proxy_address: None,
        recent_directories: Vec::new(),
        recent_tasks: vec![std::path::PathBuf::from("/previous/.hfdx/task.json")],
        verify_after_download: true,
        notify_on_completion: true,
        sound_on_completion: false,
        debug_logging: false,
        theme: Theme::System,
        language: None,
    };
    let mut snapshot = hfdx_core::task::AppSnapshot {
        revision: 0,
        phase: hfdx_core::task::UiPhase::Idle,
        selection: hfdx_core::selection::Selection::new().view(),
        task: None,
        settings: std::sync::Arc::new(current.clone()),
        recent: std::sync::Arc::from([]),
        error: None,
        diagnostics: Default::default(),
        queue: None,
    };
    assert!(
        input::start(&ui, &snapshot).is_err(),
        "Start must reject an unloaded catalog"
    );
    ui.set_endpoint("http://127.0.0.1:2".into());
    let hfdx_core::task::CoreCommand::SetAccess { endpoint, access } =
        input::set_access(&ui).expect("visible access endpoint")
    else {
        panic!("access command")
    };
    assert_eq!(endpoint.url().as_str(), "http://127.0.0.1:2/");
    assert!(access.token.is_some());
    ui.set_allow_http(false);
    assert!(
        input::set_access(&ui).is_err(),
        "HTTP access requires current consent"
    );
    ui.set_allow_http(true);
    ui.set_repository("org/repo".into());
    assert_eq!(
        input::recent_destination(&ui, std::path::Path::new("/downloads")).expect("base path"),
        std::path::PathBuf::from("/downloads/repo")
    );
    ui.set_destination("/downloads/repo".into());
    ui.set_verify(false);
    ui.set_notify(false);
    ui.set_sound(true);
    ui.set_debug(true);
    ui.set_theme_index(2);
    ui.set_language_index(2);
    ui.set_proxy("http://fixture:secret@localhost:3128".into());
    let hfdx_core::task::CoreCommand::SaveSettings(settings) =
        input::settings(&ui, &current).expect("settings parsed")
    else {
        panic!("save command")
    };
    assert_eq!(settings.recent_tasks, current.recent_tasks);
    assert!(!settings.verify_after_download);
    assert!(!settings.notify_on_completion);
    assert!(settings.sound_on_completion);
    assert!(settings.debug_logging);
    assert_eq!(settings.theme, Theme::Dark);
    assert_eq!(
        settings.language,
        Some(hfdx_core::storage::Language::ZhHant)
    );
    assert!(ui.get_token().as_str() == "new-source-secret");
    let stored = serde_json::to_string(&settings).expect("persistable settings");
    assert!(!stored.contains("secret"));
    assert_eq!(
        settings.recent_directories,
        vec![std::path::PathBuf::from("/downloads")]
    );

    let mut selection = hfdx_core::selection::Selection::new();
    let catalog = selection
        .begin(hfdx_core::repository::RepositorySpec {
            request: hfdx_core::repository::RepositoryRequest {
                endpoint: current.endpoint.clone(),
                kind: hfdx_core::repository::RepoKind::Model,
                repo_id: hfdx_core::repository::RepoId::parse("org/repo").unwrap(),
                requested_revision: hfdx_core::repository::Revision::parse("main").unwrap(),
            },
            resolved_commit: hfdx_core::repository::CommitId::parse(&"a".repeat(40)).unwrap(),
        })
        .unwrap();
    selection
        .append(hfdx_core::repository::CatalogPage {
            catalog: catalog.id,
            entries: vec![],
            complete: true,
        })
        .unwrap();
    snapshot.selection = selection.view();
    ui.set_task_connections(6);
    ui.set_task_concurrent_files(2);
    ui.set_task_verify(true);
    let hfdx_core::task::CoreCommand::Start(request) = input::start(&ui, &snapshot).unwrap() else {
        panic!("start command")
    };
    assert_eq!(request.options, DownloadOptions::new(6, 2).unwrap());
    assert!(request.verify_after_download);
    assert!(
        !ui.get_verify(),
        "per-task options leave global defaults alone"
    );
    assert_eq!(snapshot.settings.options, DownloadOptions::default());
    ui.set_task_connections(11);
    assert!(input::start(&ui, &snapshot).is_err());
    saved_platform_tokens_are_independent_and_modelscope_urls_load_without_old_hf_credentials(&ui);
}
