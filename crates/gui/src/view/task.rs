use crate::AppWindow;
use hfdx_core::{
    display,
    locale::{self, Message},
    storage::{TaskState, VerificationStatus},
    task::{AppSnapshot, CompletionOutcome, UiPhase},
};
use slint::Model;

pub fn apply(ui: &AppWindow, snapshot: &AppSnapshot) {
    let language = crate::input::language(ui);
    let text = |m| locale::text(language, m);
    let Some(task) = &snapshot.task else {
        ui.set_task_tone(0);
        if !ui.get_starting() && snapshot.queue.is_none() {
            ui.set_tasks_open(false);
        }
        ui.set_task_error_details("".into());
        ui.set_can_remove_task(false);
        ui.set_queue_details("".into());
        ui.set_remove_confirmation_id("".into());
        ui.set_task_rows(Default::default());
        ui.set_opened_task_id("".into());
        ui.set_task_destination("".into());
        ui.set_task_identity("".into());
        ui.set_progress_text("".into());
        ui.set_verification_text("".into());
        ui.set_activity("".into());
        ui.set_progress(0.0);
        return;
    };
    ui.set_task_tone(crate::files::phase_tone(task, snapshot.phase));
    let id = task.record.task_id.as_str();
    if ui.get_opened_task_id().as_str() != id {
        ui.set_remove_confirmation_id("".into());
        ui.set_error_details_open(false);
        let requested = ui.get_starting();
        ui.set_starting(false);
        if !requested {
            ui.set_tasks_open(true);
        }
        ui.set_pausing(false);
        if snapshot.queue.is_none() {
            ui.set_task_filter("".into());
            ui.set_task_status_filter(0);
            ui.set_task_file_details("".into());
            ui.set_task_file_index(-1);
            let request = &task.record.source.request;
            let previous =
                hfdx_core::repository::Endpoint::parse(&ui.get_endpoint(), ui.get_allow_http());
            if previous.as_ref().ok() != Some(&request.endpoint) {
                ui.set_token("".into());
            }
            ui.set_endpoint(request.endpoint.url().as_str().into());
            ui.set_allow_http(request.endpoint.url().scheme() == "http");
            ui.set_revision(request.requested_revision.as_str().into());
            ui.set_repository(request.repo_id.as_str().into());
            ui.set_repository_kind(i32::from(
                request.kind == hfdx_core::repository::RepoKind::Dataset,
            ));
            ui.set_destination(task.destination.to_string_lossy().as_ref().into());
        }
        ui.set_opened_task_id(id.into());
    }
    ui.set_can_remove_task(
        snapshot
            .queue
            .as_ref()
            .is_some_and(|q| !q.preparing && q.conflicts.is_none()),
    );
    let mut details = snapshot
        .error
        .as_ref()
        .map(|error| locale::error(language, error))
        .unwrap_or_default();
    if let Some((index, failure)) = task.progress.failed.first()
        && let Some(file) = task.record.files.get(*index)
    {
        if !details.is_empty() {
            details.push('\n');
        }
        details.push_str(file.entry.path.as_str());
        details.push('\n');
        details.push_str(&locale::file_failure(language, failure));
    }
    ui.set_task_error_details(details.into());
    let model = ui.get_task_rows();
    let model = if model.as_any().is::<crate::files::TaskFilesModel>() {
        model
    } else {
        slint::ModelRc::from(std::rc::Rc::new(crate::files::TaskFilesModel::new(
            task.clone(),
            language,
            snapshot.phase,
        )))
    };
    if let Some(files) = model
        .as_any()
        .downcast_ref::<crate::files::TaskFilesModel>()
    {
        files.update(
            task.clone(),
            language,
            snapshot.phase,
            &ui.get_task_filter(),
            ui.get_task_status_filter(),
        );
        ui.set_task_counts(files.counts().into());
        if let Ok(index) = usize::try_from(ui.get_task_file_index()) {
            ui.set_task_file_details(files.details(index).into());
        }
    }
    ui.set_task_rows(model);
    let record = &task.record;
    ui.set_task_identity(
        format!(
            "{} @ {} -> {} | {}",
            record.source.request.repo_id.as_str(),
            record.source.request.requested_revision.as_str(),
            record.source.resolved_commit.as_str(),
            locale::state(language, record.state)
        )
        .into(),
    );
    ui.set_task_destination(task.destination.to_string_lossy().as_ref().into());
    let mut passed = 0u64;
    let mut unavailable = 0u64;
    let mut failed = 0u64;
    let mut pending = 0u64;
    for file in &record.files {
        match file.verification {
            VerificationStatus::Passed => passed += 1,
            VerificationStatus::Unavailable | VerificationStatus::LegacyUnverified => {
                unavailable += 1
            }
            VerificationStatus::Failed => failed += 1,
            VerificationStatus::Pending => pending += 1,
        }
    }
    let disabled = record.verify_after_download == Some(false);
    let outcome = if failed > 0 {
        CompletionOutcome::IntegrityFailed
    } else if disabled {
        CompletionOutcome::VerificationDisabled
    } else if unavailable > 0 || pending > 0 {
        CompletionOutcome::Unverified
    } else {
        CompletionOutcome::Verified
    };
    let finished = !matches!(
        snapshot.phase,
        UiPhase::Downloading | UiPhase::Preparing | UiPhase::Verifying
    ) && (record.state == TaskState::Completed || failed > 0);
    let counts = if disabled {
        format!(
            "{}: {} {}",
            text(Message::DisabledCount),
            record.files.len(),
            text(Message::Files)
        )
    } else {
        format!(
            "{}: {passed} | {}: {unavailable} | {}: {failed} | {}: {pending}",
            text(Message::PassedCount),
            text(Message::UnavailableCount),
            text(Message::FailedCount),
            text(Message::PendingCount)
        )
    };
    let mut verification = counts;
    if unavailable > 0 && !disabled {
        verification.push_str(&format!(" | {}", text(Message::NoSupportedDigest)));
    }
    ui.set_verification_text(verification.into());
    if finished {
        ui.set_status(
            locale::completion(
                language,
                outcome,
                passed,
                if disabled { 0 } else { unavailable },
                failed,
            )
            .into(),
        );
    }
    if snapshot.phase == UiPhase::Downloading && task.verification.is_some() {
        ui.set_status(
            format!(
                "{} · {}",
                locale::phase(language, UiPhase::Downloading),
                locale::phase(language, UiPhase::Verifying)
            )
            .into(),
        );
    }
    let p = &task.progress;
    let downloading = snapshot.phase == UiPhase::Downloading;
    let paused = snapshot.phase == UiPhase::Paused && record.state == TaskState::Paused;
    let rate = |value| {
        if paused {
            text(Message::Paused).to_owned()
        } else {
            display::speed(downloading.then_some(value))
        }
    };
    let eta = p
        .eta
        .filter(|_| downloading)
        .map(|d| format!("{}s", d.as_secs()))
        .unwrap_or_else(|| "—".into());
    let mut done = p.acquired_bytes.get();
    let mut total = p.total_bytes.get();
    let mut lines = Vec::new();
    if let Some(v) = task.verification
        && snapshot.phase == UiPhase::Verifying
    {
        total = record.files.iter().map(|f| f.entry.size.get()).sum();
        done = record
            .files
            .iter()
            .enumerate()
            .filter(|(i, f)| {
                *i != v.file_index
                    && matches!(
                        f.verification,
                        VerificationStatus::Passed | VerificationStatus::Unavailable
                    )
            })
            .map(|(_, f)| f.entry.size.get())
            .sum::<u64>()
            .saturating_add(v.file_bytes_done);
        let name = record
            .files
            .get(v.file_index)
            .map(|f| f.entry.path.as_str())
            .unwrap_or("");
        lines.push(format!(
            "{}: {name} | {}",
            text(Message::Verifying),
            display::bytes(v.file_bytes_done)
        ));
        ui.set_progress_text(
            format!(
                "{}: {}/{} {} | {} / {} | {}",
                text(Message::Verifying),
                v.files_done,
                v.files_total,
                text(Message::Files),
                display::bytes(done),
                display::bytes(total),
                locale::phase(language, snapshot.phase)
            )
            .into(),
        );
    } else {
        ui.set_progress_text(
            format!(
                "{}/{} {} | {} / {} | {} | ETA {eta}",
                p.completed_files,
                p.total_files,
                text(Message::Files),
                display::bytes(done),
                display::bytes(total),
                rate(p.bytes_per_second)
            )
            .into(),
        );
    }
    let percent = if total == 0 {
        if finished { 100 } else { 0 }
    } else {
        u32::try_from(u128::from(done) * 100 / u128::from(total))
            .unwrap_or(100)
            .min(100)
    };
    ui.set_progress(f32::from(u16::try_from(percent).unwrap_or(100)) / 100.0);
    if task.restart_notice.is_some() {
        lines.push(text(Message::Interrupted).into());
    }
    for file in p.active.iter() {
        if let Some(item) = record.files.get(file.index) {
            lines.push(format!(
                "{}: {} | {} / {} | {}",
                text(Message::Active),
                item.entry.path.as_str(),
                display::bytes(file.completed_bytes.get()),
                display::bytes(item.entry.size.get()),
                rate(file.bytes_per_second)
            ));
        }
    }
    for (index, error) in p.failed.iter() {
        if let Some(item) = record.files.get(*index) {
            lines.push(format!(
                "{}: {} | {}",
                text(Message::Failed),
                item.entry.path.as_str(),
                locale::file_failure(language, error)
            ));
        }
    }
    if lines.is_empty() {
        lines.push(text(Message::NoActiveOrFailedFiles).into());
    }
    ui.set_activity(lines.join("\n").into());
}
