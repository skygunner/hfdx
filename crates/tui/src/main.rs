mod settings;
mod ui;

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use hfdx_core::{
    logging::{self, LogConfig, LogLevel, SessionLogger},
    notification::{self, NativePlatform, NotificationPreferences},
    storage::{load_settings, settings_paths},
    task::{self, CompletionNotice, CoreConfig},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::sync::Arc;
use std::{
    io::{self, IsTerminal, stdout},
    path::PathBuf,
};

struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(stdout(), EnterAlternateScreen, Hide, EnableBracketedPaste)?;
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Err(error) = execute!(stdout(), DisableBracketedPaste, Show, LeaveAlternateScreen) {
            eprintln!("screen restoration failed: {error}");
        }
        if let Err(error) = disable_raw_mode() {
            eprintln!("terminal restoration failed: {error}");
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("hfdx: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--help"))
        || first.as_deref() == Some(std::ffi::OsStr::new("-h"))
    {
        println!(
            "hfdx [REPOSITORY] | --task PATH | --register-notifications\n--register-notifications: register Windows portable notification identity\nTab: next field  Ctrl+C: quit\n\n{}",
            hfdx_core::version::description()
        );
        return Ok(());
    }
    if first.as_deref() == Some(std::ffi::OsStr::new("--version"))
        || first.as_deref() == Some(std::ffi::OsStr::new("-V"))
    {
        println!("hfdx {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if first.as_deref() == Some(std::ffi::OsStr::new("--register-notifications")) {
        if args.next().is_some() {
            return Err("unexpected argument".into());
        }
        #[cfg(windows)]
        {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(notification::prepare_portable_identity(true))?;
            println!("Notification identity registered");
            return Ok(());
        }
        #[cfg(not(windows))]
        return Err("notification registration is supported only on Windows".into());
    }
    let (repo, task_path) = match first {
        Some(value) if value == "--task" => (
            String::new(),
            Some(PathBuf::from(args.next().ok_or("--task requires a path")?)),
        ),
        Some(value) if value.to_string_lossy().starts_with('-') => {
            return Err("unknown option".into());
        }
        Some(value) => (
            value
                .into_string()
                .map_err(|_| "repository must be UTF-8")?,
            None,
        ),
        None => (String::new(), None),
    };
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("an interactive terminal is required".into());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let paths = settings_paths()?;
    let loaded = runtime.block_on(load_settings(&paths))?;
    let logger = logging::default_log_directory()
        .map_err(|error| error.to_string())
        .and_then(|directory| {
            SessionLogger::new(LogConfig::new(directory).with_debug(loaded.debug_logging))
                .map_err(|error| error.to_string())
        });
    let (logger, logging_failed) = match logger {
        Ok(logger) => (Some(Arc::new(logger)), false),
        Err(_) => (None, true),
    };
    #[cfg(windows)]
    let (platform, identity_error) =
        match runtime.block_on(notification::prepare_portable_identity(false)) {
            Ok(platform) => (platform, None),
            Err(error) => (NativePlatform::new(None), Some(error)),
        };
    #[cfg(not(windows))]
    let (platform, identity_error) = (NativePlatform::new(None), None);
    let diagnostic = ui::Diagnostic {
        logging_failed,
        identity_error,
        notification: None,
    };
    let (notice_tx, mut notice_rx) = tokio::sync::mpsc::channel::<CompletionNotice>(8);
    let (warning_tx, warning_rx) = tokio::sync::watch::channel(diagnostic);
    let callback_warning = warning_tx.clone();
    let callback_logger = logger.clone();
    let completion = Arc::new(move |notice: CompletionNotice| {
        if let Err(error) = notice_tx.try_send(notice) {
            callback_warning.send_modify(|state| {
                state.notification = Some(ui::NotificationDiagnostic::QueueUnavailable)
            });
            if let Some(logger) = &callback_logger {
                let notice = error.into_inner();
                if logger
                    .record(
                        LogLevel::Warn,
                        hfdx_core::logging::LogEvent {
                            event: hfdx_core::logging::EventKind::NotificationFailed,
                            task_id: Some(&notice.task_id),
                            outcome: hfdx_core::logging::LogOutcome::Unavailable,
                            count: 0,
                            bytes: 0,
                            elapsed_ms: 0,
                        },
                    )
                    .is_err()
                {
                    callback_warning.send_modify(|state| state.logging_failed = true);
                }
            }
        }
    });
    let session = runtime.block_on(task::spawn_queue_with_services(
        CoreConfig {
            paths,
            aria2_executable: PathBuf::from("aria2c"),
        },
        completion,
        logger.clone(),
    ))?;
    let snapshots = session.snapshots.clone();
    let final_settings = snapshots.clone();
    let worker_logger = logger.clone();
    let final_warning = warning_tx.subscribe();
    let notification_worker = runtime.spawn(async move {
        while let Some(notice) = notice_rx.recv().await {
            let settings = snapshots.borrow().settings.clone();
            let preferences = NotificationPreferences {
                notify: settings.notify_on_completion,
                sound: settings.sound_on_completion,
                language: settings.effective_language(),
            };
            if let Err(error) = notification::dispatch(&platform, &notice, preferences).await {
                warning_tx.send_modify(|state| {
                    state.notification =
                        Some(if error == notification::NotificationError::TimedOut {
                            ui::NotificationDiagnostic::TimedOut
                        } else {
                            ui::NotificationDiagnostic::DeliveryFailed
                        })
                });
                if let Some(logger) = &worker_logger
                    && logger
                        .record(
                            LogLevel::Warn,
                            hfdx_core::logging::LogEvent {
                                event: hfdx_core::logging::EventKind::NotificationFailed,
                                task_id: Some(&notice.task_id),
                                outcome: hfdx_core::logging::LogOutcome::Unavailable,
                                count: 0,
                                bytes: 0,
                                elapsed_ms: 0,
                            },
                        )
                        .is_err()
                {
                    warning_tx.send_modify(|state| state.logging_failed = true);
                }
            }
        }
    });
    let guard = TerminalGuard::enter();
    let result = match &guard {
        Ok(_) => Terminal::new(CrosstermBackend::new(stdout())).and_then(|mut terminal| {
            ui::run(
                &mut terminal,
                &runtime,
                &session,
                repo,
                task_path,
                warning_rx,
            )
        }),
        Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
    };
    let shutdown = session.handle.request_shutdown();
    let joined = runtime.block_on(session.join());
    let notification_joined = runtime.block_on(notification_worker);
    drop(guard);
    let warning = final_warning
        .borrow()
        .render(final_settings.borrow().settings.effective_language());
    if !warning.is_empty() {
        eprintln!("hfdx: {warning}");
    }
    if let Err(error) = shutdown {
        eprintln!("shutdown request: {error:?}");
    }
    if let Some(logger) = &logger
        && logger.flush().is_err()
    {
        eprintln!(
            "hfdx: {}",
            ui::Diagnostic {
                logging_failed: true,
                ..Default::default()
            }
            .render(final_settings.borrow().settings.effective_language())
        );
    }
    let restart = result?;
    joined?;
    notification_joined?;
    if let Some(path) = restart {
        // Keep the interactive shell waiting for its foreground command.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            return Err(std::process::Command::new(path).exec().into());
        }
        #[cfg(not(unix))]
        if !std::process::Command::new(path).status()?.success() {
            return Err("Restarted TUI exited unsuccessfully".into());
        }
    }
    Ok(())
}
