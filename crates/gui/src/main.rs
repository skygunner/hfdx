mod bridge;
mod callbacks;
#[cfg(test)]
mod display_tests;
mod files;
mod input;
mod queue_view;
mod view;

use hfdx_core::{
    storage::settings_paths,
    task::{CoreCommand, CoreConfig},
};
use slint::ComponentHandle;
use std::path::PathBuf;

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|arg| arg == "--register-notifications")
    {
        if arguments.len() != 1 {
            return Err("--register-notifications must be the only argument".into());
        }
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(hfdx_core::notification::prepare_portable_identity(true))?;
        println!("Notification identity registered.");
        return Ok(());
    }
    let mut args = arguments.into_iter();
    let mut repository = None;
    let mut task_file = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "hfdx-gui [REPOSITORY] [--task PATH] [--register-notifications]\n--register-notifications: register Windows portable notification identity\nDownloads stop and save when the window closes.\n\n{}",
                    hfdx_core::version::description()
                );
                return Ok(());
            }
            "--version" | "-V" => {
                println!("hfdx-gui {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--task" => task_file = Some(PathBuf::from(args.next().ok_or("--task needs a path")?)),
            value if value.starts_with('-') => return Err("Unknown option; use --help".into()),
            _ => {
                if repository.replace(arg).is_some() {
                    return Err("Only one repository is supported".into());
                }
            }
        }
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let platform = runtime.block_on(hfdx_core::notification::prepare_portable_identity(false));
    let ui = AppWindow::new()?;
    let (bridge, worker) = bridge::Bridge::start_with_platform(
        &ui,
        CoreConfig {
            paths: settings_paths()?,
            aria2_executable: PathBuf::from("aria2c"),
        },
        platform,
    )?;
    let snapshot = bridge
        .mailbox
        .lock()
        .map_err(|_| "Snapshot unavailable")?
        .snapshot
        .clone();
    let files = files::FilesModel::new(snapshot.selection.clone());
    ui.set_rows(slint::ModelRc::from(files.clone()));
    view::initialize(&ui, &snapshot);
    bridge.apply_diagnostics(&ui);
    if let Some(repository) = repository {
        ui.set_repository(repository.into());
    }
    callbacks::connect(&ui, &bridge, &files);
    if let Some(path) = task_file {
        bridge.submit(CoreCommand::OpenTask(path));
    }
    let updates = std::rc::Rc::new(std::cell::RefCell::new(hfdx_core::updates::Updates::new(
        "hfdx-gui",
    )));
    let restart = std::rc::Rc::new(std::cell::Cell::new(false));
    let timer = slint::Timer::default();
    {
        let u = updates.clone();
        let b = bridge.clone();
        let weak = ui.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(500),
            move || {
                let Some(ui) = weak.upgrade() else { return };
                let snapshot = b.mailbox.lock().unwrap().snapshot.clone();
                let mut u = u.borrow_mut();
                u.tick(&snapshot.settings, &b.runtime);
                ui.set_update_status(u.status.as_str().into());
                ui.set_can_update(u.can_install());
                ui.set_can_restart(u.restart_ready(&snapshot));
                ui.set_restart_prompt(u.restart_ready(&snapshot) && !u.later);
            },
        );
    }
    {
        let u = updates.clone();
        let b = bridge.clone();
        ui.on_maintenance(move |i| {
            let snapshot = b.mailbox.lock().unwrap().snapshot.clone();
            let mut u = u.borrow_mut();
            match i {
                0 => u.check(&b.runtime),
                1 => u.install(&b.runtime),
                2 => {
                    if let Err(e) = hfdx_core::updates::issue(&snapshot, "GUI") {
                        b.show_error(&e.to_string())
                    }
                }
                3 => u.later = false,
                _ => {}
            }
        });
    }
    {
        let u = updates.clone();
        let b = bridge.clone();
        let restart = restart.clone();
        let weak = ui.as_weak();
        ui.on_restart_choice(move |yes| {
            u.borrow_mut().later = true;
            if let Some(ui) = weak.upgrade() {
                ui.set_restart_prompt(false);
            }
            if yes
                && u.borrow()
                    .restart_ready(&b.mailbox.lock().unwrap().snapshot)
            {
                restart.set(true);
                let _ = slint::quit_event_loop();
            }
        });
    }
    let result = ui.run();
    timer.stop();
    // Closing the window normally has already stopped the core.
    let _ = bridge.handle.request_shutdown();
    let joined = worker.join();
    result?;
    match joined {
        Ok(Ok(())) => {
            if restart.get() {
                hfdx_core::updates::restart(
                    updates
                        .borrow()
                        .executable
                        .as_deref()
                        .ok_or("Missing executable")?,
                )?;
            }
            Ok(())
        }
        Ok(Err(error)) => Err(error.into()),
        Err(_) => Err("Core background thread panicked".into()),
    }
}
