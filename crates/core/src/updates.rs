//! GitHub release discovery, verified executable replacement and anonymous issue drafts.
use crate::{
    storage::Settings,
    task::{AppSnapshot, UiPhase},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    time::{Duration, SystemTime},
};

const REPO: &str = "skygunner/hfdx";
const LIMIT: u64 = 128 * 1024 * 1024;
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Interval {
    Daily,
    #[default]
    Weekly,
    Monthly,
}
impl Interval {
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Self::Daily,
            2 => Self::Monthly,
            _ => Self::Weekly,
        }
    }
    fn duration(self) -> Duration {
        Duration::from_secs(86400 * [1, 7, 30][self.index()])
    }
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}
#[derive(Deserialize, Clone)]
struct Asset {
    name: String,
    digest: Option<String>,
    size: u64,
}
#[derive(Clone)]
struct Upgrade {
    tag: String,
    asset: Asset,
}
enum ResultState {
    Checked(Option<Upgrade>),
    Installed,
}
pub struct Updates {
    name: &'static str,
    marker: Option<PathBuf>,
    last: Option<SystemTime>,
    pending: Option<mpsc::Receiver<Result<ResultState, String>>>,
    available: Option<Upgrade>,
    pub status: String,
    installed: bool,
    pub later: bool,
    pub executable: Option<PathBuf>,
}
impl Updates {
    pub fn new(name: &'static str) -> Self {
        let marker = crate::storage::settings_paths().ok().map(|p| {
            p.settings_file
                .with_file_name(format!("{name}-last-update-check"))
        });
        let last = marker
            .as_ref()
            .and_then(|p| fs::metadata(p).ok()?.modified().ok());
        Self {
            name,
            marker,
            last,
            pending: None,
            available: None,
            status: String::new(),
            installed: false,
            later: false,
            executable: std::env::current_exe().ok(),
        }
    }
    pub fn can_install(&self) -> bool {
        self.available.is_some() && self.pending.is_none() && !self.installed
    }
    pub fn restart_ready(&self, s: &AppSnapshot) -> bool {
        self.installed && !active(s)
    }
    pub fn tick(&mut self, s: &Settings, rt: &tokio::runtime::Handle) {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => return,
                result => {
                    self.pending = None;
                    match result {
                        Ok(Ok(ResultState::Checked(up))) => {
                            self.status = up.as_ref().map_or_else(
                                || "Already up to date / 已是最新版".into(),
                                |u| format!("Update available / 可更新至 {}", u.tag),
                            );
                            self.available = up;
                        }
                        Ok(Ok(ResultState::Installed)) => {
                            self.installed = true;
                            self.status = "Update installed / 已更新，待任务结束后可重启".into();
                        }
                        Ok(Err(e)) => self.status = e,
                        Err(_) => self.status = "Update worker stopped / 更新中断".into(),
                    }
                }
            }
        }
        if s.auto_update_check
            && !self.installed
            && self
                .last
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_none_or(|age| age >= s.update_interval.duration())
        {
            self.check(rt);
        }
    }
    pub fn check(&mut self, rt: &tokio::runtime::Handle) {
        if self.pending.is_some() || self.installed {
            return;
        }
        self.last = Some(SystemTime::now());
        if let Some(path) = &self.marker
            && let Some(parent) = path.parent()
        {
            let _ = fs::create_dir_all(parent).and_then(|_| fs::write(path, b""));
        }
        let name = self.name;
        self.start(rt, "Checking GitHub / 正在检查更新", async move {
            check(name).await.map(ResultState::Checked)
        });
    }
    fn start(
        &mut self,
        rt: &tokio::runtime::Handle,
        msg: &str,
        job: impl Future<Output = Result<ResultState, String>> + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        self.status = msg.into();
        rt.spawn(async move {
            let _ = tx.send(job.await);
        });
    }
    pub fn install(&mut self, rt: &tokio::runtime::Handle) {
        if !self.can_install() {
            return;
        }
        let up = self.available.clone().unwrap();
        self.start(rt, "Downloading update / 正在下载更新", async move {
            install(&up).await.map(|_| ResultState::Installed)
        });
    }
}
fn active(s: &AppSnapshot) -> bool {
    let busy = |p| {
        matches!(
            p,
            UiPhase::Preparing
                | UiPhase::Downloading
                | UiPhase::Verifying
                | UiPhase::Stopping
                | UiPhase::Loading
        )
    };
    busy(s.phase)
        || s.queue.as_ref().is_some_and(|q| {
            q.preparing
                || q.used_slots > 0
                || q.tasks
                    .iter()
                    .any(|t| busy(t.phase) || t.view.record.state == crate::TaskState::Running)
        })
}
fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("hfdx/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())
}
fn asset_name(ui: &str) -> String {
    format!(
        "{ui}-{}-{}{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::EXE_SUFFIX
    )
}
async fn check(name: &str) -> Result<Option<Upgrade>, String> {
    let r: Release = client()?
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    select(r, name)
}
fn select(r: Release, name: &str) -> Result<Option<Upgrade>, String> {
    let version = r
        .tag_name
        .strip_prefix('v')
        .and_then(|v| semver::Version::parse(v).ok())
        .ok_or("Invalid release tag")?;
    if version <= semver::Version::parse(crate::version::VERSION).unwrap() {
        return Ok(None);
    }
    let asset = r
        .assets
        .into_iter()
        .find(|a| a.name == asset_name(name))
        .ok_or("No update binary for this platform / 此平台暂无更新包")?;
    Ok(Some(Upgrade {
        tag: r.tag_name,
        asset,
    }))
}
async fn install(up: &Upgrade) -> Result<(), String> {
    let expected = up
        .asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64)
        .ok_or("Missing SHA-256 digest")?;
    if up.asset.size == 0 || up.asset.size > LIMIT {
        return Err("Invalid asset size".into());
    }
    let url = format!(
        "https://github.com/{REPO}/releases/download/{}/{}",
        up.tag, up.asset.name
    );
    let mut response = client()?
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = dir.path().join(&up.asset.name);
    let mut file = fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut size = 0;
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        size += chunk.len() as u64;
        if size > up.asset.size {
            return Err("Unexpected asset size".into());
        }
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
    }
    if size != up.asset.size || format!("{:x}", hash.finalize()) != expected {
        return Err("Update checksum mismatch / 更新校验失败".into());
    }
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let check = tokio::process::Command::new(&path)
        .arg("--version")
        .kill_on_drop(true)
        .output();
    let result = tokio::time::timeout(Duration::from_secs(15), check)
        .await
        .map_err(|_| "Update executable timed out")?
        .map_err(|e| e.to_string())?;
    if !result.status.success()
        || String::from_utf8_lossy(&result.stdout)
            .split_whitespace()
            .last()
            != up.tag.strip_prefix('v')
    {
        return Err("Incompatible update executable / 更新程序与当前系统不兼容".into());
    }
    self_replace::self_replace(&path).map_err(|e| format!("Install failed / 安装失败: {e}"))
}
pub fn restart(path: &Path) -> std::io::Result<()> {
    Command::new(path).spawn().map(|_| ())
}
fn issue_url(s: &AppSnapshot, ui: &str) -> std::io::Result<url::Url> {
    let mut body = format!(
        "## What happened? / 问题描述\n\n\n## Diagnostics\nhfdx {} | {} | {} {}\n",
        crate::version::VERSION,
        ui,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let mut errors = std::collections::BTreeSet::new();
    for err in s.error.iter().chain(s.queue.iter().flat_map(|q| {
        q.draft_error
            .iter()
            .chain(q.tasks.iter().filter_map(|t| t.error.as_ref()))
    })) {
        use crate::task::ErrorDetail;
        let detail = match &err.detail {
            Some(ErrorDetail::Http { status, .. }) => format!(" HTTP {status}"),
            Some(ErrorDetail::Engine { operation, reason }) => format!(" {operation}: {reason:?}"),
            _ => String::new(),
        };
        errors.insert(format!("{:?}{detail}", err.code));
    }
    for task in s.queue.iter().flat_map(|q| q.tasks.iter()) {
        for (_, failure) in task.view.progress.failed.iter() {
            errors.insert(format!(
                "File {:?}, HTTP {:?}, aria2 {:?}",
                failure.kind, failure.http_status, failure.aria2_code
            ));
        }
    }
    for e in errors.iter().take(8) {
        body.push_str(&format!("- {e}\n"));
    }
    if let Some(dir) = &s.diagnostics.current_log
        && let Ok(mut file) = fs::File::open(dir)
    {
        let start = file.metadata()?.len().saturating_sub(64 * 1024);
        file.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        file.take(64 * 1024).read_to_end(&mut buf)?;
        let counts = log_counts(&buf);
        body.push_str(&format!(
            "Recent log events: RPC failures {}, retries {}, failed tasks {}\n",
            counts[0], counts[1], counts[2]
        ));
    }
    let mut url = url::Url::parse("https://github.com/skygunner/hfdx/issues/new").unwrap();
    url.query_pairs_mut().append_pair("body", &body);
    Ok(url)
}
pub fn issue(s: &AppSnapshot, ui: &str) -> std::io::Result<()> {
    let url = issue_url(s, ui)?;
    let mut child = Command::new(if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    })
    .arg(url.as_str())
    .spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

fn log_counts(buf: &[u8]) -> [u32; 3] {
    let mut counts = [0; 3];
    for line in buf.split(|b| *b == b'\n') {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) {
            match v["event"]["event"].as_str() {
                Some("rpc_failure") => counts[0] += 1,
                Some("retry") => counts[1] += 1,
                Some("task_failed") => counts[2] += 1,
                _ => {}
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(tag: &str, name: &str) -> Release {
        Release {
            tag_name: tag.into(),
            assets: vec![Asset {
                name: name.into(),
                digest: None,
                size: 1,
            }],
        }
    }
    #[tokio::test]
    async fn restart_waits_for_work_and_issue_omits_private_fields() {
        let directory = tempfile::tempdir().unwrap();
        let settings = crate::storage::load_settings(&crate::SettingsPaths {
            settings_file: directory.path().join("settings.json"),
        })
        .await
        .unwrap();
        let mut s = AppSnapshot {
            revision: 1,
            phase: UiPhase::Downloading,
            selection: crate::Selection::new().view(),
            task: None,
            settings: std::sync::Arc::new(settings),
            recent: std::sync::Arc::from([]),
            error: Some(crate::task::UiError {
                code: crate::task::ErrorCode::Network,
                message: "private-model".into(),
                hint: "private-user".into(),
                detail: None,
            }),
            diagnostics: Default::default(),
            queue: None,
        };
        let mut updates = Updates::new("hfdx");
        updates.installed = true;
        assert!(!updates.restart_ready(&s));
        s.phase = UiPhase::Verifying;
        assert!(!updates.restart_ready(&s));
        s.phase = UiPhase::Idle;
        s.queue = Some(std::sync::Arc::new(crate::task::QueueView {
            preparing: true,
            ..Default::default()
        }));
        assert!(!updates.restart_ready(&s));
        s.queue = None;
        assert!(updates.restart_ready(&s));
        let url = issue_url(&s, "GUI").unwrap();
        let body = url.query_pairs().next().unwrap().1;
        assert!(body.contains("Network"));
        assert!(!body.contains("private-"));
    }
    #[test]
    fn only_newer_versions_and_matching_platforms_are_offered() {
        let name = asset_name("hfdx");
        assert!(select(release("v0.1.0", &name), "hfdx").unwrap().is_none());
        assert!(select(release("v0.10.0", &name), "hfdx").unwrap().is_some());
        assert!(select(release("v0.10.0", &name), "hfdx-gui").is_err());
        assert!(select(release("../other", &name), "hfdx").is_err());
    }
    #[test]
    fn log_summary_emits_only_fixed_event_counts() {
        let data=br#"{"event":{"event":"rpc_failure","task_id":"private"},"path":"private","token":"private"}
{"event":{"event":"retry"}}
{"event":{"event":"private"}}
truncated"#;
        assert_eq!(log_counts(data), [1, 1, 0]);
    }
    #[test]
    fn saved_intervals_use_daily_weekly_and_thirty_day_periods() {
        for (i, days) in [1, 7, 30].into_iter().enumerate() {
            let v = Interval::from_index(i);
            assert_eq!(v.index(), i);
            assert_eq!(v.duration(), Duration::from_secs(days * 86400));
        }
    }
}
