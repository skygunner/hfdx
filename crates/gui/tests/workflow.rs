#[path = "../src/bridge.rs"]
mod bridge;
#[path = "../src/callbacks.rs"]
mod callbacks;
#[path = "../src/files.rs"]
mod files;
#[path = "../src/input.rs"]
mod input;
#[path = "../src/queue_view.rs"]
mod queue_view;
mod support;
#[path = "../src/view.rs"]
mod view;
slint::include_modules!();

use hfdx_core::{
    storage::{SettingsPaths, TaskState},
    task::{CoreConfig, UiPhase},
};
use slint::{ComponentHandle, Model};
use std::{cell::Cell, io::Write, path::Path, rc::Rc, sync::mpsc, time::Duration};

fn capture(ui: &AppWindow, directory: &Path, name: &str) {
    ui.window().request_redraw();
    slint::platform::update_timers_and_animations();
    let pixels = ui
        .window()
        .take_snapshot()
        .expect("native rendered snapshot");
    let mut output =
        std::fs::File::create(directory.join(format!("{name}.ppm"))).expect("capture file");
    write!(output, "P6\n{} {}\n255\n", pixels.width(), pixels.height()).expect("image header");
    for pixel in pixels.as_slice() {
        output
            .write_all(&[pixel.r, pixel.g, pixel.b])
            .expect("image pixel");
    }
}

