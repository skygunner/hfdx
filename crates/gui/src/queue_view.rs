use crate::{
    AppWindow, QueueRow,
    files::{TaskFilesModel, localized, task_state},
};
use hfdx_core::{
    storage::Language,
    task::{AppSnapshot, QueueView},
};
use slint::{Model, ModelNotify, ModelTracker};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};
#[derive(PartialEq, Eq)]
enum Node {
    Repository(String, Vec<usize>),
    Batch(usize),
    File(usize, usize),
}
pub(crate) struct QueueModel {
    queue: RefCell<Arc<QueueView>>,
    nodes: RefCell<Vec<Node>>,
    collapsed: RefCell<BTreeSet<String>>,
    models: RefCell<Vec<TaskFilesModel>>,
    language: Cell<Language>,
    search: RefCell<String>,
    status: Cell<i32>,
    notify: ModelNotify,
}
impl QueueModel {
    fn new(queue: Arc<QueueView>, language: Language) -> Self {
        let model = Self {
            queue: RefCell::new(queue),
            nodes: RefCell::new(Vec::new()),
            collapsed: RefCell::new(BTreeSet::new()),
            models: RefCell::new(Vec::new()),
            language: Cell::new(language),
            search: RefCell::new(String::new()),
            status: Cell::new(0),
            notify: ModelNotify::default(),
        };
        model.rebuild();
        model
    }
    fn update(&self, queue: Arc<QueueView>, language: Language, search: &str, status: i32) {
        let search = search.to_lowercase();
        if Arc::ptr_eq(&queue, &self.queue.borrow())
            && language == self.language.get()
            && search == *self.search.borrow()
            && status == self.status.get()
        {
            return;
        }
        self.queue.replace(queue);
        self.language.set(language);
        self.search.replace(search);
        self.status.set(status);
        self.rebuild();
    }
    fn rebuild(&self) {
        let queue = self.queue.borrow();
        let search = self.search.borrow();
        let collapsed = self.collapsed.borrow();
        let mut groups = BTreeMap::<String, Vec<usize>>::new();
        let models = queue
            .tasks
            .iter()
            .map(|task| TaskFilesModel::new(task.view.clone(), self.language.get(), task.phase))
            .collect();
        self.models.replace(models);
        for (i, task) in queue.tasks.iter().enumerate() {
            let r = &task.view.record.source.request;
            let key = format!(
                "{} · {:?} · {}",
                r.repo_id.as_str(),
                r.kind,
                r.endpoint.url()
            );
            groups.entry(key).or_default().push(i);
        }
        let mut nodes = Vec::new();
        for (key, indices) in groups {
            let visible: Vec<_> = indices
                .iter()
                .map(|&i| {
                    let task = &queue.tasks[i];
                    let files = task
                        .view
                        .record
                        .files
                        .iter()
                        .enumerate()
                        .filter(|(j, f)| {
                            (search.is_empty()
                                || f.entry
                                    .path
                                    .as_str()
                                    .to_lowercase()
                                    .contains(search.as_str())
                                || task
                                    .view
                                    .record
                                    .source
                                    .request
                                    .repo_id
                                    .as_str()
                                    .to_lowercase()
                                    .contains(search.as_str()))
                                && (self.status.get() == 0
                                    || task_state(&task.view, *j, task.phase)
                                        == self.status.get() as usize)
                        })
                        .map(|(j, _)| j)
                        .collect::<Vec<_>>();
                    (i, files)
                })
                .filter(|(_, files)| !files.is_empty())
                .collect();
            if visible.is_empty() {
                continue;
            }
            nodes.push(Node::Repository(key.clone(), indices.clone()));
            if collapsed.contains(&key) {
                continue;
            }
            for (i, files) in visible {
                let id = queue.tasks[i].view.record.task_id.as_str();
                if indices.len() > 1 {
                    nodes.push(Node::Batch(i));
                    if collapsed.contains(id) {
                        continue;
                    }
                }
                for j in files {
                    nodes.push(Node::File(i, j));
                }
            }
        }
        // Preserve the focused row when branches expand/collapse or progress changes.
        let old = self.nodes.replace(nodes);
        let nodes = self.nodes.borrow();
        let prefix = old
            .iter()
            .zip(nodes.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(nodes[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let removed = old.len() - prefix - suffix;
        let added = nodes.len() - prefix - suffix;
        if removed == 0 && added > 0 {
            self.notify.row_added(prefix, added);
        } else if added == 0 && removed > 0 {
            self.notify.row_removed(prefix, removed);
        } else if added > 0 || removed > 0 {
            self.notify.reset();
            return;
        }
        for i in 0..nodes.len() {
            self.notify.row_changed(i);
        }
    }
    pub(crate) fn inspect(&self, index: usize) -> Option<(hfdx_core::TaskId, String)> {
        let queue = self.queue.borrow();
        let nodes = self.nodes.borrow();
        match nodes.get(index)? {
            Node::Repository(key, ids) => {
                let task = &queue.tasks[*ids.first()?];
                let result = (
                    task.view.record.task_id.clone(),
                    format!("{}\n{}", key, task.view.destination.display()),
                );
                let key = key.clone();
                drop(nodes);
                drop(queue);
                let mut collapsed = self.collapsed.borrow_mut();
                if !collapsed.remove(&key) {
                    collapsed.insert(key);
                }
                drop(collapsed);
                self.rebuild();
                Some(result)
            }
            Node::Batch(i) => {
                let task = &queue.tasks[*i];
                let id = task.view.record.task_id.clone();
                let details = format!(
                    "{}\n{}",
                    task.view.destination.display(),
                    task.view.record.source.resolved_commit.as_str()
                );
                let key = id.as_str().to_owned();
                drop(nodes);
                drop(queue);
                let mut collapsed = self.collapsed.borrow_mut();
                if !collapsed.remove(&key) {
                    collapsed.insert(key);
                }
                drop(collapsed);
                self.rebuild();
                Some((id, details))
            }
            Node::File(i, j) => {
                let task = &queue.tasks[*i];
                Some((
                    task.view.record.task_id.clone(),
                    format!(
                        "{}\n{}",
                        task.view
                            .destination
                            .join(task.view.record.files[*j].entry.path.as_str())
                            .display(),
                        self.models.borrow()[*i].details(*j)
                    ),
                ))
            }
        }
    }
}
impl Model for QueueModel {
    type Data = QueueRow;
    fn row_count(&self) -> usize {
        self.nodes.borrow().len()
    }
    fn row_data(&self, index: usize) -> Option<QueueRow> {
        let queue = self.queue.borrow();
        let nodes = self.nodes.borrow();
        let language = self.language.get();
        let mut row = QueueRow {
            index: index.try_into().ok()?,
            ..Default::default()
        };
        match nodes.get(index)? {
            Node::Repository(key, ids) => {
                row.kind = 1;
                row.expanded = !self.collapsed.borrow().contains(key);
                row.label = queue.tasks[*ids.first()?]
                    .view
                    .record
                    .source
                    .request
                    .repo_id
                    .as_str()
                    .into();
                let done = ids.iter().fold(0u64, |sum, i| {
                    sum.saturating_add(queue.tasks[*i].view.progress.acquired_bytes.get())
                });
                let total = ids.iter().fold(0u64, |sum, i| {
                    sum.saturating_add(queue.tasks[*i].view.progress.total_bytes.get())
                });
                let speed = ids.iter().fold(0u64, |sum, i| {
                    sum.saturating_add(queue.tasks[*i].view.progress.bytes_per_second)
                });
                row.progress = if ids
                    .iter()
                    .all(|i| queue.tasks[*i].view.record.state == hfdx_core::TaskState::Completed)
                {
                    hfdx_core::display::bytes(total)
                } else {
                    format!(
                        "{} / {} · {}",
                        hfdx_core::display::bytes(done),
                        hfdx_core::display::bytes(total),
                        hfdx_core::display::speed(Some(speed))
                    )
                }
                .into();
                row.status = format!(
                    "{} {}",
                    ids.iter()
                        .map(|i| queue.tasks[*i].view.record.files.len())
                        .sum::<usize>(),
                    localized(language, "files", "个文件", "個檔案")
                )
                .into();
            }
            Node::Batch(i) => {
                let task = &queue.tasks[*i];
                row.kind = 2;
                row.expanded = !self
                    .collapsed
                    .borrow()
                    .contains(task.view.record.task_id.as_str());
                row.label = format!(
                    "{} @ {}",
                    task.view.destination.display(),
                    &task.view.record.source.resolved_commit.as_str()[..8]
                )
                .into();
                row.status = hfdx_core::locale::state(language, task.view.record.state).into();
                row.status_tone = crate::files::phase_tone(&task.view, task.phase);
            }
            Node::File(i, j) => {
                let file = self.models.borrow()[*i].row_data(*j)?;
                row.label = file.path;
                row.status = file.status;
                row.status_tone = file.status_tone;
                row.verification_tone = file.verification_tone;
                let entry = &queue.tasks[*i].view.record.files[*j];
                row.progress = if entry.completed
                    && matches!(
                        entry.disposition,
                        hfdx_core::storage::FileDisposition::Download
                            | hfdx_core::storage::FileDisposition::ExistingMatch
                    ) {
                    file.size
                } else {
                    format!("{} / {} · {}", file.acquired, file.size, file.speed).into()
                };
                row.verification = file.verification;
            }
        }
        Some(row)
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
pub(crate) fn apply(ui: &AppWindow, snapshot: &AppSnapshot) {
    let Some(queue) = &snapshot.queue else {
        ui.set_queue_mode(false);
        return;
    };
    ui.set_queue_mode(true);
    let language = crate::input::language(ui);
    let text = |en, hans, hant| localized(language, en, hans, hant);
    let append = snapshot.selection.catalog.as_ref().and_then(|catalog| {
        queue.tasks.iter().find(|task| {
            let a = &task.view.record.source;
            let b = &catalog.source;
            a.request.endpoint == b.request.endpoint
                && a.request.kind == b.request.kind
                && a.request.repo_id == b.request.repo_id
                && a.resolved_commit == b.resolved_commit
                && task.view.destination == std::path::Path::new(ui.get_destination().as_str())
        })
    });
    ui.set_queue_append_policy(
        append
            .map(|task| {
                format!(
                    "{} · {} {} · {}: {} · {} {}",
                    text(
                        "Append uses existing settings",
                        "追加沿用原任务设置",
                        "追加沿用原任務設定"
                    ),
                    task.view.record.options.connections_per_file(),
                    text("connections", "个连接", "個連線"),
                    text("Verification", "校验", "校驗"),
                    if task.view.record.verify_after_download == Some(false) {
                        text("off", "关闭", "關閉")
                    } else {
                        text("on", "开启", "開啟")
                    },
                    queue.slot_limit,
                    text("global slots", "个全局名额", "個全域名額")
                )
            })
            .unwrap_or_default()
            .into(),
    );
    let speed = queue
        .tasks
        .iter()
        .map(|t| t.view.progress.bytes_per_second)
        .sum();
    ui.set_queue_summary(
        format!(
            "{} {}/{} · {} · {} {}",
            text("Slots", "全局名额", "全域名額"),
            queue.used_slots,
            queue.slot_limit,
            hfdx_core::display::speed(Some(speed)),
            queue.tasks.len(),
            text("download items", "个下载项", "個下載項")
        )
        .into(),
    );
    ui.set_queue_note(
        queue
            .last_submission
            .as_ref()
            .map(|s| {
                format!(
                    "{} {} · {} {} · {} {} · {} {} · {} {}",
                    text("Added", "新增", "新增"),
                    s.added,
                    text("Identical, skipped", "内容一致已跳过", "內容一致已略過"),
                    s.identical,
                    text("Already queued", "已在队列", "已在佇列"),
                    s.already_queued,
                    text("Kept local", "保留本地", "保留本機"),
                    s.kept,
                    text("Could not check", "无法核验", "無法核驗"),
                    s.unchecked
                )
            })
            .unwrap_or_default()
            .into(),
    );
    if let Some(task) = &snapshot.task
        && task.record.state == hfdx_core::TaskState::Completed
        && task.record.files.iter().any(|f| {
            matches!(
                f.disposition,
                hfdx_core::storage::FileDisposition::Unchecked
                    | hfdx_core::storage::FileDisposition::KeptDifferent
                    | hfdx_core::storage::FileDisposition::TimeUnknown
            )
        })
    {
        ui.set_status(
            text(
                "Finished; some local files were kept without a matching digest",
                "处理完成；部分本地文件已保留，不代表与远端内容一致",
                "處理完成；部分本機檔案已保留，不代表與遠端內容一致",
            )
            .into(),
        );
    }
    ui.set_queue_preparing(queue.preparing);
    ui.set_queue_cancellable(queue.cancellable);
    ui.set_queue_preparation(
        if let Some(path) = &queue.checking_path {
            format!(
                "{}: {} · {} / {}",
                text(
                    "Checking existing file",
                    "正在检查已有文件",
                    "正在檢查既有檔案"
                ),
                path,
                hfdx_core::display::bytes(queue.checking_bytes),
                hfdx_core::display::bytes(queue.checking_total)
            )
        } else {
            text(
                "Checking files and adding to the queue…",
                "正在检查文件并加入队列…",
                "正在檢查檔案並加入佇列…",
            )
            .into()
        }
        .into(),
    );
    let model = ui.get_queue_rows();
    let model = if model.as_any().is::<QueueModel>() {
        model
    } else {
        slint::ModelRc::from(Rc::new(QueueModel::new(queue.clone(), language)))
    };
    if let Some(model) = model.as_any().downcast_ref::<QueueModel>() {
        model.update(
            queue.clone(),
            language,
            &ui.get_task_filter(),
            ui.get_task_status_filter(),
        );
    }
    ui.set_queue_rows(model);
    if let Some(plan) = &queue.conflicts {
        let key = plan.id.to_string();
        if ui.get_conflict_key().as_str() != key {
            ui.set_conflict_policy(-1);
            ui.set_conflict_key(key.into());
        }
        ui.set_conflict_open(true);
        ui.set_starting(false);
        ui.set_tasks_open(false);
        ui.set_conflict_summary(
            format!(
                "{} {} · {} {} · {} {}",
                plan.new_files,
                text("new", "个新文件", "個新檔案"),
                plan.identical,
                text("identical, skipped", "个内容一致，跳过", "個內容一致，略過"),
                plan.files.len(),
                text(
                    "different, choose a policy",
                    "个内容不同，请选择处理方式",
                    "個內容不同，請選擇處理方式"
                )
            )
            .into(),
        );
        ui.set_conflict_rows(slint::ModelRc::new(slint::VecModel::from(
            plan.files
                .iter()
                .map(|f| crate::ConflictRow {
                    path: f.entry.path.as_str().into(),
                    local: format!(
                        "{} · {}",
                        hfdx_core::display::bytes(f.local_size),
                        f.local_hash.hex
                    )
                    .into(),
                    remote: format!(
                        "{} · {}",
                        hfdx_core::display::bytes(f.entry.size.get()),
                        f.entry
                            .content_hash
                            .as_ref()
                            .map_or("—", |h| h.hex.as_str())
                    )
                    .into(),
                })
                .collect::<Vec<_>>(),
        )));
    } else {
        ui.set_conflict_open(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hfdx_core::{
        Bytes,
        storage::TaskRecord,
        task::{QueuedTask, TaskProgress, TaskView, UiPhase},
    };
    #[test]
    #[ignore = "explicit 10k/100k projection benchmark; no transfers"]
    fn queue_tree_projection_benchmark() {
        use std::time::Instant;
        let mut csv = String::from("files,operation,sample,elapsed_ms,rows\n");
        for count in [10_000, 100_000] {
            let mut record: TaskRecord = serde_json::from_value(serde_json::json!({
                "schema_version": 2, "task_id": "a".repeat(32),
                "source": {"request":{"endpoint":{"url":"https://huggingface.co","allow_http":false}, "kind":"model", "repo_id":"fixture/model", "requested_revision":"main"}, "resolved_commit":"a".repeat(40)},
                "files":[], "options":{"connections_per_file":1,"concurrent_files":2},
                "proxy_address":null,"state":"running","created_unix_ms":1,"updated_unix_ms":1
            })).unwrap();
            record.files = (0..count).map(|i| serde_json::from_value(serde_json::json!({
                "entry":{"path":format!("path-{i:06}.gguf"),"size":1024},"completed":false,"failure":null
            })).unwrap()).collect();
            let queue = Arc::new(QueueView {
                tasks: Arc::from([QueuedTask {
                    view: TaskView {
                        record: Arc::new(record),
                        destination: "/fixture".into(),
                        progress: TaskProgress {
                            completed_files: 0,
                            total_files: count,
                            acquired_bytes: Bytes::new(0),
                            total_bytes: Bytes::new(count as u64 * 1024),
                            bytes_per_second: 0,
                            eta: None,
                            active: Arc::from([]),
                            failed: Arc::from([]),
                        },
                        verification: None,
                        restart_notice: None,
                    },
                    phase: UiPhase::Downloading,
                    error: None,
                }]),
                ..Default::default()
            });
            let model = QueueModel::new(queue.clone(), Language::English);
            assert_eq!(
                model.row_count(),
                count + 1,
                "restored tasks must be visible immediately"
            );
            for sample in 0..30 {
                let start = Instant::now();
                let search = if sample % 2 == 0 { "path-000" } else { "" };
                model.update(queue.clone(), Language::English, search, 0);
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(
                    model.row_count(),
                    if sample % 2 == 0 { 1001 } else { count + 1 }
                );
                assert!(model.row_data(0).is_some());
                csv.push_str(&format!(
                    "{count},filter,{sample},{elapsed:.3},{}\n",
                    model.row_count()
                ));
            }
            let start = Instant::now();
            model.inspect(0).unwrap();
            assert_eq!(model.row_count(), 1);
            csv.push_str(&format!(
                "{count},collapse,0,{:.3},1\n",
                start.elapsed().as_secs_f64() * 1000.0
            ));
        }
        if let Some(path) = std::env::var_os("HFDX_QUEUE_BENCHMARK") {
            std::fs::write(path, &csv).unwrap();
        }
        println!("{csv}");
    }
}
