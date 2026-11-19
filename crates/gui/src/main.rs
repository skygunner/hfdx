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
    let result = ui.run();
    if bridge.handle.request_shutdown().is_err() {
        eprintln!("Core already closed");
    }
    let joined = worker.join();
    result?;
    match joined {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error.into()),
        Err(_) => Err("Core background thread panicked".into()),
    }
}
