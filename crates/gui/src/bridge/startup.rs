use super::*;

impl Bridge {
    pub fn start_with_platform(
        ui: &AppWindow,
        config: CoreConfig,
        platform: Result<
            hfdx_core::notification::NativePlatform,
            hfdx_core::notification::NotificationError,
        >,
    ) -> Result<(Self, std::thread::JoinHandle<Result<(), String>>), String> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let weak = ui.as_weak();
        let worker = std::thread::spawn(move || {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(_) => {
                    if tx
                        .send(Err("Cannot start background runtime".into()))
                        .is_err()
                    {
                        eprintln!("GUI startup receiver closed");
                    }
                    return Err("Cannot start background runtime".into());
                }
            };
            runtime.block_on(async move {
                let services = Arc::new(services::Services::with_platform(&config.paths, platform).await);
                let (completion, notice_rx) = notifications::callback(services.clone());
                let session =
                    match task::spawn_queue_with_services(config, completion.clone(), services.logger.clone())
                        .await
                    {
                        Ok(session) => session,
                        Err(error) => {
                            if tx.send(Err(error.ui_error().message)).is_err() {
                                eprintln!("GUI startup receiver closed");
                            }
                            return Err("Core startup failed".into());
                        }
                    };
                let mut snapshots = session.snapshots.clone();
                let mut notification_worker = tokio::spawn(notifications::receive(
                    notice_rx, snapshots.clone(), services.clone(),
                ));
                let bridge = Self {
                    handle: session.handle.clone(),
                    runtime: tokio::runtime::Handle::current(),
                    mailbox: Arc::new(Mutex::new(Mailbox {
                        snapshot: snapshots.borrow().clone(),
                        error: None,
                        finished: false,
                    })),
                    services: services.clone(),
                    rendered: Arc::new(Mutex::new(diagnostics::DiagnosticRender::default())),
                    pending: Arc::new(AtomicBool::new(false)),
                    ui: weak,
                };
                if tx.send(Ok(bridge.clone())).is_err() && bridge.handle.request_shutdown().is_err()
                {
                    eprintln!("Core already stopped during GUI startup");
                }
                loop {
                    tokio::select! {
                        changed = snapshots.changed() => {
                            if changed.is_err() { break; }
                            match bridge.mailbox.lock() {
                                Ok(mut box_) => box_.snapshot = snapshots.borrow_and_update().clone(),
                                Err(_) => { eprintln!("GUI snapshot mailbox poisoned"); break; }
                            }
                        }
                        () = services.changed.notified() => {}
                    }
                    bridge.refresh();
                }
                let result = session.join().await;
                drop(completion);
                if let Err(error) = notifications::finish(&mut notification_worker).await {
                    services.notification_failed(None, error);
                }
                services.flush();
                let language = bridge
                    .mailbox
                    .lock()
                    .map(|b| b.snapshot.settings.effective_language())
                    .unwrap_or(hfdx_core::storage::Language::English);
                let diagnostics = services.text(language);
                let message = result.as_ref().err().map(|error| hfdx_core::locale::error(language, &error.ui_error()));
                let final_message = [message.clone(), (!diagnostics.is_empty()).then_some(diagnostics)].into_iter().flatten().reduce(|left, right| format!("{left}\n{right}"));
                match bridge.mailbox.lock() {
                    Ok(mut box_) => {
                        box_.finished = true;
                        if let Some(error) = final_message.clone() {
                            box_.error = Some(error);
                        }
                    }
                    Err(_) => eprintln!("GUI shutdown mailbox poisoned"),
                }
                bridge.refresh();
                match (result, final_message) {
                    (Ok(_), None) => Ok(()),
                    (_, Some(message)) => Err(message),
                    (Err(_), None) => Err("Core shutdown failed".into()),
                }
            })
        });
        match rx.recv() {
            Ok(Ok(bridge)) => Ok((bridge, worker)),
            Ok(Err(error)) => {
                if worker.join().is_err() {
                    eprintln!("GUI worker panicked");
                }
                Err(error)
            }
            Err(_) => {
                if worker.join().is_err() {
                    eprintln!("GUI worker panicked");
                }
                Err("Core startup stopped".into())
            }
        }
    }
}
