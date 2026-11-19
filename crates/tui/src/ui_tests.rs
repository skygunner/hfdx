use super::*;
use crossterm::event::KeyEvent;
use hfdx_core::{Bytes, CatalogEntry, CatalogPage, CommitId, RepoPath, RepositorySpec, Selection};
use ratatui::backend::TestBackend;
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn paste_updates_focused_fields_and_keeps_filter_and_token_rules() {
    let session = hfdx_core::task::spawn(hfdx_core::task::CoreConfig {
        paths: hfdx_core::SettingsPaths {
            settings_file: std::env::temp_dir().join(format!("hfdx-paste-{}", std::process::id())),
        },
        aria2_executable: "aria2c".into(),
    })
    .await
    .unwrap();
    let snapshot = session.snapshots.borrow().clone();
    let mut app = App::new(String::new(), &snapshot);
    for (focus, text) in [
        "https://modelscope.ai/models/Qwen/Qwen2.5-0.5B",
        "master",
        "https://modelscope.ai",
        "ms_fake_token",
        "http://localhost:8080",
        "/my models/Qwen",
        "4",
        "2",
    ]
    .into_iter()
    .enumerate()
    {
        app.focus = focus;
        app.fields[focus].clear();
        paste(&mut app, &session, &snapshot, &format!("{text}\r\n"));
        assert_eq!(app.fields[focus], text);
    }
    assert_eq!(app.token_origin, "https://modelscope.ai");
    app.focus = 0;
    let repository = app.fields[0].clone();
    app.queue_open = true;
    paste(&mut app, &session, &snapshot, "ignored on the queue page");
    app.queue_open = false;
    paste(&mut app, &session, &snapshot, "multiple\nlines");
    assert_eq!(app.fields[0], repository);

    app.focus = 8;
    app.cursor = 7;
    app.offset = 5;
    paste(&mut app, &session, &snapshot, "*.gguf\r\nconfig.json");
    assert_eq!(app.fields[8], "*.gguf\nconfig.json");
    assert_eq!((app.cursor, app.offset), (0, 0));
    let (receipt, _) = app.pending.pop().unwrap();
    receipt.await.unwrap().unwrap();
    assert_eq!(session.snapshots.borrow().selection.filter, app.fields[8]);
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
}

