#[path = "diagnostics.rs"]
mod diagnostics;
#[path = "notifications.rs"]
mod notifications;
#[path = "services.rs"]
mod services;
#[path = "bridge/startup.rs"]
mod startup;
use crate::{AppWindow, view};
use hfdx_core::task::{self, AppSnapshot, CommandSendError, CoreCommand, CoreConfig, CoreHandle};
use slint::ComponentHandle;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub struct Mailbox {
    pub snapshot: Arc<AppSnapshot>,
    pub error: Option<String>,
    pub finished: bool,
}
pub struct Bridge {
    pub handle: CoreHandle,
    pub runtime: tokio::runtime::Handle,
    pub mailbox: Arc<Mutex<Mailbox>>,
    services: Arc<services::Services>,
    rendered: Arc<Mutex<diagnostics::DiagnosticRender>>,
    pending: Arc<AtomicBool>,
    ui: slint::Weak<AppWindow>,
}
impl Clone for Bridge {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            runtime: self.runtime.clone(),
            mailbox: self.mailbox.clone(),
            services: self.services.clone(),
            rendered: self.rendered.clone(),
            pending: self.pending.clone(),
            ui: self.ui.clone(),
        }
    }
}
impl Bridge {
    pub fn apply_diagnostics(&self, ui: &AppWindow) {
        let local = self.services.text(crate::input::language(ui));
        match self.rendered.lock() {
            Ok(mut rendered) => {
                ui.set_diagnostics_status(
                    rendered
                        .status
                        .apply(&ui.get_diagnostics_status(), &local)
                        .into(),
                );
                ui.set_error(rendered.error.apply(&ui.get_error(), &local).into());
            }
            Err(_) => eprintln!("GUI diagnostic rendering unavailable"),
        }
    }
    pub fn ui(&self) -> &slint::Weak<AppWindow> {
        &self.ui
    }
    fn refresh(&self) {
        if !self.pending.swap(true, Ordering::AcqRel)
            && self
                .ui
                .upgrade_in_event_loop(|ui| ui.invoke_refresh())
                .is_err()
        {
            self.pending.store(false, Ordering::Release);
            eprintln!("GUI event loop unavailable");
            if self.handle.request_shutdown().is_err() {
                eprintln!("Core already closed");
            }
        }
    }
    pub fn take_refresh(&self) -> Result<(Arc<AppSnapshot>, Option<String>, bool), String> {
        let mut box_ = self.mailbox.lock().map_err(|_| "GUI mailbox unavailable")?;
        self.pending.store(false, Ordering::Release);
        let error = if box_.finished {
            box_.error.clone()
        } else {
            box_.error.take()
        };
        Ok((box_.snapshot.clone(), error, box_.finished))
    }
    pub fn submit(&self, command: CoreCommand) {
        match self.handle.submit(command) {
            Ok(receipt) => {
                // Core publishes command errors in its snapshot; do not detach a receipt task.
                drop(receipt);
            }
            Err(error) => self.show_message(match error {
                CommandSendError::Busy => hfdx_core::locale::Message::CommandUnavailable,
                CommandSendError::Closed => hfdx_core::locale::Message::Closed,
            }),
        }
    }
    pub fn show_message(&self, message: hfdx_core::locale::Message) {
        let language = self
            .mailbox
            .lock()
            .map(|b| b.snapshot.settings.effective_language())
            .unwrap_or(hfdx_core::storage::Language::English);
        self.show_error(hfdx_core::locale::text(language, message));
    }
    pub fn show_error(&self, error: &str) {
        match self.mailbox.lock() {
            Ok(mut box_) => box_.error = Some(error.into()),
            Err(_) => eprintln!("GUI error mailbox unavailable"),
        }
        self.refresh();
    }
    pub fn close(&self, ui: &AppWindow) -> slint::CloseRequestResponse {
        let finished = self.mailbox.lock().map(|b| b.finished).unwrap_or(true);
        if finished {
            return slint::CloseRequestResponse::HideWindow;
        }
        ui.set_status(crate::input::message(ui, hfdx_core::locale::Message::Stopping).into());
        ui.set_editable(false);
        if let Err(error) = self.handle.request_shutdown() {
            self.show_message(match error {
                CommandSendError::Busy => hfdx_core::locale::Message::CommandUnavailable,
                CommandSendError::Closed => hfdx_core::locale::Message::Closed,
            });
        }
        slint::CloseRequestResponse::KeepWindowShown
    }
}

pub fn finish(ui: &AppWindow) {
    if ui.hide().is_err() {
        eprintln!("Cannot hide GUI window");
    }
    if slint::quit_event_loop().is_err() {
        eprintln!("Cannot stop GUI event loop");
    }
}

pub fn refresh_ui(ui: &AppWindow, bridge: &Bridge, files: &crate::files::FilesModel) {
    match bridge.take_refresh() {
        Ok((snapshot, error, finished)) => {
            view::apply(ui, &snapshot);
            crate::callbacks::destination(ui, &snapshot);
            files.update(snapshot.selection.clone());
            if let Some(error) = error {
                ui.set_error(error.into());
            }
            bridge.apply_diagnostics(ui);
            if finished {
                finish(ui);
            }
        }
        Err(error) => ui.set_error(error.into()),
    }
}
