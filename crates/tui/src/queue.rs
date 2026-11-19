use super::*;
use hfdx_core::{
    storage::{FileDisposition, Theme},
    task::{TaskView, UiPhase},
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Cell, Row, Table},
};

fn color(theme: Theme, tone: u8) -> Style {
    let value = match (theme, tone) {
        (_, 0) => return Style::default(),
        (Theme::Light, 1) => Color::Rgb(23, 93, 182),
        (Theme::Light, 2) => Color::Rgb(117, 64, 181),
        (Theme::Light, 3) => Color::Rgb(25, 115, 62),
        (Theme::Light, 4) => Color::Rgb(137, 89, 0),
        (Theme::Light, _) => Color::Rgb(181, 46, 55),
        (_, 1) => Color::Blue,
        (_, 2) => Color::Magenta,
        (_, 3) => Color::Green,
        (_, 4) => Color::Yellow,
        _ => Color::Red,
    };
    Style::default().fg(value)
}

fn file_row(
    task: &TaskView,
    index: usize,
    phase: UiPhase,
    language: hfdx_core::Language,
    theme: Theme,
    compact: bool,
) -> Row<'static> {
    use VerificationStatus::*;
    let file = &task.record.files[index];
    let active = task.progress.active.iter().find(|f| f.index == index);
    let verification = task.verification.filter(|v| v.file_index == index);
    let (status, tone) = match file.disposition {
        FileDisposition::ExistingMatch => {
            (text(language, "Already present", "已存在", "已存在"), 3)
        }
        FileDisposition::KeptDifferent => (text(language, "Kept local", "保留本地", "保留本機"), 4),
        FileDisposition::TimeUnknown => (text(language, "Time unknown", "时间不明", "時間不明"), 4),
        FileDisposition::Unchecked => (text(language, "Not checked", "无法核验", "無法核驗"), 4),
        _ if file.failure.is_some()
            || file.verification == Failed
            || task.progress.failed.iter().any(|(i, _)| *i == index) =>
        {
            (text(language, "Failed", "失败", "失敗"), 5)
        }
        _ if verification.is_some() => (text(language, "Verifying", "校验中", "驗證中"), 2),
        _ if file.completed => (text(language, "Downloaded", "已下载", "已下載"), 3),
        _ if active.is_some() && phase == UiPhase::Downloading => {
            (text(language, "Downloading", "下载中", "下載中"), 1)
        }
        _ => (text(language, "Waiting", "等待", "等待"), 0),
    };
    let (result, result_tone) = if verification.is_some() {
        (text(language, "Verifying", "校验中", "驗證中"), 2)
    } else if task.record.verify_after_download == Some(false) {
        (text(language, "Disabled", "未启用", "未啟用"), 4)
    } else {
        match file.verification {
            Passed => (text(language, "Passed", "通过", "通過"), 3),
            Failed => (text(language, "Failed", "失败", "失敗"), 5),
            Pending => (text(language, "Pending", "待校验", "待驗證"), 0),
            Unavailable | LegacyUnverified => (text(language, "Unverified", "未校验", "未驗證"), 4),
        }
    };
    let total = bytes(file.entry.size.get());
    let mut height = 1;
    let progress = if let Some(v) = verification {
        format!("{} / {total}", bytes(v.file_bytes_done))
    } else if file.completed
        || matches!(
            file.disposition,
            FileDisposition::KeptDifferent
                | FileDisposition::TimeUnknown
                | FileDisposition::Unchecked
        )
    {
        total
    } else if let Some(active) = active.filter(|_| phase == UiPhase::Downloading) {
        if compact {
            height = 2;
        }
        format!(
            "{} / {total}{}{}",
            bytes(active.completed_bytes.get()),
            if compact { "\n" } else { " · " },
            speed(Some(active.bytes_per_second))
        )
    } else {
        total
    };
    let error = task
        .progress
        .failed
        .iter()
        .find(|(i, _)| *i == index)
        .map(|(_, e)| e)
        .or(file.failure.as_ref());
    let filename = if let Some(error) = error {
        height = height.max(2);
        format!(
            "    {}\n    {}",
            file.entry.path.as_str(),
            locale::file_failure(language, error)
        )
    } else {
        format!("    {}", file.entry.path.as_str())
    };
    Row::new([
        Cell::from(filename),
        Cell::from(status).style(color(theme, tone)),
        Cell::from(progress),
        Cell::from(result).style(color(theme, result_tone)),
    ])
    .height(height)
}

