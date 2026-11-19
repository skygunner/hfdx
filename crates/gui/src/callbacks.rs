use crate::{AppWindow, bridge::Bridge, files::FilesModel, input};
use hfdx_core::{selection::SelectionChange, storage::default_destination, task::CoreCommand};
use slint::ComponentHandle;
use slint::Model;
use std::{path::PathBuf, rc::Rc};

fn spawn_launcher(command: &mut std::process::Command) -> std::io::Result<()> {
    let mut child = command.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

pub fn connect(ui: &AppWindow, bridge: &Bridge, files: &Rc<FilesModel>) {
    let weak = ui.as_weak();
    ui.on_get_token(move |modelscope| {
        let url = if modelscope {
            "https://modelscope.ai/my/myaccesstoken"
        } else {
            "https://huggingface.co/settings/tokens"
        };
        let program = if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        if spawn_launcher(std::process::Command::new(program).arg(url)).is_err()
            && let Some(ui) = weak.upgrade()
        {
            ui.set_error(input::message(&ui, hfdx_core::locale::Message::NetworkError).into());
        }
    });
    let b = bridge.clone();
    let f = files.clone();
    ui.on_refresh(move || {
        if let Some(ui) = b_ui(&b) {
            crate::bridge::refresh_ui(&ui, &b, &f);
        }
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.on_load(move || {
        if let Some(ui) = weak.upgrade() {
            let command = input::load(&ui).and_then(|command| {
                if let CoreCommand::Load(request) = &command {
                    let snapshot = b
                        .mailbox
                        .lock()
                        .map_err(|_| {
                            input::message(&ui, hfdx_core::locale::Message::InternalError)
                                .to_owned()
                        })?
                        .snapshot
                        .clone();
                    let path = hfdx_core::task::draft_destination(
                        &snapshot,
                        &request.repository,
                        std::path::Path::new(ui.get_destination().as_str()),
                    )
                    .map_err(|_| {
                        input::message(&ui, hfdx_core::locale::Message::EnterDestination).to_owned()
                    })?;
                    ui.set_destination(path.to_string_lossy().as_ref().into());
                }
                Ok(command)
            });
            submit_result(&b, command);
        }
    });
    let weak = ui.as_weak();
    ui.on_task_inspect(move |index| {
        if let Some(ui) = weak.upgrade()
            && let Ok(index) = usize::try_from(index)
            && let Some(model) = ui
                .get_task_rows()
                .as_any()
                .downcast_ref::<crate::files::TaskFilesModel>()
        {
            ui.set_task_file_index(index as i32);
            ui.set_task_file_details(model.details(index).into());
        }
    });
    let weak = ui.as_weak();
    let b = bridge.clone();
    ui.on_queue_inspect(move |index| {
        if let Some(ui) = weak.upgrade()
            && let Ok(index) = usize::try_from(index)
        {
            let rows = ui.get_queue_rows();
            if let Some(model) = rows
                .as_any()
                .downcast_ref::<crate::queue_view::QueueModel>()
                && let Some((id, details)) = model.inspect(index)
            {
                ui.set_queue_details(details.into());
                b.submit(CoreCommand::SelectTask(id));
            }
        }
    });
    let b = bridge.clone();
    ui.on_pause_all(move || queue_control(&b, CoreCommand::PauseAll, 1));
    let b = bridge.clone();
    ui.on_resume_all(move || queue_control(&b, CoreCommand::ResumeAll, 2));
    let weak = ui.as_weak();
    let b = bridge.clone();
    ui.on_remove_task(move |confirmed| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let snapshot = b.mailbox.lock().ok().map(|m| m.snapshot.clone());
        let Some(task) = snapshot.as_ref().and_then(|s| s.task.as_ref()) else {
            return;
        };
        let id = &task.record.task_id;
        if ui.get_opened_task_id().as_str() != id.as_str() {
            return;
        }
        if !confirmed && task.record.state != hfdx_core::storage::TaskState::Completed {
            ui.set_remove_confirmation_id(id.as_str().into());
            return;
        }
        if confirmed && ui.get_remove_confirmation_id().as_str() != id.as_str() {
            return;
        }
        ui.set_remove_confirmation_id("".into());
        ui.set_queue_details("".into());
        b.submit(CoreCommand::RemoveTask {
            id: id.clone(),
            confirmed,
        });
    });
    let weak = ui.as_weak();
    let b = bridge.clone();
    ui.on_append_current(move || {
        if let Some(ui) = weak.upgrade() {
            let snapshot = b.mailbox.lock().ok().map(|m| m.snapshot.clone());
            if let Some(task) = snapshot.as_ref().and_then(|s| s.task.as_ref()) {
                ui.set_tasks_open(false);
                ui.set_destination(task.destination.to_string_lossy().as_ref().into());
                ui.set_repository(task.record.source.request.repo_id.as_str().into());
                ui.set_endpoint(task.record.source.request.endpoint.url().as_str().into());
                ui.set_revision(task.record.source.resolved_commit.as_str().into());
                b.submit(CoreCommand::LoadActive(task.record.task_id.clone()));
            }
        }
    });
    let b = bridge.clone();
    ui.on_confirm_conflicts(move |policy| {
        let plan = b.mailbox.lock().ok().and_then(|m| {
            m.snapshot
                .queue
                .as_ref()
                .and_then(|q| q.conflicts.as_ref().map(|p| p.id))
        });
        let policy = match policy {
            0 => hfdx_core::task::ConflictPolicy::Overwrite,
            1 => hfdx_core::task::ConflictPolicy::NewerOnly,
            2 => hfdx_core::task::ConflictPolicy::Archive,
            _ => return,
        };
        if let Some(plan) = plan
            && let Some(ui) = b.ui().upgrade()
        {
            begin_submission(&b, &ui, CoreCommand::ConfirmConflicts { plan, policy });
        }
    });
    let b = bridge.clone();
    ui.on_cancel_submission(move || b.submit(CoreCommand::CancelSubmission));
    let b = bridge.clone();
    ui.on_cancel_load(move || b.submit(CoreCommand::CancelLoad));
    let b = bridge.clone();
    ui.on_profile_toggle(move |index, selected| {
        let catalog = b
            .mailbox
            .lock()
            .ok()
            .and_then(|state| state.snapshot.selection.catalog.as_ref().map(|c| c.id));
        if let (Some(catalog), Ok(index)) = (catalog, usize::try_from(index)) {
            b.submit(CoreCommand::ChangeProfile {
                catalog,
                index,
                selected,
            });
        }
    });
    let b = bridge.clone();
    ui.on_filter_edited(move |text| b.submit(CoreCommand::SetFilter(text.into())));
    let b = bridge.clone();
    ui.on_selected_only_edited(move |enabled| b.submit(CoreCommand::SetSelectedOnly(enabled)));
    let b = bridge.clone();
    ui.on_selection_action(move |action| {
        let change = match action {
            0 => SelectionChange::SelectMatches,
            1 => SelectionChange::DeselectMatches,
            2 => SelectionChange::Clear,
            _ => {
                b.show_message(hfdx_core::locale::Message::InternalError);
                return;
            }
        };
        b.submit(CoreCommand::ChangeSelection(change));
    });
    let b = bridge.clone();
    let f = files.clone();
    ui.on_toggle(move |row, selected| {
        if let Some(id) = f.id(row) {
            b.submit(CoreCommand::ChangeSelection(SelectionChange::Set {
                id,
                selected,
            }));
        }
    });
    let f = files.clone();
    let weak = ui.as_weak();
    ui.on_inspect(move |row| {
        if let (Some(ui), Some(details)) = (weak.upgrade(), f.details(row)) {
            ui.set_details(details.into());
        }
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.on_start(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_starting() {
            return;
        }
        let result = b
            .mailbox
            .lock()
            .map_err(|_| input::message(&ui, hfdx_core::locale::Message::InternalError).to_owned())
            .and_then(|state| input::start(&ui, &state.snapshot));
        let command = match result {
            Ok(command) => command,
            Err(error) => {
                b.show_error(&error);
                return;
            }
        };
        begin_submission(&b, &ui, command);
    });

    let weak = ui.as_weak();
    let b = bridge.clone();
    ui.on_browse(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let start = PathBuf::from(ui.get_destination().as_str());
        let title = input::message(&ui, hfdx_core::locale::Message::Destination);
        let weak = weak.clone();
        let b = b.clone();
        b.runtime.clone().spawn(async move {
            let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
            if start.is_dir() {
                dialog = dialog.set_directory(&start);
            }
            let Some(path) = dialog.pick_folder().await else {
                return;
            };
            let path = path.path().to_path_buf();
            if weak
                .upgrade_in_event_loop(move |ui| {
                    ui.set_destination(path.to_string_lossy().as_ref().into());
                })
                .is_err()
            {
                b.show_message(hfdx_core::locale::Message::Closed);
            }
        });
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.on_pause(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if !ui.get_can_pause() || ui.get_pausing() {
            return;
        }
        ui.set_pausing(true);
        ui.set_can_pause(false);
        ui.window().request_redraw();
        match b.handle.submit(CoreCommand::Pause) {
            Ok(receipt) => {
                let weak = ui.as_weak();
                b.runtime.spawn(async move {
                    // Success is shown only when its paused snapshot reaches the UI.
                    if !matches!(receipt.await, Ok(Ok(_))) {
                        let _ = weak.upgrade_in_event_loop(|ui| {
                            ui.set_pausing(false);
                            ui.invoke_refresh();
                        });
                    }
                });
            }
            Err(_) => {
                ui.set_pausing(false);
                ui.invoke_refresh();
                b.show_message(hfdx_core::locale::Message::CommandUnavailable);
            }
        }
    });
    let b = bridge.clone();
    ui.on_resume(move || b.submit(CoreCommand::Resume));
    let b = bridge.clone();
    ui.on_retry(move || b.submit(CoreCommand::RetryFailed));
    let b = bridge.clone();
    ui.on_verify_existing(move || b.submit(CoreCommand::VerifyExisting));
    let weak = ui.as_weak();
    ui.on_open_folder(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let path = std::path::PathBuf::from(ui.get_task_destination().as_str());
        if !path.is_dir() {
            ui.set_error(input::message(&ui, hfdx_core::locale::Message::Missing).into());
            return;
        }
        let program = if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        if spawn_launcher(std::process::Command::new(program).arg(&path)).is_err() {
            ui.set_error(input::message(&ui, hfdx_core::locale::Message::DiskError).into());
        }
    });
    let weak = ui.as_weak();
    ui.on_open_logs(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let program = if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        let path = PathBuf::from(ui.get_log_directory().as_str());
        if spawn_launcher(std::process::Command::new(program).arg(path)).is_err() {
            ui.set_error(input::message(&ui, hfdx_core::locale::Message::DiskError).into());
        }
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.on_appearance_edited(move || {
        if let Some(ui) = weak.upgrade() {
            crate::view::select_language(&ui);
            match b.mailbox.lock() {
                Ok(box_) => {
                    let snapshot = box_.snapshot.clone();
                    let mut settings = snapshot.settings.as_ref().clone();
                    settings.language = Some(input::language(&ui));
                    settings.theme = input::theme(&ui);
                    drop(box_);
                    crate::view::apply(&ui, &snapshot);
                    b.apply_diagnostics(&ui);
                    b.submit(CoreCommand::SaveSettings(settings));
                }
                Err(_) => b.show_error(input::message(
                    &ui,
                    hfdx_core::locale::Message::InternalError,
                )),
            }
        }
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.on_apply_access(move || {
        if let Some(ui) = weak.upgrade() {
            submit_result(&b, input::set_access(&ui));
        }
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.on_save(move || {
        if let Some(ui) = weak.upgrade() {
            match b.mailbox.lock() {
                Ok(box_) => {
                    let result = input::settings(&ui, &box_.snapshot.settings);
                    drop(box_);
                    submit_result(&b, result);
                }
                Err(_) => b.show_message(hfdx_core::locale::Message::InternalError),
            }
        }
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.on_save_tokens(move || {
        use hfdx_core::locale::Message;
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let command = input::platform_tokens(&ui)
            .map(|(hf, modelscope)| CoreCommand::SavePlatformTokens { hf, modelscope });
        let result = command.and_then(|command| {
            b.handle
                .submit(command)
                .map_err(|_| input::message(&ui, Message::CommandUnavailable).to_owned())
        });
        match result {
            Ok(receipt) => {
                ui.set_token_save_status("".into());
                ui.set_saving_tokens(true);
                let weak = ui.as_weak();
                b.runtime.spawn(async move {
                    let result = receipt.await;
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_saving_tokens(false);
                        let language = input::language(&ui);
                        let status = match result {
                            Ok(Ok(_)) => input::message(&ui, Message::TokensSaved).to_owned(),
                            Ok(Err(error)) => hfdx_core::locale::error(language, &error.ui_error()),
                            Err(_) => input::message(&ui, Message::Closed).to_owned(),
                        };
                        ui.set_token_save_status(status.into());
                    });
                });
            }
            Err(error) => ui.set_token_save_status(error.into()),
        }
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.on_open_task(move |path| {
        if path.trim().is_empty() {
            b.show_message(hfdx_core::locale::Message::InputError);
            return;
        }
        b.submit(CoreCommand::OpenTask(PathBuf::from(path.as_str())));
        if let Some(ui) = weak.upgrade() {
            ui.set_history_open(false);
        }
    });
    let b = bridge.clone();
    ui.on_forget_task(move |path| {
        b.submit(CoreCommand::ForgetRecent(PathBuf::from(path.as_str())))
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.on_choose_directory(move |index| {
        if let Some(ui) = weak.upgrade() {
            match b.mailbox.lock() {
                Ok(box_) => {
                    if let Some(path) = usize::try_from(index)
                        .ok()
                        .and_then(|i| box_.snapshot.settings.recent_directories.get(i))
                    {
                        match input::recent_destination(&ui, path) {
                            Ok(destination) => {
                                ui.set_destination(destination.to_string_lossy().as_ref().into())
                            }
                            Err(error) => ui.set_error(error.into()),
                        }
                        ui.set_history_open(false);
                    }
                }
                Err(_) => b.show_message(hfdx_core::locale::Message::InternalError),
            }
        }
    });
    let b = bridge.clone();
    let weak = ui.as_weak();
    ui.window()
        .on_close_requested(move || match weak.upgrade() {
            Some(ui) => b.close(&ui),
            None => slint::CloseRequestResponse::HideWindow,
        });
}

#[cfg(all(test, target_os = "linux"))]
mod launcher_tests {
    #[test]
    fn exited_desktop_launcher_is_reaped() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("launcher.pid");
        super::spawn_launcher(
            std::process::Command::new("sh")
                .args(["-c", "printf '%s' \"$$\" > \"$1\"", "hfdx-launch-test"])
                .arg(&pid_file),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = pid.parse::<u32>()
                && !std::path::Path::new(&format!("/proc/{pid}")).exists()
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "exited launcher must not remain a zombie child"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
fn b_ui(bridge: &Bridge) -> Option<AppWindow> {
    bridge.ui().upgrade()
}
fn submit_result(bridge: &Bridge, command: Result<CoreCommand, String>) {
    match command {
        Ok(command) => bridge.submit(command),
        Err(error) => bridge.show_error(&error),
    }
}
pub fn destination(ui: &AppWindow, snapshot: &hfdx_core::task::AppSnapshot) {
    if !ui.get_destination().is_empty() {
        return;
    }
    if let Some(catalog) = &snapshot.selection.catalog {
        match default_destination(&snapshot.settings, &catalog.source.request.repo_id) {
            Ok(path) => ui.set_destination(path.to_string_lossy().as_ref().into()),
            Err(_) => ui
                .set_error(input::message(ui, hfdx_core::locale::Message::EnterDestination).into()),
        }
    }
}

fn queue_control(bridge: &Bridge, command: CoreCommand, waiting: i32) {
    let Some(ui) = bridge.ui().upgrade() else {
        return;
    };
    if ui.get_queue_control_wait() != 0 {
        return;
    }
    ui.set_queue_control_wait(waiting);
    match bridge.handle.submit(command) {
        Ok(receipt) => {
            let weak = ui.as_weak();
            bridge.runtime.spawn(async move {
                let _ = receipt.await;
                let _ = weak.upgrade_in_event_loop(|ui| {
                    ui.set_queue_control_wait(0);
                    ui.invoke_refresh();
                });
            });
        }
        Err(_) => {
            ui.set_queue_control_wait(0);
            bridge.show_message(hfdx_core::locale::Message::CommandUnavailable);
        }
    }
}

fn begin_submission(b: &Bridge, ui: &AppWindow, command: CoreCommand) {
    if ui.get_starting() {
        return;
    }
    match b.handle.submit(command) {
        Ok(receipt) => {
            ui.set_starting(true);
            ui.set_settings_open(false);
            ui.set_history_open(false);
            ui.set_help_open(false);
            ui.set_tasks_open(true);
            ui.window().request_redraw();
            let weak = ui.as_weak();
            b.runtime.spawn(async move {
                let success = matches!(receipt.await, Ok(Ok(_)));
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_starting(false);
                    if !success && ui.get_task_identity().is_empty() {
                        ui.set_tasks_open(false);
                    }
                });
            });
        }
        Err(_) => b.show_message(hfdx_core::locale::Message::CommandUnavailable),
    }
}
