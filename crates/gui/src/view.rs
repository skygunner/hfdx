#[path = "view/task.rs"]
mod task;

use crate::{AppWindow, RecentRow};
use hfdx_core::{
    display,
    locale::{self, Message},
    selection::CatalogState,
    storage::{Language, TaskState, Theme},
    task::{AppSnapshot, UiPhase},
};
use slint::{ModelRc, VecModel};
use std::rc::Rc;

pub fn initialize(ui: &AppWindow, snapshot: &AppSnapshot) {
    ui.set_build_info(hfdx_core::version::description().into());
    ui.set_endpoint(snapshot.settings.endpoint.url().as_str().into());
    ui.set_hf_token(
        snapshot
            .settings
            .hf_token
            .as_ref()
            .map_or("", |t| t.expose())
            .into(),
    );
    ui.set_modelscope_token(
        snapshot
            .settings
            .modelscope_token
            .as_ref()
            .map_or("", |t| t.expose())
            .into(),
    );
    if snapshot.settings.endpoint.is_modelscope() {
        ui.set_revision("master".into());
    }
    ui.set_allow_http(snapshot.settings.endpoint.url().scheme() == "http");
    ui.set_proxy(
        snapshot
            .settings
            .proxy_address
            .as_ref()
            .map(|p| p.url().as_str())
            .unwrap_or("")
            .into(),
    );
    ui.set_filter(snapshot.selection.filter.as_str().into());
    ui.set_selected_only(snapshot.selection.selected_only);
    ui.set_connections(i32::from(snapshot.settings.options.connections_per_file()));
    ui.set_concurrent_files(i32::from(snapshot.settings.options.concurrent_files()));
    ui.set_verify(snapshot.settings.verify_after_download);
    ui.set_task_connections(i32::from(snapshot.settings.options.connections_per_file()));
    ui.set_task_concurrent_files(i32::from(snapshot.settings.options.concurrent_files()));
    ui.set_task_verify(snapshot.settings.verify_after_download);
    ui.set_notify(snapshot.settings.notify_on_completion);
    ui.set_sound(snapshot.settings.sound_on_completion);
    ui.set_debug(snapshot.settings.debug_logging);
    ui.set_language_index(match snapshot.settings.effective_language() {
        Language::English => 0,
        Language::ZhHans => 1,
        Language::ZhHant => 2,
    });
    ui.set_theme_index(match snapshot.settings.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    });
    select_language(ui);
    apply(ui, snapshot);
}

pub fn select_language(ui: &AppWindow) {
    let language = match crate::input::language(ui) {
        Language::English => "en",
        Language::ZhHans => "zh_CN",
        Language::ZhHant => "zh_TW",
    };
    if let Err(error) = slint::select_bundled_translation(language) {
        ui.set_error(
            format!(
                "{}: {error}",
                crate::input::message(ui, Message::InternalError)
            )
            .into(),
        );
    }
}