pub(super) fn text(
    language: hfdx_core::Language,
    en: &'static str,
    cn: &'static str,
    tw: &'static str,
) -> &'static str {
    match language {
        hfdx_core::Language::English => en,
        hfdx_core::Language::ZhHans => cn,
        hfdx_core::Language::ZhHant => tw,
    }
}
pub(super) fn render(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    snapshot: &AppSnapshot,
) -> bool {
    let Some(queue) = &snapshot.queue else {
        return false;
    };
    let language = snapshot.settings.effective_language();
    if let Some(plan) = &queue.conflicts {
        let mut lines = vec![
            format!(
                "{}: {}",
                text(
                    language,
                    "Different content",
                    "同名文件内容不同",
                    "同名檔案內容不同"
                ),
                plan.destination.display()
            ),
            format!(
                "{} new · {} identical · {} conflicts",
                plan.new_files,
                plan.identical,
                plan.files.len()
            ),
        ];
        lines.extend(
            plan.files
                .iter()
                .skip(app.queue_scroll as usize)
                .take(area.height.saturating_sub(8) as usize)
                .map(|f| {
                    format!(
                        "{}\n  local {} · {}\n  remote {} · {}",
                        f.entry.path.as_str(),
                        bytes(f.local_size),
                        f.local_hash.hex,
                        bytes(f.entry.size.get()),
                        f.entry
                            .content_hash
                            .as_ref()
                            .map_or("—", |h| h.hex.as_str())
                    )
                }),
        );
        frame.render_widget(
            Paragraph::new(lines.join("\n"))
                .block(Block::bordered().title(text(
                    language,
                    "File conflicts",
                    "文件冲突",
                    "檔案衝突",
                )))
                .wrap(Wrap { trim: false }),
            area,
        );
        return true;
    }
    if queue.preparing && !app.queue_open {
        let label = text(
            language,
            "Checking existing files / adding to queue",
            "检查已有文件 / 加入队列",
            "檢查既有檔案 / 加入佇列",
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{label}\n{}\n{} / {}",
                queue.checking_path.as_deref().unwrap_or("…"),
                bytes(queue.checking_bytes),
                bytes(queue.checking_total),
            ))
            .block(Block::bordered())
            .wrap(Wrap { trim: false }),
            area,
        );
        return true;
    }
    if !app.queue_open {
        return false;
    }
    let mut title = format!(
        "{} · slots {}/{} · {}",
        text(language, "Download queue", "下载队列", "下載佇列"),
        queue.used_slots,
        queue.slot_limit,
        speed(Some(
            queue
                .tasks
                .iter()
                .map(|t| t.view.progress.bytes_per_second)
                .sum()
        ))
    );
    if queue.preparing {
        title.push_str(&format!(
            "\n{}: {} / {} · {}",
            text(language, "Checking files", "检查文件", "檢查檔案"),
            bytes(queue.checking_bytes),
            bytes(queue.checking_total),
            queue.checking_path.as_deref().unwrap_or("…")
        ));
    }
    let rows = Layout::vertical([
        Constraint::Length(if queue.preparing { 4 } else { 3 }),
        Constraint::Min(1),
        Constraint::Length(4),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(title).block(Block::bordered()), rows[0]);
    let compact = area.width < 110;
    let theme = snapshot.settings.theme;
    let lines = queue
        .tasks
        .iter()
        .enumerate()
        .flat_map(|(i, task)| {
            let id = task.view.record.task_id.as_str();
            let closed = app.queue_collapsed.contains(id);
            let heading = Row::new([
                Cell::from(format!(
                    "{} {} {}",
                    if app.queue_cursor == i { ">" } else { " " },
                    if closed { "▸" } else { "▾" },
                    task.view.record.source.request.repo_id.as_str()
                )),
                Cell::from(
                    if matches!(
                        task.view.record.state,
                        TaskState::Completed | TaskState::Failed
                    ) {
                        locale::state(language, task.view.record.state)
                    } else {
                        locale::phase(language, task.phase)
                    },
                )
                .style(color(
                    theme,
                    match task.phase {
                        _ if task.view.record.state == TaskState::Failed => 5,
                        UiPhase::Downloading | UiPhase::Preparing => 1,
                        UiPhase::Verifying => 2,
                        _ if task.view.record.state == TaskState::Completed => {
                            if task.view.record.verify_after_download != Some(false)
                                && task
                                    .view
                                    .record
                                    .files
                                    .iter()
                                    .all(|f| f.verification == VerificationStatus::Passed)
                            {
                                3
                            } else {
                                4
                            }
                        }
                        _ => 0,
                    },
                )),
                Cell::from(format!(
                    "{} / {}",
                    bytes(task.view.progress.acquired_bytes.get()),
                    bytes(task.view.progress.total_bytes.get())
                )),
                Cell::from(""),
            ])
            .style(Style::default().add_modifier(Modifier::BOLD));
            std::iter::once(heading).chain(
                task.view
                    .record
                    .files
                    .iter()
                    .enumerate()
                    .filter(move |_| !closed)
                    .map(move |(index, _)| {
                        file_row(&task.view, index, task.phase, language, theme, compact)
                    }),
            )
        })
        .skip(app.queue_scroll as usize)
        .take(rows[1].height.saturating_sub(3) as usize);
    let header = Row::new([
        text(language, "File", "文件", "檔案"),
        text(language, "Status", "状态", "狀態"),
        text(language, "Progress / speed", "进度 / 速度", "進度 / 速度"),
        text(language, "Verification", "校验结果", "驗證結果"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(
        Table::new(
            lines,
            [
                Constraint::Min(16),
                Constraint::Length(12),
                Constraint::Length(if compact { 26 } else { 42 }),
                Constraint::Length(12),
            ],
        )
        .header(header)
        .column_spacing(1)
        .block(Block::bordered().title(text(language, "Repositories", "仓库", "倉庫"))),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n{}",
            queue
                .tasks
                .get(app.queue_cursor)
                .map_or_else(String::new, |t| format!(
                    "{} @ {}",
                    t.view.destination.display(),
                    &t.view.record.source.resolved_commit.as_str()[..8]
                )),
            if let Some(error) = app.message_error.as_ref().or(queue.draft_error.as_ref()) {
                locale::error(language, error)
            } else {
                queue
                    .tasks
                    .get(app.queue_cursor)
                    .and_then(|t| t.view.progress.failed.first())
                    .map(|(_, error)| locale::file_failure(language, error))
                    .unwrap_or_else(|| app.message.clone())
            }
        ))
        .wrap(Wrap { trim: false }),
        rows[2],
    );
    true
}
pub(super) fn key(
    app: &mut App,
    session: &CoreSession,
    snapshot: &AppSnapshot,
    key: crossterm::event::KeyEvent,
) -> bool {
    let Some(queue) = &snapshot.queue else {
        return false;
    };
    if let Some(plan) = &queue.conflicts {
        let policy = match key.code {
            KeyCode::Char('1') => Some(hfdx_core::task::ConflictPolicy::Overwrite),
            KeyCode::Char('2') => Some(hfdx_core::task::ConflictPolicy::NewerOnly),
            KeyCode::Char('3') => Some(hfdx_core::task::ConflictPolicy::Archive),
            KeyCode::Esc => {
                app.submit(session, CoreCommand::CancelSubmission);
                return true;
            }
            KeyCode::PageDown => {
                app.queue_scroll = app.queue_scroll.saturating_add(1);
                return true;
            }
            KeyCode::PageUp => {
                app.queue_scroll = app.queue_scroll.saturating_sub(1);
                return true;
            }
            _ => None,
        };
        if let Some(policy) = policy {
            app.submit(
                session,
                CoreCommand::ConfirmConflicts {
                    plan: plan.id,
                    policy,
                },
            );
        }
        return true;
    }
    if key.code == KeyCode::Char('j')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && (app.queue_open || app.focus != 8)
    {
        app.queue_open = !app.queue_open;
        return true;
    }
    if queue.preparing && !app.queue_open {
        if key.code == KeyCode::Esc && queue.cancellable {
            app.submit(session, CoreCommand::CancelSubmission);
        }
        return true;
    }
    if !app.queue_open {
        return false;
    }
    let task = queue.tasks.get(app.queue_cursor);
    match key.code {
        KeyCode::Esc => app.queue_open = false,
        KeyCode::Char('c') if queue.cancellable => {
            app.submit(session, CoreCommand::CancelSubmission)
        }
        KeyCode::Down => {
            app.queue_cursor = (app.queue_cursor + 1).min(queue.tasks.len().saturating_sub(1))
        }
        KeyCode::Up => app.queue_cursor = app.queue_cursor.saturating_sub(1),
        KeyCode::PageDown => app.queue_scroll = app.queue_scroll.saturating_add(10),
        KeyCode::PageUp => app.queue_scroll = app.queue_scroll.saturating_sub(10),
        KeyCode::Char('P') => app.queue_control(session, CoreCommand::PauseAll),
        KeyCode::Char('R') => app.queue_control(session, CoreCommand::ResumeAll),
        KeyCode::Char('p') => {
            if let Some(task) = task {
                app.queue_control(
                    session,
                    CoreCommand::PauseTask(task.view.record.task_id.clone()),
                );
            }
        }
        KeyCode::Char('r') => {
            if let Some(task) = task {
                app.queue_control(
                    session,
                    CoreCommand::ResumeTask(task.view.record.task_id.clone()),
                );
            }
        }
        KeyCode::Char(' ') => {
            if let Some(task) = task {
                let id = task.view.record.task_id.as_str().to_owned();
                if !app.queue_collapsed.remove(&id) {
                    app.queue_collapsed.insert(id);
                }
            }
        }
        KeyCode::Enter => {
            if let Some(task) = task {
                app.submit(
                    session,
                    CoreCommand::SelectTask(task.view.record.task_id.clone()),
                );
            }
        }
        KeyCode::Char('a') => {
            if let Some(task) = task {
                app.opened(&task.view.record.source.request);
                app.fields[1] = task.view.record.source.resolved_commit.as_str().into();
                app.fields[5] = task.view.destination.to_string_lossy().into();
                app.submit(
                    session,
                    CoreCommand::LoadActive(task.view.record.task_id.clone()),
                );
                app.queue_open = false;
            }
        }
        _ => {}
    }
    true
}