#[tokio::test]
async fn tui_edits_and_saves_each_platform_token_without_echoing_it() {
    use hfdx_core::storage::{SettingsPaths, load_settings};
    let paths = SettingsPaths {
        settings_file: std::env::temp_dir().join(format!(
            "hfdx-token-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )),
    };
    let session = hfdx_core::task::spawn(hfdx_core::task::CoreConfig {
        paths: paths.clone(),
        aria2_executable: "aria2c".into(),
    })
    .await
    .unwrap();
    let mut app = App::new("owner/model".into(), &session.snapshots.borrow());
    app.settings_open = true;
    for (index, secret) in [(6, "hf_fake_saved"), (7, "ms_fake_saved")] {
        app.settings_selection = index;
        app.settings_pending = false;
        let snapshot = session.snapshots.borrow().clone();
        handle_key(
            &mut app,
            &session,
            &snapshot,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            24,
        );
        paste(&mut app, &session, &snapshot, &format!("{secret}\r\n"));
        let mut terminal = ratatui::Terminal::new(TestBackend::new(100, 32)).unwrap();
        terminal
            .draw(|frame| render(frame, &app, &snapshot))
            .unwrap();
        let visible = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(!visible.contains(secret));
        assert!(visible.contains("****"));
        assert!(visible.contains("Ctrl+Shift+V") && visible.contains("Ctrl+U"));
        handle_key(
            &mut app,
            &session,
            &snapshot,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            24,
        );
        let (receipt, _) = app.pending.pop().unwrap();
        receipt.await.unwrap().unwrap();
    }
    let loaded = load_settings(&paths).await.unwrap();
    assert_eq!(loaded.hf_token.as_ref().unwrap().expose(), "hf_fake_saved");
    assert_eq!(
        loaded.modelscope_token.as_ref().unwrap().expose(),
        "ms_fake_saved"
    );
    session.handle.request_shutdown().unwrap();
    session.join().await.unwrap();
    std::fs::remove_file(&paths.settings_file).unwrap();
}

#[test]
fn diagnostics_follow_current_language() {
    use hfdx_core::storage::Language;
    let diagnostic = Diagnostic {
        logging_failed: true,
        identity_error: None,
        notification: Some(NotificationDiagnostic::TimedOut),
    };
    assert!(
        diagnostic
            .render(Language::English)
            .contains("Notification timed out")
    );
    assert!(diagnostic.render(Language::ZhHans).contains("通知超时"));
    assert!(diagnostic.render(Language::ZhHant).contains("通知逾時"));
}

#[test]
fn settings_save_reloads_language_and_preserves_inputs() {
    use hfdx_core::storage::{Language, SettingsPaths, load_settings};
    let path = std::env::temp_dir().join(format!(
        "hfdx-settings-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let session = runtime
        .block_on(hfdx_core::task::spawn(hfdx_core::task::CoreConfig {
            paths: SettingsPaths {
                settings_file: path.clone(),
            },
            aria2_executable: "aria2c".into(),
        }))
        .unwrap();
    let snapshot = session.snapshots.borrow().clone();
    assert!(!snapshot.settings.debug_logging);
    let mut app = App::new("owner/model".into(), &snapshot);
    app.fields[3] = "test-token".into();
    app.fields[8] = "weights".into();
    app.cursor = 3;
    handle_key(
        &mut app,
        &session,
        &snapshot,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        24,
    );
    handle_key(
        &mut app,
        &session,
        &snapshot,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        24,
    );
    let (receipt, _) = app.pending.pop().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), receipt)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    });
    let saved = runtime
        .block_on(load_settings(&SettingsPaths {
            settings_file: path.clone(),
        }))
        .unwrap();
    assert_eq!(saved.language, Some(Language::English));
    let snapshot = session.snapshots.borrow().clone();
    app.settings_pending = false;
    handle_key(
        &mut app,
        &session,
        &snapshot,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        24,
    );
    let (receipt, _) = app.pending.pop().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), receipt)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    });
    let saved = runtime
        .block_on(load_settings(&SettingsPaths {
            settings_file: path.clone(),
        }))
        .unwrap();
    assert_eq!(saved.language, Some(Language::ZhHans));
    use hfdx_core::storage::Theme;
    for (selection, expected_language, expected_theme) in [
        (0, Some(Language::ZhHant), Theme::System),
        (1, Some(Language::ZhHant), Theme::Light),
        (1, Some(Language::ZhHant), Theme::Dark),
        (1, Some(Language::ZhHant), Theme::System),
    ] {
        let snapshot = session.snapshots.borrow().clone();
        if app.settings_selection != selection {
            handle_key(
                &mut app,
                &session,
                &snapshot,
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                24,
            );
        }
        app.settings_pending = false;
        let mut updates = session.snapshots.clone();
        updates.borrow_and_update();
        handle_key(
            &mut app,
            &session,
            &snapshot,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            24,
        );
        let (receipt, _) = app.pending.pop().unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), receipt)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            tokio::time::timeout(
                Duration::from_secs(3),
                updates.wait_for(|snapshot| {
                    snapshot.settings.language == expected_language
                        && snapshot.settings.theme == expected_theme
                }),
            )
            .await
            .unwrap()
            .unwrap();
        });
        let current = session.snapshots.borrow().clone();
        assert_eq!(current.settings.language, expected_language);
        assert_eq!(current.settings.theme, expected_theme);
        let persisted = runtime
            .block_on(load_settings(&SettingsPaths {
                settings_file: path.clone(),
            }))
            .unwrap();
        assert_eq!(persisted.language, expected_language);
        assert_eq!(persisted.theme, expected_theme);
    }
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let long_path = format!("{}END_OF_LOG_PATH", "a".repeat(100));
    terminal
        .draw(|frame| crate::settings::render(frame, frame.area(), &saved, 0, &long_path, "", None))
        .unwrap();
    let display: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(display.contains("END_OF_LOG_PATH"));
    assert_eq!(app.fields[0], "owner/model");
    assert_eq!(app.fields[3], "test-token");
    assert_eq!(app.fields[8], "weights");
    assert_eq!(app.cursor, 3);
    session.handle.request_shutdown().unwrap();
    runtime.block_on(session.join()).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn status_scrolls_and_help_is_localized() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let paths = hfdx_core::storage::SettingsPaths {
        settings_file: std::env::temp_dir().join(format!("hfdx-help-{}", std::process::id())),
    };
    let session = runtime
        .block_on(hfdx_core::task::spawn(hfdx_core::task::CoreConfig {
            paths,
            aria2_executable: "aria2c".into(),
        }))
        .unwrap();
    let snapshot = session.snapshots.borrow().clone();
    let mut app = App::new(String::new(), &snapshot);
    let mut settings = (*snapshot.settings).clone();
    settings.language = Some(hfdx_core::storage::Language::ZhHant);
    let mut snapshot = AppSnapshot {
        settings: Arc::new(settings),
        diagnostics: hfdx_core::task::Diagnostics::default(),
        queue: None,
        revision: snapshot.revision,
        phase: snapshot.phase,
        selection: snapshot.selection.clone(),
        task: snapshot.task.clone(),
        recent: snapshot.recent.clone(),
        error: snapshot.error.clone(),
    };
    use hfdx_core::storage::Language;
    for language in [Language::English, Language::ZhHans, Language::ZhHant] {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        Arc::make_mut(&mut snapshot.settings).language = Some(language);
        let expected = shortcuts(&app, &snapshot);
        assert_eq!(expected.lines().count(), usize::from(DRAFT_SHORTCUT_HEIGHT));
        terminal
            .draw(|frame| render(frame, &app, &snapshot))
            .unwrap();
        let area = terminal.backend().buffer();
        let footer = area
            .content()
            .chunks_exact(80)
            .skip(24 - usize::from(DRAFT_SHORTCUT_HEIGHT));
        for (row, line) in footer.zip(expected.lines()) {
            let visible: String = row.iter().map(|cell| cell.symbol()).collect();
            assert_eq!(
                visible.replace(' ', ""),
                line.replace(' ', ""),
                "{language:?}"
            );
        }
    }
    app.message = "x".repeat(500);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render(frame, &app, &snapshot))
        .unwrap();
    handle_key(
        &mut app,
        &session,
        &snapshot,
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        24,
    );
    assert_eq!(app.status_scroll, 1);
    session.handle.request_shutdown().unwrap();
    runtime.block_on(session.join()).unwrap();
}