#[test]
fn gui_callbacks_download_real_core_and_join_shutdown() {
    let environment = support::Environment::start(None, false);
    let origin = environment.origin.clone();
    let root = environment.root.clone();
    let evidence = environment.evidence.clone();
    let ui = AppWindow::new().expect("native window");
    let (bridge, worker) = bridge::Bridge::start_with_platform(
        &ui,
        CoreConfig {
            paths: SettingsPaths {
                settings_file: root.join("settings.json"),
            },
            aria2_executable: "aria2c".into(),
        },
        Ok(hfdx_core::notification::NativePlatform::new(None)),
    )
    .expect("real core");
    let snapshot = bridge.mailbox.lock().expect("mailbox").snapshot.clone();
    let files = files::FilesModel::new(snapshot.selection.clone());
    ui.set_rows(slint::ModelRc::from(files.clone()));
    view::initialize(&ui, &snapshot);
    callbacks::connect(&ui, &bridge, &files);
    ui.set_endpoint(origin.into());
    ui.set_allow_http(true);
    ui.set_repository("fixture/repo".into());
    ui.set_destination(root.join("download").to_string_lossy().as_ref().into());
    let step = Rc::new(Cell::new(0));
    let b = bridge.clone();
    let weak = ui.as_weak();
    let stage = step.clone();
    let directory = evidence.clone();
    ui.on_refresh(move || {
        let ui = weak.upgrade().expect("window alive until joined");
        bridge::refresh_ui(&ui, &b, &files);
        let snapshot = b.mailbox.lock().expect("mailbox").snapshot.clone();
        assert!(snapshot.error.is_none(), "core error: {:?}", snapshot.error);
        match stage.get() {
            0 if snapshot.phase == UiPhase::Browsing => {
                assert_eq!(files.row_count(), 4);
                assert_eq!(snapshot.selection.summary.selected_count, 0);
                capture(&ui, &directory, "catalog");
                if std::env::var_os("HFDX_GUI_EVIDENCE").is_some() {
                    // A view-only combination row also exercises the search control's layout.
                    let profiles = ui.get_profiles();
                    ui.set_profiles(slint::ModelRc::new(slint::VecModel::from(vec![
                        ProfileRow {
                            label: "Q4_K_M (layout fixture)".into(),
                            ..Default::default()
                        },
                    ])));
                    ui.window()
                        .set_size(slint::LogicalSize::new(1920.0, 1080.0));
                    capture(&ui, &directory, "large-single-line");
                    ui.set_filter("small\nlarge".into());
                    capture(&ui, &directory, "large-multiple-conditions");
                    ui.set_filter("".into());
                    ui.set_profiles(profiles);
                    ui.window().set_size(slint::LogicalSize::new(1040.0, 760.0));
                }
                stage.set(1);
                ui.invoke_filter_edited("small\nspecial/space".into());
            }
            1 if snapshot.selection.matches.len() == 2 => {
                stage.set(2);
                ui.invoke_selection_action(0);
            }
            2 if snapshot.selection.summary.selected_count == 2 => {
                stage.set(3);
                ui.invoke_filter_edited("large".into());
            }
            3 if snapshot.selection.summary.hidden_selected_count == 2 => {
                capture(&ui, &directory, "filter-applied");
                assert_eq!(files.row_count(), 1);
                capture(&ui, &directory, "hidden-selection");
                stage.set(4);
                ui.invoke_selection_action(0);
            }
            4 if snapshot.selection.summary.selected_count == 3 => {
                stage.set(9);
                ui.set_language_index(2);
                ui.set_theme_index(2);
                ui.set_token("gui-memory-token".into());
                ui.set_filter("input being typed".into());
                ui.invoke_appearance_edited();
                assert_eq!(ui.get_token().as_str(), "gui-memory-token");
                assert_eq!(ui.get_filter().as_str(), "input being typed");
                assert_eq!(snapshot.selection.summary.selected_count, 3);
            }
            9 if snapshot.settings.language == Some(hfdx_core::storage::Language::ZhHant)
                && snapshot.settings.theme == hfdx_core::storage::Theme::Dark =>
            {
                stage.set(5);
                assert_eq!(snapshot.selection.summary.selected_count, 3);
                ui.set_token("".into());
                ui.invoke_inspect(0);
                assert!(ui.get_details().contains("large-partial.bin"));
                ui.invoke_start();
                assert!(ui.get_tasks_open(), "start navigates synchronously");
                assert!(
                    ui.get_starting(),
                    "initialization has immediate visible feedback"
                );
                capture(&ui, &directory, "initializing");
            }
            5 if snapshot.task.as_ref().is_some_and(|t| {
                t.progress.acquired_bytes.get() > 0
                    && t.progress.completed_files < t.progress.total_files
            }) =>
            {
                stage.set(6);
                // Freeze the transfer before expensive native screenshot I/O can finish it.
                ui.invoke_pause();
                assert!(!ui.get_can_pause(), "pause disables synchronously");
                ui.invoke_pause();
                view::apply(&ui, &snapshot);
                assert!(
                    !ui.get_can_pause(),
                    "an in-flight snapshot must not enable a second pause"
                );
                assert!(ui.get_pausing());
                assert!(ui.get_tasks_open());
                assert_eq!(ui.get_task_rows().row_count(), 3);
                let filter = ui.get_filter();
                ui.set_tasks_open(false);
                view::apply(&ui, &snapshot);
                assert!(!ui.get_tasks_open());
                assert_eq!(ui.get_filter(), filter);
                assert_eq!(snapshot.selection.summary.selected_count, 3);
                capture(&ui, &directory, "setup-during-download");
                ui.set_tasks_open(true);
                capture(&ui, &directory, "pausing");
                capture(&ui, &directory, "downloading");
            }
            6 if snapshot.phase == UiPhase::Paused => {
                assert!(!ui.get_pausing());
                assert!(ui.get_can_resume());
                stage.set(7);
                capture(&ui, &directory, "paused");
                ui.invoke_resume();
            }
            7 if snapshot
                .task
                .as_ref()
                .is_some_and(|t| t.record.state == TaskState::Completed) =>
            {
                let task = snapshot.task.as_ref().expect("completed task");
                assert_eq!(task.progress.completed_files, 3);
                assert_eq!(task.progress.acquired_bytes.get(), 33_817_600);
                ui.invoke_save();
                capture(&ui, &directory, "completed");
                ui.set_tasks_open(false);
                view::apply(&ui, &snapshot);
                assert!(
                    !ui.get_task_identity().is_empty(),
                    "setup navigation keeps the finished task"
                );
                assert_eq!(ui.get_task_rows().row_count(), 3);
                ui.set_tasks_open(true);

                ui.set_settings_open(true);
                capture(&ui, &directory, "settings");
                ui.set_settings_open(false);
                ui.set_history_open(true);
                capture(&ui, &directory, "recent");
                ui.set_history_open(false);
                ui.set_help_open(true);
                capture(&ui, &directory, "about");
                ui.set_help_open(false);
                ui.window().set_size(slint::LogicalSize::new(800.0, 600.0));
                capture(&ui, &directory, "800x600");
                stage.set(8);
                assert_eq!(b.close(&ui), slint::CloseRequestResponse::KeepWindowShown);
            }
            _ => {}
        }
    });
    let (done_tx, done_rx) = mpsc::channel();
    let timeout = std::thread::spawn(move || {
        if done_rx.recv_timeout(Duration::from_secs(90)).is_err() {
            slint::invoke_from_event_loop(|| {
                slint::quit_event_loop().expect("timeout exit");
            })
            .expect("event loop reachable");
        }
    });
    ui.show().expect("show actual window");
    ui.invoke_load();
    ui.run().expect("native event loop");
    done_tx.send(()).expect("cancel timeout");
    timeout.join().expect("timeout watcher joined");
    worker
        .join()
        .expect("background core joined")
        .expect("core shutdown succeeded");
    assert_eq!(
        step.get(),
        8,
        "workflow must reach completion before shutdown"
    );
    let mailbox = bridge.mailbox.lock().expect("mailbox");
    assert!(mailbox.finished);
    assert_eq!(
        mailbox.snapshot.settings.recent_directories.first(),
        Some(&root),
        "recent directory must be the destination parent"
    );
    assert!(root.join("download/.hfdx/task.json").is_file());
    drop(mailbox);
    environment.finish();
}