pub fn apply(ui: &AppWindow, snapshot: &AppSnapshot) {
    let language = crate::input::language(ui);
    let text = |m| locale::text(language, m);
    ui.set_token_help(text(Message::TokenHelp).into());
    ui.set_token_notice(
        if crate::input::access(ui).is_ok_and(|a| a.token.is_none()) {
            text(Message::TokenMissing)
        } else {
            ""
        }
        .into(),
    );
    let log_directory = snapshot
        .diagnostics
        .log_directory
        .clone()
        .or_else(|| hfdx_core::logging::default_log_directory().ok());
    ui.set_log_directory(
        log_directory
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
    ui.set_diagnostics_status(
        match snapshot.diagnostics.logging {
            Some(hfdx_core::logging::LogStatus::Ready) => "",
            Some(
                hfdx_core::logging::LogStatus::BudgetExceeded
                | hfdx_core::logging::LogStatus::Failed(_),
            )
            | None => text(Message::DiskError),
        }
        .into(),
    );
    let editable = if snapshot.queue.is_some() {
        !matches!(snapshot.selection.state, CatalogState::Loading)
            && !matches!(snapshot.phase, UiPhase::Stopping | UiPhase::Closed)
    } else {
        matches!(
            snapshot.phase,
            UiPhase::Idle | UiPhase::Browsing | UiPhase::Paused
        )
    };
    ui.set_status(locale::phase(language, snapshot.phase).into());
    ui.set_editable(editable);
    ui.set_error(
        (if !ui.get_tasks_open() {
            snapshot
                .queue
                .as_ref()
                .and_then(|q| q.draft_error.as_ref())
                .or(snapshot.error.as_ref())
        } else {
            snapshot.error.as_ref()
        })
        .map(|e| locale::error(language, e))
        .unwrap_or_default()
        .into(),
    );
    let selection = &snapshot.selection;
    let ready = selection.state == CatalogState::Ready;
    ui.set_ready(ready);
    ui.set_loading(selection.state == CatalogState::Loading);
    let catalog_key = selection.catalog.as_ref().map_or_else(
        || {
            if snapshot.phase == UiPhase::Loading {
                "loading".to_owned()
            } else {
                String::new()
            }
        },
        |catalog| format!("{:?}", catalog.id),
    );
    if ui.get_catalog_key().as_str() != catalog_key {
        if ready {
            apply_draft_defaults(ui, &snapshot.settings);
        }
        ui.set_filter(selection.filter.as_str().into());
        ui.set_selected_only(selection.selected_only);
        ui.set_catalog_key(catalog_key.into());
    }
    let profiles = if ready {
        selection
            .catalog
            .as_ref()
            .map(|c| hfdx_core::profiles::analyze(c))
            .unwrap_or_default()
    } else {
        &[]
    };
    ui.set_profiles(ModelRc::from(Rc::new(VecModel::from(
        profiles
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.label
                    .to_lowercase()
                    .contains(&ui.get_profile_filter().to_lowercase())
            })
            .map(|(index, p)| crate::ProfileRow {
                index: i32::try_from(index).unwrap_or(i32::MAX),
                label: format!(
                    "{} · {} · {}{}",
                    p.display_label(language),
                    p.files.len(),
                    display::bytes(p.bytes),
                    if p.complete { "" } else { " [incomplete]" }
                )
                .into(),
                selected: p.files.iter().all(|id| selection.selected.contains(id)),
                partial: !p.files.iter().all(|id| selection.selected.contains(id))
                    && p.files.iter().any(|id| {
                        selection.selected.contains(id)
                            && selection
                                .catalog
                                .as_ref()
                                .and_then(|c| c.entry(*id))
                                .is_some_and(|e| {
                                    let path = e.path.as_str().to_ascii_lowercase();
                                    path.ends_with(".gguf") && !path.contains("mmproj")
                                })
                    }),
                complete: p.complete,
            })
            .collect::<Vec<_>>(),
    ))));
    ui.set_matching_count(i32::try_from(selection.matches.len()).unwrap_or(i32::MAX));
    ui.set_selected_count(i32::try_from(selection.summary.selected_count).unwrap_or(i32::MAX));
    ui.set_hidden_count(i32::try_from(selection.summary.hidden_selected_count).unwrap_or(i32::MAX));
    let matched_selected = selection
        .matches
        .iter()
        .filter(|id| selection.selected.contains(id))
        .count();
    ui.set_header_state(if selection.matches.is_empty() || matched_selected == 0 {
        0
    } else if matched_selected == selection.matches.len() {
        2
    } else {
        1
    });
    ui.set_can_start(ready && selection.summary.selected_count > 0 && editable);
    ui.set_start_reason(
        if selection.state == CatalogState::Loading {
            text(Message::Loading)
        } else if !ready {
            match language {
                Language::English => "View the repository file list first.",
                Language::ZhHans => "请先查看仓库文件列表。",
                Language::ZhHant => "請先查看儲存庫檔案清單。",
            }
        } else if selection.summary.selected_count == 0 {
            match language {
                Language::English => "Select at least one file.",
                Language::ZhHans => "请至少勾选一个文件。",
                Language::ZhHant => "請至少勾選一個檔案。",
            }
        } else if !editable {
            locale::phase(language, snapshot.phase)
        } else {
            ""
        }
        .into(),
    );
    let (verify, connections, concurrent) = snapshot.task.as_ref().map_or_else(
        || {
            (
                snapshot.settings.verify_after_download,
                snapshot.settings.options.connections_per_file(),
                snapshot.settings.options.concurrent_files(),
            )
        },
        |task| {
            (
                task.record.verify_after_download != Some(false),
                task.record.options.connections_per_file(),
                task.record.options.concurrent_files(),
            )
        },
    );
    ui.set_options_summary(
        format!(
            "{}: {} | {}: {connections} | {}: {concurrent}",
            text(Message::VerifyAfterDownload),
            text(if verify { Message::On } else { Message::Off }),
            text(Message::ConnectionsPerFile),
            text(Message::ConcurrentFiles)
        )
        .into(),
    );
    ui.set_catalog_summary(
        selection
            .catalog
            .as_ref()
            .map(|catalog| {
                format!(
                    "{} | {} {} | {}",
                    text(
                        if catalog.source.request.kind == hfdx_core::repository::RepoKind::Dataset {
                            Message::Dataset
                        } else {
                            Message::Model
                        }
                    ),
                    catalog.len(),
                    text(Message::Files),
                    catalog.source.request.requested_revision.as_str()
                )
            })
            .unwrap_or_default()
            .into(),
    );
    let state = text(match selection.state {
        CatalogState::Empty => Message::Idle,
        CatalogState::Loading => Message::Loading,
        CatalogState::Ready => Message::Completed,
        CatalogState::Failed => Message::Failed,
    });
    let count = selection.catalog.as_ref().map_or(0, |c| c.len());
    ui.set_catalog_status(
        format!(
            "{state}: {count} {} | {}: {} | {}: {} | {}: {}",
            text(Message::Files),
            text(Message::Match),
            selection.matches.len(),
            text(Message::Visible),
            selection.visible.len(),
            text(Message::Selected),
            selection.summary.selected_count
        )
        .into(),
    );
    ui.set_summary(
        format!(
            "{}: {} {} | {} | {}: {}",
            text(Message::Selected),
            selection.summary.selected_count,
            text(Message::Files),
            display::bytes(selection.summary.selected_bytes.get()),
            text(Message::Hidden),
            selection.summary.hidden_selected_count
        )
        .into(),
    );
    let pause_allowed = matches!(snapshot.phase, UiPhase::Downloading | UiPhase::Verifying);
    if !pause_allowed {
        ui.set_pausing(false);
    }
    ui.set_can_pause(pause_allowed && !ui.get_pausing());
    ui.set_can_resume(
        snapshot
            .task
            .as_ref()
            .is_some_and(|t| t.record.state != TaskState::Completed)
            && matches!(
                snapshot.phase,
                UiPhase::Paused | UiPhase::Browsing | UiPhase::Idle
            ),
    );
    ui.set_can_retry(
        snapshot
            .task
            .as_ref()
            .is_some_and(|t| !t.progress.failed.is_empty())
            && matches!(
                snapshot.phase,
                UiPhase::Paused | UiPhase::Browsing | UiPhase::Idle
            ),
    );
    ui.set_can_open_folder(snapshot.task.as_ref().is_some_and(|task| {
        task.destination.is_dir()
            && matches!(
                snapshot.phase,
                UiPhase::Idle | UiPhase::Paused | UiPhase::Browsing
            )
    }));
    ui.set_can_verify(
        snapshot.task.as_ref().is_some_and(|t| {
            t.record.verify_after_download != Some(false)
                && t.record.files.iter().all(|f| {
                    f.completed
                        && f.failure.is_none()
                        && !matches!(
                            f.disposition,
                            hfdx_core::storage::FileDisposition::KeptDifferent
                                | hfdx_core::storage::FileDisposition::TimeUnknown
                                | hfdx_core::storage::FileDisposition::Unchecked
                        )
                })
        }) && matches!(
            snapshot.phase,
            UiPhase::Paused | UiPhase::Browsing | UiPhase::Idle
        ),
    );
    task::apply(ui, snapshot);
    crate::queue_view::apply(ui, snapshot);
    if snapshot
        .queue
        .as_ref()
        .is_some_and(|q| q.preparing || q.conflicts.is_some())
    {
        ui.set_can_start(false);
    }
    ui.set_recent(ModelRc::from(Rc::new(VecModel::from(
        snapshot
            .recent
            .iter()
            .map(|r| RecentRow {
                path: r.task_file.to_string_lossy().as_ref().into(),
                status: format!(
                    "{}: {} | {}{}",
                    text(Message::Destination),
                    text(if r.destination_exists {
                        Message::Exists
                    } else {
                        Message::Missing
                    }),
                    r.state
                        .map(|s| locale::state(language, s))
                        .unwrap_or(text(Message::Unknown)),
                    r.issue
                        .as_ref()
                        .map(|_| format!(" | {}", text(Message::InputError)))
                        .unwrap_or_default()
                )
                .into(),
            })
            .collect::<Vec<_>>(),
    ))));
    ui.set_directories(ModelRc::from(Rc::new(VecModel::from(
        snapshot
            .settings
            .recent_directories
            .iter()
            .map(|p| p.to_string_lossy().as_ref().into())
            .collect::<Vec<slint::SharedString>>(),
    ))));
}

/// A newly loaded draft catalog starts from saved defaults without dropping the current task.
pub fn apply_draft_defaults(ui: &AppWindow, settings: &hfdx_core::storage::Settings) {
    ui.set_task_connections(i32::from(settings.options.connections_per_file()));
    ui.set_task_concurrent_files(i32::from(settings.options.concurrent_files()));
    ui.set_task_verify(settings.verify_after_download);
}