#[test]
fn focused_input_and_list_selection_render_at_terminal_boundary() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let paths = hfdx_core::storage::SettingsPaths {
        settings_file: std::env::temp_dir()
            .join(format!("hfdx-tui-test-{}-settings", std::process::id())),
    };
    let session = runtime
        .block_on(hfdx_core::task::spawn(hfdx_core::task::CoreConfig {
            paths,
            aria2_executable: "aria2c".into(),
        }))
        .unwrap();
    let initial = session.snapshots.borrow().clone();
    let mut selection = Selection::new();
    let request = RepositoryRequest {
        endpoint: initial.settings.endpoint.clone(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("org/model").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    let catalog = selection
        .begin(RepositorySpec {
            request,
            resolved_commit: CommitId::parse(&"a".repeat(40)).unwrap(),
        })
        .unwrap();
    selection
        .append(CatalogPage {
            catalog: catalog.id,
            entries: vec![CatalogEntry {
                path: RepoPath::parse("weights.bin").unwrap(),
                size: Bytes::new(10),
                content_hash: None,
            }],
            complete: true,
        })
        .unwrap();
    let snapshot = AppSnapshot {
        revision: 0,
        phase: hfdx_core::task::UiPhase::Browsing,
        selection: selection.view(),
        task: None,
        settings: initial.settings.clone(),
        recent: Arc::from([]),
        error: None,
        diagnostics: hfdx_core::task::Diagnostics::default(),
        queue: None,
    };
    let mut app = App::new("org/model".into(), &snapshot);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    handle_key(&mut app, &session, &snapshot, key(KeyCode::Char(' ')), 24);
    terminal
        .draw(|frame| render(frame, &app, &snapshot))
        .unwrap();
    assert!(app.fields[0].ends_with(' '));
    assert_eq!(
        app.pending.len(),
        0,
        "space in an input must not select a file"
    );
    handle_key(&mut app, &session, &snapshot, key(KeyCode::Esc), 24);
    handle_key(&mut app, &session, &snapshot, key(KeyCode::Char(' ')), 24);
    let (mut receiver, _) = app.pending.pop().unwrap();
    let result = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), &mut receiver)
            .await
            .unwrap()
            .unwrap()
    });
    assert!(matches!(result, Err(CoreError::Selection(_))));
    assert_eq!(
        session.snapshots.borrow().selection.summary.selected_count,
        0,
        "unrelated real core catalog cannot accept stale id"
    );
    selection
        .change(SelectionChange::Set {
            id: snapshot.selection.visible[0],
            selected: true,
        })
        .unwrap();
    let selected = AppSnapshot {
        selection: selection.view(),
        ..snapshot
    };
    terminal
        .draw(|frame| render(frame, &app, &selected))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let display: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
    assert!(display.contains("[x] weights.bin"));

    let mut auth = App::new("org/model".into(), &selected);
    auth.fields[5] = std::env::temp_dir()
        .join("hfdx-test-destination")
        .display()
        .to_string();
    auth.fields[3].clear();
    auth.focus = 3;
    handle_key(&mut auth, &session, &selected, key(KeyCode::Char('a')), 24);
    auth.fields[2] = "https://mirror.example/".into();
    assert!(
        auth.load(&session).is_err(),
        "token from the old origin must not be sent"
    );
    handle_key(&mut auth, &session, &selected, key(KeyCode::Char('b')), 24);
    assert!(
        auth.load(&session).is_err(),
        "editing an old token must not rebind it"
    );
    handle_key(
        &mut auth,
        &session,
        &selected,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        24,
    );
    handle_key(&mut auth, &session, &selected, key(KeyCode::Char('c')), 24);
    assert!(
        auth.load(&session).is_ok(),
        "a fresh token must bind to the normalized new origin"
    );
    let mut history = App::new("org/model".into(), &selected);
    history.fields[3].clear();
    history.focus = 3;
    handle_key(
        &mut history,
        &session,
        &selected,
        key(KeyCode::Char('a')),
        24,
    );
    let mut old_source = RepositoryRequest {
        endpoint: initial.settings.endpoint.clone(),
        kind: RepoKind::Model,
        repo_id: RepoId::parse("org/original").unwrap(),
        requested_revision: Revision::parse("main").unwrap(),
    };
    history.opened(&old_source);
    assert_eq!(history.fields[3], "a", "same-origin history retains token");
    old_source.endpoint = Endpoint::parse("https://mirror.example/", false).unwrap();
    old_source.repo_id = RepoId::parse("other/model").unwrap();
    old_source.requested_revision = Revision::parse("branch").unwrap();
    history.opened(&old_source);
    assert_eq!(history.fields[3], "", "cross-origin history clears token");
    assert_eq!(history.fields[0], "other/model");
    assert_eq!(history.fields[1], "branch");
    assert_eq!(history.fields[2], "https://mirror.example");
    assert_ne!(
        history.token_origin, "https://mirror.example",
        "opening history must not rebind a token"
    );
    history.fields[3] = "a".into();
    let prior_pending = history.pending.len();
    handle_key(&mut history, &session, &selected, key(KeyCode::F(6)), 24);
    assert_eq!(
        history.pending.len(),
        prior_pending,
        "resume rejects a retained cross-origin token"
    );
    assert!(!history.message.is_empty());
    history.fields[8] = "weights".into();
    history.only_selected = true;
    history.load_accepted();
    assert!(history.fields[8].is_empty());
    assert!(!history.only_selected);
    selection.set_filter("GGUF").unwrap();
    let no_match = AppSnapshot {
        selection: selection.view(),
        ..selected
    };
    terminal
        .draw(|frame| render(frame, &app, &no_match))
        .unwrap();
    let display: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(display.contains("No matching files"));
    assert_eq!(no_match.selection.summary.hidden_selected_count, 1);
    selection.set_filter("").unwrap();
    selection.change(SelectionChange::Clear).unwrap();
    selection.set_selected_only(true);
    let hidden = AppSnapshot {
        selection: selection.view(),
        ..no_match
    };
    terminal.draw(|frame| render(frame, &app, &hidden)).unwrap();
    let display: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(display.contains("Selected-only hides all matches"));
    for command in [
        CoreCommand::SetFilter("GGUF".into()),
        CoreCommand::SetSelectedOnly(true),
    ] {
        let receiver = session.handle.submit(command).unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), receiver)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        });
    }
    let selected_before = session.snapshots.borrow().selection.selected.clone();
    app.fields[8] = "GGUF".into();
    app.only_selected = true;
    handle_key(&mut app, &session, &hidden, key(KeyCode::F(12)), 24);
    assert!(app.fields[8].is_empty());
    for (receiver, _) in app.pending.drain(..) {
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), receiver)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        });
    }
    assert!(session.snapshots.borrow().selection.filter.is_empty());
    assert!(!session.snapshots.borrow().selection.selected_only);
    assert_eq!(
        session.snapshots.borrow().selection.selected,
        selected_before
    );
    session.handle.request_shutdown().unwrap();
    runtime.block_on(session.join()).unwrap();
}

