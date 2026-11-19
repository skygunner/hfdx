use crate::FileRow;
use hfdx_core::{repository::EntryId, selection::SelectionView};
use slint::{Model, ModelNotify, ModelTracker};
use std::{cell::RefCell, rc::Rc};

/// Only visible delegates allocate strings; catalog storage stays shared with core.
pub struct FilesModel {
    view: RefCell<SelectionView>,
    notify: ModelNotify,
}
impl FilesModel {
    pub fn new(view: SelectionView) -> Rc<Self> {
        Rc::new(Self {
            view: RefCell::new(view),
            notify: ModelNotify::default(),
        })
    }
    pub fn update(&self, view: SelectionView) {
        let current = self.view.borrow();
        let changed = !std::sync::Arc::ptr_eq(&current.visible, &view.visible)
            || !std::sync::Arc::ptr_eq(&current.selected, &view.selected)
            || current.catalog.as_ref().map(|c| (c.id, c.len()))
                != view.catalog.as_ref().map(|c| (c.id, c.len()));
        drop(current);
        self.view.replace(view);
        if changed {
            self.notify.reset();
        }
    }
    pub fn id(&self, row: i32) -> Option<EntryId> {
        self.view
            .borrow()
            .visible
            .get(usize::try_from(row).ok()?)
            .copied()
    }
    pub fn details(&self, row: i32) -> Option<String> {
        let view = self.view.borrow();
        let id = view.visible.get(usize::try_from(row).ok()?)?;
        let entry = view.catalog.as_ref()?.entry(*id)?;
        Some(format!(
            "{} | {} B{}",
            entry.path.as_str(),
            entry.size.get(),
            match &entry.content_hash {
                Some(hash) => format!(
                    " | {}: {}",
                    match hash.algorithm {
                        hfdx_core::repository::HashAlgorithm::Sha256 => "SHA-256",
                        hfdx_core::repository::HashAlgorithm::GitSha1 => "Git SHA-1",
                    },
                    hash.hex
                ),
                None => String::new(),
            }
        ))
    }
}
impl Model for FilesModel {
    type Data = FileRow;
    fn row_count(&self) -> usize {
        self.view.borrow().visible.len()
    }
    fn row_data(&self, row: usize) -> Option<FileRow> {
        let view = self.view.borrow();
        let id = view.visible.get(row)?;
        let entry = view.catalog.as_ref()?.entry(*id)?;
        Some(FileRow {
            path: entry.path.as_str().into(),
            size: hfdx_core::display::bytes(entry.size.get()).into(),
            selected: view.selected.contains(id),
        })
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// All frozen task files remain available; strings are allocated only for visible rows.
pub struct TaskFilesModel {
    task: RefCell<hfdx_core::task::TaskView>,
    language: std::cell::Cell<hfdx_core::storage::Language>,
    phase: std::cell::Cell<hfdx_core::task::UiPhase>,
    indices: RefCell<Option<Vec<usize>>>,
    notify: ModelNotify,
}
pub(crate) fn task_state(
    task: &hfdx_core::task::TaskView,
    index: usize,
    phase: hfdx_core::task::UiPhase,
) -> usize {
    let file = &task.record.files[index];
    match file.disposition {
        hfdx_core::storage::FileDisposition::ExistingMatch => return 6,
        hfdx_core::storage::FileDisposition::KeptDifferent
        | hfdx_core::storage::FileDisposition::TimeUnknown => return 7,
        hfdx_core::storage::FileDisposition::Unchecked => return 8,
        _ => {}
    }
    if file.failure.is_some()
        || file.verification == hfdx_core::storage::VerificationStatus::Failed
        || task.progress.failed.iter().any(|(i, _)| *i == index)
    {
        4
    } else if task.verification.is_some_and(|v| v.file_index == index) {
        5
    } else if file.completed {
        3
    } else if phase == hfdx_core::task::UiPhase::Downloading
        && task.progress.active.iter().any(|f| f.index == index)
    {
        1
    } else {
        2
    }
}
// 0 neutral, 1 downloading, 2 verifying, 3 success, 4 caution, 5 failure.
pub(crate) fn phase_tone(task: &hfdx_core::task::TaskView, phase: hfdx_core::task::UiPhase) -> i32 {
    use hfdx_core::{
        storage::{TaskState, VerificationStatus},
        task::UiPhase,
    };
    if task.record.state == TaskState::Failed
        || task
            .record
            .files
            .iter()
            .any(|f| f.failure.is_some() || f.verification == VerificationStatus::Failed)
    {
        return 5;
    }
    match phase {
        UiPhase::Verifying => 2,
        UiPhase::Downloading | UiPhase::Preparing => 1,
        _ if task.record.state == TaskState::Completed => {
            if task.record.verify_after_download == Some(false)
                || task
                    .record
                    .files
                    .iter()
                    .any(|f| f.verification != VerificationStatus::Passed)
            {
                4
            } else {
                3
            }
        }
        _ => 0,
    }
}
pub(crate) fn file_tones(
    state: usize,
    verification: hfdx_core::storage::VerificationStatus,
    disabled: bool,
) -> (i32, i32) {
    use hfdx_core::storage::VerificationStatus::*;
    let status = match state {
        1 => 1,
        5 => 2,
        3 | 6 => 3,
        4 => 5,
        7 | 8 => 4,
        _ => 0,
    };
    let verification = if state == 5 {
        2
    } else if disabled {
        4
    } else {
        match verification {
            Passed => 3,
            Failed => 5,
            Pending => 0,
            Unavailable | LegacyUnverified => 4,
        }
    };
    (status, verification)
}
pub(crate) fn localized(
    language: hfdx_core::storage::Language,
    en: &'static str,
    hans: &'static str,
    hant: &'static str,
) -> &'static str {
    match language {
        hfdx_core::storage::Language::English => en,
        hfdx_core::storage::Language::ZhHans => hans,
        hfdx_core::storage::Language::ZhHant => hant,
    }
}
impl TaskFilesModel {
    pub fn new(
        task: hfdx_core::task::TaskView,
        language: hfdx_core::storage::Language,
        phase: hfdx_core::task::UiPhase,
    ) -> Self {
        Self {
            task: RefCell::new(task),
            language: std::cell::Cell::new(language),
            phase: std::cell::Cell::new(phase),
            indices: RefCell::new(None),
            notify: ModelNotify::default(),
        }
    }
    pub fn update(
        &self,
        task: hfdx_core::task::TaskView,
        language: hfdx_core::storage::Language,
        phase: hfdx_core::task::UiPhase,
        search: &str,
        status: i32,
    ) {
        let search = search.to_lowercase();
        let indices = if search.is_empty() && status == 0 {
            None
        } else {
            Some(
                task.record
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(i, f)| {
                        (status == 0 || task_state(&task, *i, phase) == status as usize)
                            && f.entry.path.as_str().to_lowercase().contains(&search)
                    })
                    .map(|(i, _)| i)
                    .collect(),
            )
        };
        self.task.replace(task);
        self.language.set(language);
        self.phase.set(phase);
        self.indices.replace(indices);
        self.notify.reset();
    }
    pub fn counts(&self) -> String {
        let task = self.task.borrow();
        let language = self.language.get();
        let mut counts = [0usize; 9];
        for index in 0..task.record.files.len() {
            counts[task_state(&task, index, self.phase.get())] += 1;
        }
        format!(
            "{} {} | {} {} | {} {} | {} {} | {} {} | {} {}",
            localized(language, "Total", "总计", "總計"),
            task.record.files.len(),
            localized(language, "Active", "下载中", "下載中"),
            counts[1],
            localized(language, "Waiting", "等待", "等待"),
            counts[2],
            localized(language, "Downloaded", "已下载", "已下載"),
            counts[3],
            localized(language, "Failed", "失败", "失敗"),
            counts[4],
            localized(language, "Verifying", "校验中", "驗證中"),
            counts[5]
        )
    }
    pub fn details(&self, index: usize) -> String {
        let task = self.task.borrow();
        let Some(file) = task.record.files.get(index) else {
            return String::new();
        };
        let error = task
            .progress
            .failed
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, e)| e)
            .or(file.failure.as_ref());
        format!(
            "{}\n{}{}",
            file.entry.path.as_str(),
            hfdx_core::display::bytes(file.entry.size.get()),
            error
                .map(|e| format!(
                    " | {}",
                    hfdx_core::locale::file_failure(self.language.get(), e)
                ))
                .unwrap_or_default()
        )
    }
}
impl Model for TaskFilesModel {
    type Data = crate::TaskRow;
    fn row_count(&self) -> usize {
        self.indices
            .borrow()
            .as_ref()
            .map_or_else(|| self.task.borrow().record.files.len(), Vec::len)
    }
    fn row_data(&self, row: usize) -> Option<Self::Data> {
        let indices = self.indices.borrow();
        let index = indices
            .as_ref()
            .map_or(Some(row), |v| v.get(row).copied())?;
        let task = self.task.borrow();
        let file = task.record.files.get(index)?;
        let language = self.language.get();
        let state = task_state(&task, index, self.phase.get());
        let status = match file.disposition {
            hfdx_core::storage::FileDisposition::ExistingMatch => {
                localized(language, "Already present", "已存在", "已存在")
            }
            hfdx_core::storage::FileDisposition::KeptDifferent => {
                localized(language, "Kept local", "已保留本地", "已保留本機")
            }
            hfdx_core::storage::FileDisposition::TimeUnknown => {
                localized(language, "Time unavailable", "时间无法比较", "時間無法比較")
            }
            hfdx_core::storage::FileDisposition::Unchecked => {
                localized(language, "Not checked", "无法核验", "無法核驗")
            }
            _ => match state {
                1 if self.phase.get() == hfdx_core::task::UiPhase::Downloading => {
                    localized(language, "Downloading", "下载中", "下載中")
                }
                1 => localized(language, "Paused", "已暂停", "已暫停"),
                3 => localized(language, "Downloaded", "已下载", "已下載"),
                4 => localized(language, "Failed", "失败", "失敗"),
                5 => localized(language, "Verifying", "校验中", "驗證中"),
                _ => localized(language, "Waiting", "等待", "等待"),
            },
        };
        use hfdx_core::storage::VerificationStatus::*;
        let verification = if state == 5 {
            localized(language, "Verifying", "校验中", "驗證中")
        } else if task.record.verify_after_download == Some(false) {
            localized(language, "Disabled", "未启用", "未啟用")
        } else {
            match file.verification {
                Passed => localized(language, "Passed", "通过", "通過"),
                Failed => localized(language, "Failed", "失败", "失敗"),
                Pending => localized(language, "Pending", "待校验", "待驗證"),
                Unavailable | LegacyUnverified => {
                    localized(language, "Unverified", "未校验", "未驗證")
                }
            }
        };
        let active = task.progress.active.iter().find(|f| f.index == index);
        let done = if let Some(v) = task.verification.filter(|v| v.file_index == index) {
            v.file_bytes_done
        } else if file.completed {
            file.entry.size.get()
        } else {
            active.map_or(0, |f| f.completed_bytes.get())
        };
        let error = task
            .progress
            .failed
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, e)| e)
            .or(file.failure.as_ref());
        let (status_tone, verification_tone) = file_tones(
            state,
            file.verification,
            task.record.verify_after_download == Some(false),
        );
        Some(crate::TaskRow {
            status_tone,
            verification_tone,
            index: i32::try_from(index).ok()?,
            path: file.entry.path.as_str().into(),
            status: status.into(),
            acquired: if matches!(
                file.disposition,
                hfdx_core::storage::FileDisposition::KeptDifferent
                    | hfdx_core::storage::FileDisposition::TimeUnknown
                    | hfdx_core::storage::FileDisposition::Unchecked
            ) {
                "—".into()
            } else if file.completed || active.is_some() {
                hfdx_core::display::bytes(done).into()
            } else {
                "—".into()
            },
            size: hfdx_core::display::bytes(file.entry.size.get()).into(),
            speed: hfdx_core::display::speed(
                active
                    .filter(|_| self.phase.get() == hfdx_core::task::UiPhase::Downloading)
                    .map(|f| f.bytes_per_second),
            )
            .into(),
            verification: verification.into(),
            error: error
                .map(|e| hfdx_core::locale::file_failure(language, e))
                .unwrap_or_default()
                .into(),
        })
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