/// Public metadata only: no model weights are downloaded by this check.
#[test]
#[ignore = "requires public Hugging Face and a native display"]
fn public_gguf_combinations_preview_exact_union() {
    let root = tempfile::tempdir().unwrap();
    let evidence = std::env::var_os("HFDX_GUI_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.path().join("evidence"));
    std::fs::create_dir_all(&evidence).unwrap();
    let ui = AppWindow::new().unwrap();
    let (bridge, worker) = bridge::Bridge::start_with_platform(
        &ui,
        CoreConfig {
            paths: SettingsPaths {
                settings_file: root.path().join("settings.json"),
            },
            aria2_executable: "aria2c".into(),
        },
        Ok(hfdx_core::notification::NativePlatform::new(None)),
    )
    .unwrap();
    let snapshot = bridge.mailbox.lock().unwrap().snapshot.clone();
    let files = files::FilesModel::new(snapshot.selection.clone());
    ui.set_rows(slint::ModelRc::from(files.clone()));
    view::initialize(&ui, &snapshot);
    callbacks::connect(&ui, &bridge, &files);
    ui.set_repository("owner/old-model".into());
    ui.show().unwrap();
    ui.window().take_snapshot().unwrap();
    // Exercise Slint's real pointer routing, not a select-all callback.
    use slint::platform::{PointerEventButton, WindowEvent};
    for _ in 0..3 {
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(230.0, 112.0),
            button: PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(230.0, 112.0),
            button: PointerEventButton::Left,
        });
    }
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: "z".into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: "z".into() });
    assert_eq!(
        ui.get_repository().as_str(),
        "z",
        "triple-click selects the entire repository input"
    );
    ui.set_destination("/tmp/old-directory".into());
    let height = ui
        .window()
        .size()
        .to_logical(ui.window().scale_factor())
        .height;
    for _ in 0..3 {
        let position = slint::LogicalPosition::new(120.0, height - 120.0);
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    }
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: "z".into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: "z".into() });
    assert_eq!(
        ui.get_destination().as_str(),
        "z",
        "triple-click selects the destination input"
    );
    ui.set_destination(
        root.path()
            .join("download")
            .to_string_lossy()
            .as_ref()
            .into(),
    );
    ui.set_repository("unsloth/Qwen3.8-27B-GGUF".into());
    let weak = ui.as_weak();
    let b = bridge.clone();
    let stage = Rc::new(Cell::new(0));
    let step = stage.clone();
    let directory = evidence.clone();
    ui.on_refresh(move || {
        let ui = weak.upgrade().unwrap();
        bridge::refresh_ui(&ui, &b, &files);
        let snapshot = b.mailbox.lock().unwrap().snapshot.clone();
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        if snapshot.phase == UiPhase::Browsing {
            let catalog = snapshot.selection.catalog.as_ref().unwrap();
            let profiles = hfdx_core::profiles::analyze(catalog);
            let wanted: Vec<_> = profiles
                .iter()
                .enumerate()
                .filter(|(_, p)| p.label == "Model · UD-Q3_K_XL" || p.label == "Model · UD-Q4_K_M")
                .map(|(i, _)| i)
                .collect();
            assert_eq!(wanted.len(), 2);
            match step.get() {
                0 => {
                    capture(&ui, &directory, "public-combinations");
                    step.set(1);
                    for i in wanted {
                        ui.invoke_profile_toggle(i as i32, true);
                    }
                }
                1 if wanted.iter().all(|i| {
                    profiles[*i]
                        .files
                        .iter()
                        .all(|id| snapshot.selection.selected.contains(id))
                }) =>
                {
                    let expected: std::collections::BTreeSet<_> = wanted
                        .iter()
                        .flat_map(|i| profiles[*i].files.iter().copied())
                        .collect();
                    assert_eq!(expected, *snapshot.selection.selected);
                    let paths: Vec<_> = expected
                        .iter()
                        .map(|id| catalog.entry(*id).unwrap().path.as_str())
                        .collect();
                    assert!(paths.contains(&"mmproj-F16.gguf"));
                    assert!(paths.contains(&"config.json"));
                    assert!(
                        !paths
                            .iter()
                            .any(|p| p.contains("MTP/") || p.contains("imatrix"))
                    );
                    std::fs::write(
                        directory.join("public-preview.txt"),
                        format!(
                            "commit={}\n{}\nbytes={}\n",
                            catalog.source.resolved_commit.as_str(),
                            paths.join("\n"),
                            snapshot.selection.summary.selected_bytes.get()
                        ),
                    )
                    .unwrap();
                    capture(&ui, &directory, "public-two-combinations");
                    ui.set_profiles_expanded(false);
                    ui.set_selected_only(true);
                    ui.invoke_selected_only_edited(true);
                    step.set(2);
                }
                2 if snapshot.selection.selected_only => {
                    assert_eq!(files.row_count(), snapshot.selection.summary.selected_count);
                    capture(&ui, &directory, "public-final-preview");
                    step.set(3);
                    for i in wanted {
                        ui.invoke_profile_toggle(i as i32, false);
                    }
                }
                3 if snapshot.selection.selected.is_empty() => {
                    assert_eq!(snapshot.selection.summary.selected_bytes.get(), 0);
                    step.set(4);
                    b.handle.request_shutdown().unwrap();
                }
                _ => {}
            }
        }
        if b.mailbox.lock().unwrap().finished {
            slint::quit_event_loop().unwrap();
        }
    });
    let timeout = slint::Timer::default();
    timeout.start(
        slint::TimerMode::SingleShot,
        Duration::from_secs(90),
        || panic!("public metadata timeout"),
    );
    ui.invoke_load();
    ui.run().unwrap();
    worker.join().unwrap().unwrap();
    assert_eq!(stage.get(), 4);
}