#[test]
fn queue_page_and_existing_file_check_render_in_the_terminal() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = std::env::temp_dir().join(format!("hfdx-queue-render-{}", std::process::id()));
    let session = runtime
        .block_on(hfdx_core::task::spawn(hfdx_core::task::CoreConfig {
            paths: hfdx_core::SettingsPaths {
                settings_file: root.join("settings.json"),
            },
            aria2_executable: "aria2c".into(),
        }))
        .unwrap();
    let mut snapshot = (*session.snapshots.borrow().clone()).clone();
    snapshot.queue = Some(Arc::new(hfdx_core::task::QueueView {
        used_slots: 1,
        slot_limit: 2,
        ..Default::default()
    }));
    let mut app = App::new("org/model".into(), &snapshot);
    app.queue_open = true;
    let mut terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app, &snapshot)).unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(screen.contains("slots 1/2"));
    snapshot.queue = Some(Arc::new(hfdx_core::task::QueueView {
        preparing: true,
        cancellable: true,
        checking_path: Some("model.gguf".into()),
        checking_bytes: 1024,
        checking_total: 2048,
        ..Default::default()
    }));
    app.queue_open = false;
    terminal.draw(|f| render(f, &app, &snapshot)).unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(screen.contains("model.gguf"));
    assert!(screen.contains("Ctrl+J"));
    session.handle.request_shutdown().unwrap();
    runtime.block_on(session.join()).unwrap();
}

