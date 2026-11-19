use hfdx_core::{
    CatalogState, DownloadOptions, Endpoint, NetworkAccess, RepoId, RepoKind, RepositoryRequest,
    Revision, SelectionChange, SettingsPaths, TaskState,
    task::{self, CoreCommand, CoreConfig, CoreHandle, LoadRequest, StartRequest},
};
use serde_json::json;
use std::{
    error::Error,
    io::{self, BufRead},
    path::PathBuf,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

async fn command(handle: &CoreHandle, value: CoreCommand) -> Result<()> {
    handle
        .submit(value)
        .map_err(|e| format!("command rejected: {e:?}"))?
        .await??;
    Ok(())
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 7 || !matches!(args[1].as_str(), "fresh" | "open") {
        return Err(
            "usage: task_driver fresh|open ENDPOINT REPO FILTER DESTINATION SETTINGS".into(),
        );
    }
    let destination = PathBuf::from(&args[5]);
    let mut session = task::spawn(CoreConfig {
        paths: SettingsPaths {
            settings_file: PathBuf::from(&args[6]),
        },
        aria2_executable: "aria2c".into(),
    })
    .await?;
    let handle = session.handle.clone();
    let setup = async {
        if args[1] == "fresh" {
            let repository = RepositoryRequest {
                endpoint: Endpoint::parse(&args[2], true)?,
                kind: RepoKind::Model,
                repo_id: RepoId::parse(&args[3])?,
                requested_revision: Revision::parse("main")?,
            };
            command(
                &handle,
                CoreCommand::Load(LoadRequest {
                    repository,
                    access: NetworkAccess::default(),
                }),
            )
            .await?;
            session
                .snapshots
                .wait_for(|s| s.selection.state == CatalogState::Ready || s.error.is_some())
                .await?;
            let catalog = {
                let snapshot = session.snapshots.borrow();
                if snapshot.error.is_some() {
                    return Err("catalog load failed".into());
                }
                snapshot
                    .selection
                    .catalog
                    .as_ref()
                    .ok_or("missing catalog")?
                    .id
            };
            command(&handle, CoreCommand::SetFilter(args[4].clone())).await?;
            command(
                &handle,
                CoreCommand::ChangeSelection(SelectionChange::SelectMatches),
            )
            .await?;
            let verify_after_download = session.snapshots.borrow().settings.verify_after_download;
            command(
                &handle,
                CoreCommand::Start(StartRequest {
                    catalog,
                    destination: destination.clone(),
                    options: DownloadOptions::new(4, 5)?,
                    verify_after_download,
                }),
            )
            .await?;
        } else {
            command(
                &handle,
                CoreCommand::OpenTask(destination.join(".hfdx/task.json")),
            )
            .await?;
        }
        Result::<()>::Ok(())
    }
    .await;
    let mut failure = setup.err();
    let (input_tx, mut input) = tokio::sync::mpsc::channel(4);
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if input_tx.blocking_send(line).is_err() {
                break;
            }
        }
    });
    while failure.is_none() {
        let (phase, completed, total, bytes, state, has_error) = {
            let s = session.snapshots.borrow_and_update();
            let t = s.task.as_ref();
            (
                format!("{:?}", s.phase),
                t.map_or(0, |t| t.progress.completed_files),
                t.map_or(0, |t| t.progress.total_files),
                t.map_or(0, |t| t.progress.acquired_bytes.get()),
                t.map(|t| t.record.state),
                s.error.is_some(),
            )
        };
        println!(
            "{}",
            json!({"event":"snapshot","phase":phase,"completed_files":completed,"total_files":total,"acquired_bytes":bytes,"state":state.map(|s| format!("{s:?}")),"error":has_error})
        );
        if has_error {
            failure = Some("core reported an error".into());
            break;
        }
        if state == Some(TaskState::Completed) {
            break;
        }
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal?; break; }
            changed = session.snapshots.changed() => { changed?; }
            line = input.recv(), if !input.is_closed() => match line.transpose()? {
                Some(line) if line == "resume" => {
                    if let Err(error) = command(&handle, CoreCommand::Resume).await { failure = Some(error); }
                    else { println!("{}", json!({"event":"resume_accepted"})); }
                }
                Some(_) => { failure = Some("unknown input command".into()); }
                None => {},
            }
        }
    }
    handle
        .request_shutdown()
        .map_err(|e| format!("shutdown rejected: {e:?}"))?;
    let receipt = session.join().await?;
    println!(
        "{}",
        json!({"event":"exit","stop":format!("{:?}", receipt.stop),"child_pid":receipt.child_pid,"child_exit_code":receipt.child_exit_code,"ok":failure.is_none()})
    );
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{}", json!({"event":"error","message":error.to_string()}));
        std::process::exit(1);
    }
}
