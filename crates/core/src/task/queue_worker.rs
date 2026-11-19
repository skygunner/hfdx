use super::*;

impl Worker {
    pub(super) async fn start_prepared(
        &mut self,
        mut prepared: PreparedTask,
    ) -> Result<(), CoreError> {
        if self.task.is_some() {
            return Err(CoreError::InvalidState {
                action: "replace worker",
            });
        }
        self.access_origin = Some(prepared.record.source.request.endpoint.url().origin());
        self.access = std::mem::take(&mut prepared.access);
        let mut opened = TaskStore::adopt(prepared).await?;
        opened.store.set_logger(self.logger.clone());
        self.task = Some(RunningTask {
            store: opened.store,
            cached_record: Arc::new(opened.record.clone()),
            record: opened.record,
            engine: None,
            jobs: BTreeMap::new(),
            slots: BTreeMap::new(),
            pending: VecDeque::new(),
            active: Vec::new(),
            restart_notice: None,
            cached_progress: empty_progress(),
            verification: None,
        });
        self.phase = UiPhase::Paused;
        self.resume(false).await
    }
    pub(super) async fn append_prepared(
        &mut self,
        prepared: PreparedAppend,
    ) -> Result<(), CoreError> {
        let task = self
            .task
            .as_mut()
            .ok_or(CoreError::InvalidState { action: "append" })?;
        if task.record.task_id != prepared.task_id {
            return Err(CoreError::InvalidState {
                action: "append target",
            });
        }
        let previous = task.record.state;
        let mut record = task.record.clone();
        record.schema_version = 2;
        let mut changed = Vec::new();
        for file in prepared.files {
            let index = if let Some(index) = record
                .files
                .iter()
                .position(|f| f.entry.path == file.entry.path)
            {
                if record.files[index].entry != file.entry {
                    return Err(StorageError::CorruptRecord.into());
                }
                if task.jobs.values().any(|i| *i == index)
                    || task.pending.iter().any(|(i, _)| *i == index)
                    || self.verification.as_ref().is_some_and(|v| v.index == index)
                {
                    continue;
                }
                record.files[index] = file;
                index
            } else {
                let index = record.files.len();
                record.files.push(file);
                index
            };
            changed.push(index);
        }
        record.replacements.extend(prepared.replacements);
        task.store.append(&record, prepared.stamps).await?;
        task.record = record;
        task.refresh();
        if previous == TaskState::Paused {
            return Ok(());
        }
        if previous != TaskState::Running {
            return self.resume(false).await;
        }
        let actions = task.store.inspect(&task.record).await?;
        for item in actions {
            if changed.contains(&item.index) && !task.record.files[item.index].completed {
                task.pending.push_back((item.index, item.action));
            }
        }
        if let Some(budget) = &self.budget {
            budget.register(
                task.record.task_id.as_str(),
                task.pending.iter().map(|(i, _)| *i),
            );
        }
        self.phase = UiPhase::Downloading;
        self.fill().await
    }
}