#[test]
fn task_pages_preserve_draft_and_show_localized_colored_file_progress() {
    use hfdx_core::{
        storage::{FileDisposition, Language, Theme},
        task::{ActiveFile, QueueView, QueuedTask, UiPhase, VerificationProgress},
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = std::env::temp_dir().join(format!(
        "hfdx-tui-sync-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("model/.hfdx")).unwrap();
    let task_file = root.join("model/.hfdx/task.json");
    std::fs::write(&task_file, r#"{
        "schema_version":2,"task_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "source":{"request":{"endpoint":{"url":"https://huggingface.co/","allow_http":false},"kind":"model","repo_id":"fixture/colors","requested_revision":"main"},"resolved_commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
        "files":[{"entry":{"path":"weights.gguf","size":21000000000,"content_hash":null},"completed":false,"failure":null,"verification":"pending"}],
        "options":{"connections_per_file":1,"concurrent_files":2},"proxy_address":null,"verify_after_download":true,"state":"paused","created_unix_ms":1,"updated_unix_ms":1
    }"#).unwrap();
    let session = runtime
        .block_on(hfdx_core::task::spawn_queue_with_services(
            hfdx_core::task::CoreConfig {
                paths: hfdx_core::SettingsPaths {
                    settings_file: root.join("settings.json"),
                },
                aria2_executable: "aria2c".into(),
            },
            Arc::new(|_| {}),
            None,
        ))
        .unwrap();
    runtime.block_on(async {
        session
            .handle
            .submit(CoreCommand::OpenTask(task_file))
            .unwrap()
            .await
            .unwrap()
            .unwrap();
    });
    let mut snapshot = (*session.snapshots.borrow().clone()).clone();
    let mut task = snapshot.task.clone().unwrap();
    let record = Arc::make_mut(&mut task.record);
    let base = record.files[0].clone();
    record.files = (0..5)
        .map(|i| {
            let mut f = base.clone();
            f.entry.path = RepoPath::parse(&format!("file-{i}.gguf")).unwrap();
            f
        })
        .collect();
    record.files[2].completed = true;
    record.files[2].verification = VerificationStatus::Passed;
    record.files[3].verification = VerificationStatus::Failed;
    record.files[3].failure = Some(hfdx_core::storage::FileFailure {
        kind: hfdx_core::storage::FailureKind::Network,
        message: "must-not-display-secret".into(),
        aria2_code: Some(22),
        http_status: Some(429),
    });
    record.files[4].disposition = FileDisposition::KeptDifferent;
    record.files[4].verification = VerificationStatus::Unavailable;
    record.state = TaskState::Running;
    task.progress.failed = Arc::from([(3, task.record.files[3].failure.clone().unwrap())]);
    task.progress.active = Arc::from([ActiveFile {
        index: 0,
        completed_bytes: Bytes::new(3_200_000_000),
        bytes_per_second: 125_500_000,
    }]);
    task.verification = Some(VerificationProgress {
        file_index: 1,
        file_bytes_done: 1000,
        files_done: 0,
        files_total: 5,
    });
    snapshot.queue = Some(Arc::new(QueueView {
        tasks: Arc::from([QueuedTask {
            view: task.clone(),
            phase: UiPhase::Downloading,
            error: None,
        }]),
        ..Default::default()
    }));
    let mut app = App::new("draft/repository".into(), &snapshot);
    app.fields[8] = "keep my filter".into();
    let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
    handle_key(&mut app, &session, &snapshot, ctrl('d'), 24);
    assert!(app.queue_open);
    handle_key(&mut app, &session, &snapshot, ctrl('s'), 24);
    assert!(app.settings_open && !app.queue_open);
    handle_key(
        &mut app,
        &session,
        &snapshot,
        KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE),
        24,
    );
    assert!(app.history_open && !app.settings_open);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render(f, &app, &snapshot)).unwrap();
    let screen = |t: &Terminal<TestBackend>| {
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>()
    };
    assert!(screen(&terminal).contains("History"));
    assert!(screen(&terminal).contains("Enter Open saved task"));
    handle_key(&mut app, &session, &snapshot, ctrl('n'), 24);
    assert!(!app.history_open && !app.queue_open);
    assert_eq!(app.fields[0], "draft/repository");
    assert_eq!(app.fields[8], "keep my filter");
    assert_eq!(snapshot.queue.as_ref().unwrap().tasks.len(), 1);
    app.focus = 8;
    handle_key(&mut app, &session, &snapshot, ctrl('j'), 24);
    assert!(
        !app.queue_open && app.fields[8].ends_with('\n'),
        "Ctrl+J keeps multiline filter editing"
    );
    app.queue_open = true;
    for language in [Language::English, Language::ZhHans, Language::ZhHant] {
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            let settings = Arc::make_mut(&mut snapshot.settings);
            settings.language = Some(language);
            settings.theme = theme;
            for width in [80, 120] {
                let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
                terminal.draw(|f| render(f, &app, &snapshot)).unwrap();
                let visible = screen(&terminal);
                assert!(
                    visible
                        .split_whitespace()
                        .collect::<String>()
                        .contains("HTTP429")
                );
                assert!(!visible.contains("must-not-display-secret"));
                assert!(
                    visible.contains("125.5 MB/s"),
                    "speed clipped at {width}: {visible}"
                );
                assert!(visible.contains("3.2 GB / 21 GB"));
                for (en, cn, tw, expected) in [
                    (
                        "Downloading",
                        "下载中",
                        "下載中",
                        if theme == Theme::Light {
                            ratatui::style::Color::Rgb(23, 93, 182)
                        } else {
                            ratatui::style::Color::Blue
                        },
                    ),
                    (
                        "Verifying",
                        "校验中",
                        "驗證中",
                        if theme == Theme::Light {
                            ratatui::style::Color::Rgb(117, 64, 181)
                        } else {
                            ratatui::style::Color::Magenta
                        },
                    ),
                    (
                        "Passed",
                        "通过",
                        "通過",
                        if theme == Theme::Light {
                            ratatui::style::Color::Rgb(25, 115, 62)
                        } else {
                            ratatui::style::Color::Green
                        },
                    ),
                    (
                        "Failed",
                        "失败",
                        "失敗",
                        if theme == Theme::Light {
                            ratatui::style::Color::Rgb(181, 46, 55)
                        } else {
                            ratatui::style::Color::Red
                        },
                    ),
                    (
                        "Unverified",
                        "未校验",
                        "未驗證",
                        if theme == Theme::Light {
                            ratatui::style::Color::Rgb(137, 89, 0)
                        } else {
                            ratatui::style::Color::Yellow
                        },
                    ),
                ] {
                    let word = queue::text(language, en, cn, tw);
                    assert!(
                        visible.replace(' ', "").contains(&word.replace(' ', "")),
                        "missing {word} at {width}: {visible}"
                    );
                    assert!(
                        terminal
                            .backend()
                            .buffer()
                            .content()
                            .iter()
                            .any(|c| c.symbol()
                                == &word[..word.chars().next().unwrap().len_utf8()]
                                && c.fg == expected)
                    );
                }
            }
        }
    }
    // Starting switches pages before waiting for the command receipt.
    let mut selection = Selection::new();
    let cat = selection.begin(task.record.source.clone()).unwrap();
    selection
        .append(CatalogPage {
            catalog: cat.id,
            entries: vec![base.entry.clone()],
            complete: true,
        })
        .unwrap();
    snapshot.selection = selection.view();
    app.queue_open = false;
    app.fields[5] = root.join("new").display().to_string();
    app.start(&session, &snapshot).unwrap();
    assert!(app.queue_open);
    assert!(app.message.contains("初始化") || app.message.contains("Initializing"));
    session.handle.request_shutdown().unwrap();
    runtime.block_on(session.join()).unwrap();
    for p in [
        root.join("model/.hfdx/task.json"),
        root.join("model/.hfdx/task.lock"),
    ] {
        if p.exists() {
            std::fs::remove_file(p).unwrap();
        }
    }
    // The test uses a uniquely owned root; no user data is referenced.
    assert!(
        root.starts_with(std::env::temp_dir())
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("hfdx-tui-sync-")
    );
    let mut entries = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    while let Some(p) = entries.pop() {
        if p.is_dir() {
            let contents = std::fs::read_dir(&p)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect::<Vec<_>>();
            if !contents.is_empty() {
                entries.push(p);
                entries.extend(contents);
            } else {
                std::fs::remove_dir(p).unwrap();
            }
        } else {
            std::fs::remove_file(p).unwrap();
        }
    }
    std::fs::remove_dir(root).unwrap();
}
