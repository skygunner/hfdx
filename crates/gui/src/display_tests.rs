use crate::{AppWindow, files::FilesModel, view};
use hfdx_core::{
    repository::*,
    selection::{Selection, SelectionChange},
    storage::{Settings, TaskRecord, TaskState},
    task::{ActiveFile, AppSnapshot, TaskProgress, TaskView, UiPhase},
};
use slint::Model;
use std::{sync::Arc, time::Duration};

#[test]
fn gui_projects_selection_resets_and_live_transfer_units() {
    let source = RepositorySpec {
        request: RepositoryRequest {
            endpoint: Endpoint::parse("https://huggingface.co", false).unwrap(),
            kind: RepoKind::Model,
            repo_id: RepoId::parse("fixture/model").unwrap(),
            requested_revision: Revision::parse("main").unwrap(),
        },
        resolved_commit: CommitId::parse(&"a".repeat(40)).unwrap(),
    };
    let entries: Vec<_> = [("MODEL.GGUF", 1_000_000), ("config.json", 999)]
        .into_iter()
        .map(|(path, size)| CatalogEntry {
            path: RepoPath::parse(path).unwrap(),
            size: Bytes::new(size),
            content_hash: None,
        })
        .collect();
    let mut selection = Selection::new();
    let request = selection.begin(source.clone()).unwrap();
    selection
        .append(CatalogPage {
            catalog: request.id,
            entries: entries.clone(),
            complete: true,
        })
        .unwrap();
    let settings: Settings = serde_json::from_value(serde_json::json!({
        "endpoint": source.request.endpoint,
        "options": {"connections_per_file": 4, "concurrent_files": 5},
        "proxy_address": null, "recent_directories": [], "recent_tasks": []
    }))
    .unwrap();
    let mut snapshot = AppSnapshot {
        revision: 0,
        phase: UiPhase::Browsing,
        selection: selection.view(),
        task: None,
        settings: Arc::new(settings),
        recent: Arc::from([]),
        error: None,
        diagnostics: Default::default(),
        queue: None,
    };
    let ui = AppWindow::new().expect("native window");
    let files = FilesModel::new(selection.view());
    view::initialize(&ui, &snapshot);
    assert!(ui.get_filter().is_empty());
    ui.set_task_verify(false);
    ui.set_task_connections(9);
    view::apply(&ui, &snapshot);
    assert!(
        !ui.get_task_verify(),
        "refresh preserves this-task override"
    );
    assert_eq!(ui.get_task_connections(), 9);
    view::apply_draft_defaults(&ui, &snapshot.settings);
    assert!(
        ui.get_task_verify(),
        "new draft uses saved verification default"
    );
    assert_eq!(ui.get_task_connections(), 4);

    assert!(!ui.get_selected_only());
    assert_eq!(files.row_data(0).unwrap().size.as_str(), "1 MB");

    selection.set_filter("gguf").unwrap();
    selection.change(SelectionChange::SelectMatches).unwrap();
    selection.set_filter("config").unwrap();
    selection.set_selected_only(true);
    ui.set_filter("config".into());
    ui.set_selected_only(true);
    snapshot.selection = selection.view();
    view::apply(&ui, &snapshot);
    files.update(selection.view());
    assert_eq!(ui.get_filter().as_str(), "config");
    assert!(ui.get_selected_only());
    assert_eq!(ui.get_matching_count(), 1);
    assert_eq!(files.row_count(), 0);
    assert!(ui.get_can_start(), "hidden selection is still downloadable");
    assert!(ui.get_summary().contains("1 MB"));
    assert_eq!(snapshot.selection.summary.hidden_selected_count, 1);

    selection.set_filter("missing").unwrap();
    snapshot.selection = selection.view();
    view::apply(&ui, &snapshot);
    assert_eq!(ui.get_matching_count(), 0);

    ui.set_filter("typing ahead".into());
    view::apply(&ui, &snapshot);
    assert_eq!(ui.get_filter().as_str(), "typing ahead");

    selection.begin(source.clone()).unwrap();
    snapshot.selection = selection.view();
    ui.set_filter("old input".into());
    ui.set_selected_only(true);
    view::apply(&ui, &snapshot);
    assert!(ui.get_filter().is_empty());
    assert!(!ui.get_selected_only());

    let record: TaskRecord = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "task_id": "a".repeat(32), "source": source,
        "files": [{"entry": entries[0], "completed": false, "failure": null}],
        "options": {"connections_per_file": 4, "concurrent_files": 5},
        "proxy_address": null, "state": "running", "created_unix_ms": 1,
        "updated_unix_ms": 1
    }))
    .unwrap();
    snapshot.task = Some(TaskView {
        record: Arc::new(record),
        destination: "/fixture/download".into(),
        restart_notice: None,
        verification: None,
        progress: TaskProgress {
            completed_files: 0,
            total_files: 1,
            acquired_bytes: Bytes::new(12_400),
            total_bytes: Bytes::new(1_000_000),
            bytes_per_second: 32_500,
            eta: Some(Duration::from_secs(30)),
            active: Arc::from([ActiveFile {
                index: 0,
                completed_bytes: Bytes::new(12_400),
                bytes_per_second: 32_500,
            }]),
            failed: Arc::from([]),
        },
    });
    snapshot.phase = UiPhase::Downloading;
    view::apply(&ui, &snapshot);
    assert!(
        ui.get_progress_text()
            .contains("12.4 KB / 1 MB | 32.5 KB/s")
    );
    assert!(ui.get_activity().contains("12.4 KB / 1 MB | 32.5 KB/s"));
    assert!(ui.get_tasks_open(), "new task opens its dedicated page");
    assert_eq!(ui.get_task_tone(), 1);
    assert_eq!(ui.get_task_rows().row_data(0).unwrap().status_tone, 1);
    assert_eq!(ui.get_task_rows().row_count(), 1);
    assert_eq!(
        ui.get_task_rows().row_data(0).unwrap().speed.as_str(),
        "32.5 KB/s"
    );
    ui.set_tasks_open(false);
    let draft = ui.get_filter();
    view::apply(&ui, &snapshot);
    assert!(
        !ui.get_tasks_open(),
        "refresh does not pull the user back from setup"
    );
    assert_eq!(ui.get_filter(), draft);
    ui.set_task_status_filter(4);
    view::apply(&ui, &snapshot);
    assert_eq!(ui.get_task_rows().row_count(), 0);
    ui.set_task_status_filter(0);
    ui.set_tasks_open(true);

    let task = snapshot.task.as_mut().unwrap();
    Arc::make_mut(&mut task.record).state = TaskState::Paused;
    snapshot.phase = UiPhase::Paused;
    view::apply(&ui, &snapshot);
    assert!(ui.get_progress_text().contains("| Paused | ETA —"));
    assert!(!ui.get_activity().contains("32.5 KB/s"));
    assert!(ui.get_activity().contains("| Paused"));

    snapshot.phase = UiPhase::Stopping;
    view::apply(&ui, &snapshot);
    assert!(ui.get_progress_text().contains("| — | ETA —"));
    assert!(!ui.get_activity().contains("32.5 KB/s"));

    ui.set_token("memory-token".into());
    ui.set_proxy("http://user:secret@localhost:3128".into());
    ui.set_filter("unfinished input".into());
    ui.set_language_index(1);
    ui.set_theme_index(2);
    view::select_language(&ui);
    view::apply(&ui, &snapshot);
    assert_eq!(ui.get_token().as_str(), "memory-token");
    assert_eq!(ui.get_filter().as_str(), "unfinished input");
    assert!(ui.get_proxy().contains("secret"));
    assert_eq!(snapshot.selection.summary.selected_count, 0);
    assert_eq!(
        ui.get_status().as_str(),
        hfdx_core::locale::phase(hfdx_core::storage::Language::ZhHans, UiPhase::Stopping)
    );

    ui.set_language_index(0);
    view::select_language(&ui);
    let task = snapshot.task.as_mut().unwrap();
    let record = Arc::make_mut(&mut task.record);
    record.files[0].completed = true;
    record.state = TaskState::Completed;
    snapshot.phase = UiPhase::Paused;
    view::apply(&ui, &snapshot);
    assert!(
        ui.get_can_verify(),
        "completed legacy records must remain verifiable"
    );
    assert!(!ui.get_can_resume());
    assert!(ui.get_verification_text().contains("unverified: 1"));

    let task = snapshot.task.as_mut().unwrap();
    let record = Arc::make_mut(&mut task.record);
    record.verify_after_download = Some(true);
    record.state = TaskState::Running;
    record.files[0].verification = hfdx_core::storage::VerificationStatus::Pending;
    task.verification = Some(hfdx_core::task::VerificationProgress {
        file_index: 0,
        file_bytes_done: 500_000,
        files_done: 0,
        files_total: 1,
    });
    snapshot.phase = UiPhase::Verifying;
    view::apply(&ui, &snapshot);
    assert!(ui.get_can_pause());
    assert!(!ui.get_can_verify());
    assert!(ui.get_progress_text().contains("500 KB / 1 MB"));
    assert_eq!(ui.get_task_tone(), 2);
    assert_eq!(ui.get_task_rows().row_data(0).unwrap().verification_tone, 2);
    assert!((ui.get_progress() - 0.5).abs() < f32::EPSILON);
    assert_eq!(
        ui.get_status().as_str(),
        hfdx_core::locale::phase(hfdx_core::storage::Language::English, UiPhase::Verifying)
    );

    let task = snapshot.task.as_mut().unwrap();
    task.verification = None;
    let record = Arc::make_mut(&mut task.record);
    record.files[0].verification = hfdx_core::storage::VerificationStatus::Passed;
    record.state = TaskState::Completed;
    snapshot.phase = UiPhase::Paused;
    view::apply(&ui, &snapshot);
    assert!(ui.get_verification_text().contains("verified: 1"));
    assert_eq!(
        ui.get_status().as_str(),
        hfdx_core::locale::completion(
            hfdx_core::storage::Language::English,
            hfdx_core::task::CompletionOutcome::Verified,
            1,
            0,
            0
        )
    );
    let task = snapshot.task.as_mut().unwrap();
    let record = Arc::make_mut(&mut task.record);
    record.files[0].verification = hfdx_core::storage::VerificationStatus::Failed;
    record.state = TaskState::Failed;
    view::apply(&ui, &snapshot);
    assert_eq!(
        ui.get_task_rows().row_data(0).unwrap().status.as_str(),
        "Failed"
    );
    assert_eq!(
        ui.get_task_rows()
            .row_data(0)
            .unwrap()
            .verification
            .as_str(),
        "Failed"
    );
    assert_eq!(
        ui.get_status().as_str(),
        hfdx_core::locale::completion(
            hfdx_core::storage::Language::English,
            hfdx_core::task::CompletionOutcome::IntegrityFailed,
            0,
            0,
            1
        )
    );
    let task = snapshot.task.as_mut().unwrap();
    let record = Arc::make_mut(&mut task.record);
    record.verify_after_download = Some(false);
    record.files[0].verification = hfdx_core::storage::VerificationStatus::LegacyUnverified;
    record.state = TaskState::Completed;
    view::apply(&ui, &snapshot);
    assert!(!ui.get_can_verify());
    assert!(
        ui.get_verification_text()
            .contains("verification disabled: 1")
    );
    // The task table covers frozen files, including those not yet started.
    let mut task = snapshot.task.as_ref().unwrap().clone();
    let base = task.record.files[0].clone();
    let record = Arc::make_mut(&mut task.record);
    record.verify_after_download = Some(true);
    record.files = (0..5)
        .map(|i| {
            let mut f = base.clone();
            f.entry.path = RepoPath::parse(&format!("file-{i}.gguf")).unwrap();
            f.completed = i == 0;
            f.verification = if i == 0 {
                hfdx_core::storage::VerificationStatus::Passed
            } else {
                hfdx_core::storage::VerificationStatus::Pending
            };
            f.failure = None;
            f
        })
        .collect();
    record.files[2].failure = Some(hfdx_core::storage::FileFailure {
        kind: hfdx_core::storage::FailureKind::Network,
        message: "Network error".into(),
        aria2_code: Some(22),
        http_status: Some(429),
    });
    task.progress.active = Arc::from([ActiveFile {
        index: 3,
        completed_bytes: Bytes::new(500),
        bytes_per_second: 1000,
    }]);
    task.progress.failed = Arc::from([]);
    task.verification = Some(hfdx_core::task::VerificationProgress {
        file_index: 4,
        file_bytes_done: 100,
        files_done: 1,
        files_total: 5,
    });
    let model = crate::files::TaskFilesModel::new(
        task.clone(),
        hfdx_core::storage::Language::English,
        UiPhase::Downloading,
    );
    assert_eq!(model.row_count(), 5);
    assert_eq!(model.row_data(0).unwrap().status.as_str(), "Downloaded");
    assert_eq!(model.row_data(1).unwrap().status.as_str(), "Waiting");
    assert_eq!(model.row_data(1).unwrap().acquired.as_str(), "—");
    assert_eq!(model.row_data(2).unwrap().status.as_str(), "Failed");
    assert!(model.row_data(2).unwrap().error.contains("HTTP 429"));
    assert!(model.details(2).contains("aria2 22"));
    assert_eq!(model.row_data(3).unwrap().speed.as_str(), "1 KB/s");
    assert_eq!(model.row_data(4).unwrap().status.as_str(), "Verifying");
    model.update(
        task.clone(),
        hfdx_core::storage::Language::English,
        UiPhase::Downloading,
        "",
        4,
    );
    assert_eq!(model.row_count(), 1);
    assert_eq!(model.row_data(0).unwrap().index, 2);
    assert!(!model.details(2).is_empty());
    model.update(
        task,
        hfdx_core::storage::Language::English,
        UiPhase::Paused,
        "file-3",
        1,
    );
    assert_eq!(
        model.row_count(),
        0,
        "paused files are not counted as downloading"
    );
}

#[test]
fn task_colors_distinguish_verification_from_transfer_and_unchecked_files() {
    use crate::files::file_tones;
    use hfdx_core::storage::VerificationStatus::*;
    assert_eq!(file_tones(1, Pending, false), (1, 0));
    assert_eq!(file_tones(5, Pending, false), (2, 2));
    assert_eq!(file_tones(3, Passed, false), (3, 3));
    assert_eq!(file_tones(4, Failed, false), (5, 5));
    assert_eq!(file_tones(6, Passed, false), (3, 3));
    assert_eq!(file_tones(7, Unavailable, false), (4, 4));
    assert_eq!(file_tones(8, Unavailable, false), (4, 4));
    assert_eq!(file_tones(3, LegacyUnverified, false), (3, 4));
    assert_eq!(file_tones(3, Pending, true), (3, 4));
}
