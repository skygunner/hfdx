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
use std::{cell::Cell, io::Write, rc::Rc, time::Duration};
fn capture(ui: &AppWindow, dir: &std::path::Path, name: &str) {
    ui.window().request_redraw();
    slint::platform::update_timers_and_animations();
    let pixels = ui.window().take_snapshot().unwrap();
    let mut file = std::fs::File::create(dir.join(format!("{name}.ppm"))).unwrap();
    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height()).unwrap();
    for pixel in pixels.as_slice() {
        file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap();
    }
}
#[test]
fn gui_adds_second_repository_and_appends_without_resetting_active_download() {
    let environment = support::Environment::start(None, true);
    let root = environment.root.clone();
    let evidence = environment.evidence.clone();
    let ui = AppWindow::new().unwrap();
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
    .unwrap();
    let snapshot = bridge.mailbox.lock().unwrap().snapshot.clone();
    let files = files::FilesModel::new(snapshot.selection.clone());
    ui.set_rows(slint::ModelRc::from(files.clone()));
    view::initialize(&ui, &snapshot);
    assert_eq!(
        ui.get_build_info().as_str(),
        hfdx_core::version::description()
    );
    callbacks::connect(&ui, &bridge, &files);
    ui.set_endpoint(environment.origin.clone().into());
    ui.set_allow_http(true);
    ui.set_repository("fixture/A".into());
    ui.set_destination(root.join("A").to_string_lossy().as_ref().into());
    std::fs::create_dir_all(root.join("A")).unwrap();
    std::fs::write(
        root.join("A/small-complete.bin"),
        (0..=255u8).cycle().take(262144).collect::<Vec<_>>(),
    )
    .unwrap();
    let stage = Rc::new(Cell::new(0));
    let done = stage.clone();
    let weak = ui.as_weak();
    let b = bridge.clone();
    let output = evidence.clone();
    let target = root.clone();
    ui.on_refresh(move || {
        let ui = weak.upgrade().unwrap();
        bridge::refresh_ui(&ui, &b, &files);
        let snapshot = b.mailbox.lock().unwrap().snapshot.clone();
        assert!(
            snapshot.error.is_none(),
            "queue GUI error: {:?}",
            snapshot.error
        );
        let queue = snapshot.queue.as_ref().unwrap();
        let repo = snapshot
            .selection
            .catalog
            .as_ref()
            .map(|c| c.source.request.repo_id.as_str());
        match stage.get() {
            0 if snapshot.selection.state == hfdx_core::CatalogState::Ready => {
                stage.set(1);
                ui.invoke_filter_edited("large-partial.bin".into());
            }
            1 if snapshot.selection.matches.len() == 1
                && snapshot.selection.selected.is_empty() =>
            {
                stage.set(2);
                ui.invoke_selection_action(0);
            }
            2 if snapshot.selection.summary.selected_count == 1 => {
                stage.set(3);
                ui.set_task_connections(1);
                ui.invoke_start();
                assert!(ui.get_starting());
            }
            3 if queue.tasks.len() == 1
                && queue.tasks[0].view.progress.acquired_bytes.get() > 0 =>
            {
                stage.set(4);
                capture(&ui, &output, "one-active-repository");
                assert!(ui.get_editable());
                assert!(ui.get_can_remove_task());
                ui.invoke_remove_task(false);
                assert_eq!(ui.get_remove_confirmation_id(), ui.get_opened_task_id());
                ui.set_remove_confirmation_id("".into());
                ui.set_tasks_open(false);
                ui.set_repository("fixture/B".into());
                // Reproduce switching repositories without editing the previous task's folder.
                ui.invoke_load();
            }
            4 if repo == Some("fixture/B")
                && snapshot.selection.state == hfdx_core::CatalogState::Ready =>
            {
                assert_eq!(
                    std::path::PathBuf::from(ui.get_destination().as_str()),
                    target.join("B")
                );
                stage.set(5);
                ui.invoke_filter_edited("small-complete.bin".into());
            }
            5 if repo == Some("fixture/B")
                && snapshot.selection.matches.len() == 1
                && snapshot.selection.selected.is_empty() =>
            {
                stage.set(6);
                ui.invoke_selection_action(0);
            }
            6 if repo == Some("fixture/B") && snapshot.selection.summary.selected_count == 1 => {
                stage.set(7);
                ui.invoke_start();
            }
            7 if queue.tasks.len() == 2
                && queue.tasks.iter().any(|t| {
                    t.view.record.source.request.repo_id.as_str() == "fixture/B"
                        && t.view.record.state == TaskState::Completed
                }) =>
            {
                stage.set(8);
                capture(&ui, &output, "two-repositories");
                let rows = ui.get_queue_rows().row_count();
                ui.invoke_queue_inspect(0);
                assert!(ui.get_queue_rows().row_count() < rows);
                ui.invoke_queue_inspect(0);
                ui.set_tasks_open(false);
                ui.set_repository("fixture/A".into());
                ui.set_destination(target.join("A").to_string_lossy().as_ref().into());
                ui.invoke_load();
            }
            8 if repo == Some("fixture/A")
                && snapshot.selection.state == hfdx_core::CatalogState::Ready =>
            {
                assert!(
                    ui.get_queue_append_policy()
                        .contains("Append uses existing settings")
                );
                assert!(ui.get_queue_append_policy().contains("1 connections"));
                stage.set(9);
                ui.invoke_filter_edited("small-complete.bin\nempty.bin".into());
            }
            9 if repo == Some("fixture/A")
                && snapshot.selection.matches.len() == 2
                && snapshot.selection.selected.is_empty() =>
            {
                stage.set(10);
                ui.invoke_selection_action(0);
            }
            10 if repo == Some("fixture/A") && snapshot.selection.summary.selected_count == 2 => {
                stage.set(11);
                ui.invoke_start();
            }
            11 if queue.tasks.iter().any(|t| {
                t.view.record.source.request.repo_id.as_str() == "fixture/A"
                    && t.view.record.files.len() == 3
            }) =>
            {
                stage.set(12);
                capture(&ui, &output, "appended-files");
                ui.invoke_pause_all();
                assert_eq!(ui.get_queue_control_wait(), 1);
            }
            12 if queue.tasks.iter().all(|t| t.phase == UiPhase::Paused)
                && ui.get_queue_control_wait() == 0 =>
            {
                stage.set(13);
                capture(&ui, &output, "queue-paused");
                ui.invoke_resume_all();
            }
            13 if queue
                .tasks
                .iter()
                .all(|t| t.view.record.state == TaskState::Completed) =>
            {
                stage.set(14);
                capture(&ui, &output, "queue-complete");
                ui.window().set_size(slint::LogicalSize::new(800., 600.));
                let previous_theme = ui.get_theme_index();
                for (theme, name) in [(1, "queue-light-800x600"), (2, "queue-dark-800x600")] {
                    ui.set_theme_index(theme);
                    ui.window().take_snapshot().unwrap();
                    // Let native widget palette transitions settle before taking evidence.
                    std::thread::sleep(Duration::from_millis(400));
                    capture(&ui, &output, name);
                }
                ui.set_theme_index(previous_theme);
                capture(&ui, &output, "queue-800x600");
                // Slint hit-testing + keyboard activation of the compact row (logical coordinates).
                use slint::platform::{PointerEventButton, WindowEvent};
                let expanded = ui.get_queue_rows().row_count();
                ui.window().dispatch_event(WindowEvent::PointerPressed {
                    position: slint::LogicalPosition::new(52., 391.),
                    button: PointerEventButton::Left,
                });
                ui.window().dispatch_event(WindowEvent::PointerReleased {
                    position: slint::LogicalPosition::new(52., 391.),
                    button: PointerEventButton::Left,
                });
                assert!(
                    ui.get_queue_rows().row_count() < expanded,
                    "clicking the repository row collapses its files"
                );
                ui.window().take_snapshot().unwrap();
                ui.window()
                    .dispatch_event(WindowEvent::KeyPressed { text: " ".into() });
                ui.window()
                    .dispatch_event(WindowEvent::KeyReleased { text: " ".into() });
                assert_eq!(
                    ui.get_queue_rows().row_count(),
                    expanded,
                    "Space expands the focused repository row"
                );
                ui.set_language_index(1);
                ui.invoke_appearance_edited();
                std::fs::create_dir_all(target.join("C")).unwrap();
                std::fs::write(
                    target.join("C/small-complete.bin"),
                    b"archive this conflict",
                )
                .unwrap();
                ui.set_tasks_open(false);
                ui.set_repository("fixture/C".into());
                ui.set_destination(target.join("C").to_string_lossy().as_ref().into());
                ui.invoke_load();
            }
            14 if repo == Some("fixture/C")
                && snapshot.selection.state == hfdx_core::CatalogState::Ready =>
            {
                stage.set(15);
                ui.invoke_filter_edited("small-complete.bin".into());
            }
            15 if repo == Some("fixture/C")
                && snapshot.selection.matches.len() == 1
                && snapshot.selection.selected.is_empty() =>
            {
                stage.set(16);
                ui.invoke_selection_action(0);
            }
            16 if repo == Some("fixture/C") && snapshot.selection.summary.selected_count == 1 => {
                stage.set(17);
                ui.invoke_start();
            }
            17 if queue.conflicts.is_some() => {
                stage.set(18);
                assert!(ui.get_conflict_open());
                assert!(!ui.get_tasks_open());
                assert_eq!(ui.get_conflict_policy(), -1);
                capture(&ui, &output, "conflict-choices");
                ui.invoke_cancel_submission();
            }
            18 if queue.conflicts.is_none() && !queue.preparing => {
                stage.set(19);
                assert_eq!(
                    std::fs::read(target.join("C/small-complete.bin")).unwrap(),
                    b"archive this conflict"
                );
                assert_eq!(queue.tasks.len(), 2);
                ui.invoke_start();
            }
            19 if queue.conflicts.is_some() => {
                stage.set(20);
                ui.set_conflict_policy(2);
                ui.invoke_confirm_conflicts(2);
            }
            20 if queue.tasks.len() == 3
                && queue
                    .tasks
                    .iter()
                    .all(|t| t.view.record.state == TaskState::Completed) =>
            {
                stage.set(21);
                assert_eq!(
                    std::fs::metadata(target.join("C/small-complete.bin"))
                        .unwrap()
                        .len(),
                    262144
                );
                let archive = std::fs::read_dir(target.join("C/archive"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                assert_eq!(
                    std::fs::read(archive.join("small-complete.bin")).unwrap(),
                    b"archive this conflict"
                );
                capture(&ui, &output, "conflict-archived");
                ui.invoke_remove_task(false);
                assert!(ui.get_remove_confirmation_id().is_empty());
            }
            21 if queue.tasks.len() == 2 => {
                assert!(target.join("C/small-complete.bin").is_file());
                stage.set(22);
                ui.set_hf_token("hf_fake_gui_check".into());
                ui.set_modelscope_token("ms_fake_gui_check".into());
                ui.set_endpoint("invalid endpoint".into());
                ui.invoke_save_tokens();
            }
            22 if snapshot.settings.hf_token.is_some()
                && snapshot.settings.modelscope_token.is_some() =>
            {
                stage.set(23);
                let mut diagnostic = (*snapshot).clone();
                diagnostic.error = Some(hfdx_core::task::UiError {
                    code: hfdx_core::task::ErrorCode::Engine,
                    message: String::new(),
                    hint: String::new(),
                    detail: Some(hfdx_core::task::ErrorDetail::Engine {
                        operation: "status",
                        reason: hfdx_core::task::EngineFailure::Timeout,
                    }),
                });
                view::apply(&ui, &diagnostic);
                assert!(ui.get_task_error_details().contains("aria2 请求超时"));
                ui.set_error_details_open(true);
                capture(&ui, &output, "aria2-error-details");
                ui.set_tasks_open(false);
                ui.set_help_open(true);
                capture(&ui, &output, "help-build-info");
                b.handle.request_shutdown().unwrap();
            }
            _ => {}
        }
    });
    let timeout = slint::Timer::default();
    let stop = bridge.handle.clone();
    timeout.start(
        slint::TimerMode::SingleShot,
        Duration::from_secs(75),
        move || {
            stop.request_shutdown().unwrap();
        },
    );
    ui.show().unwrap();
    ui.invoke_load();
    ui.run().unwrap();
    worker.join().unwrap().unwrap();
    assert_eq!(done.get(), 23, "native queue workflow must finish");
    environment.finish();
}
