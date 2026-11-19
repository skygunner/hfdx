#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use hfdx_core::{
    DownloadOptions, Endpoint, NetworkAccess, ProxySpec, RepoId, RepoKind, RepositoryRequest,
    Revision, SelectionChange, Token,
    display::{bytes, speed},
    locale::{self, Message},
    parse_repository,
    storage::{TaskState, VerificationStatus},
    task::{
        AppSnapshot, CommandReceipt, CoreCommand, CoreError, CoreSession, LoadRequest, StartRequest,
    },
};
use ratatui::{
    Terminal,
    backend::Backend,
    layout::{Constraint, Layout},
    widgets::{Block, Paragraph, Wrap},
};
use std::{io, path::PathBuf, time::Duration};
use tokio::sync::oneshot;

#[path = "queue.rs"]
mod queue;

const NAMES: [Message; 9] = [
    Message::Repository,
    Message::Revision,
    Message::Endpoint,
    Message::Token,
    Message::Proxy,
    Message::Destination,
    Message::ConnectionsPerFile,
    Message::ConcurrentFiles,
    Message::Filter,
];
const INPUT_HEIGHT: u16 = 11;
const STATUS_HEIGHT: u16 = 4;
const DRAFT_SHORTCUT_HEIGHT: u16 = 6;
#[derive(Clone, Copy, Debug)]
pub(crate) enum NotificationDiagnostic {
    QueueUnavailable,
    TimedOut,
    DeliveryFailed,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Diagnostic {
    pub logging_failed: bool,
    pub identity_error: Option<hfdx_core::notification::NotificationError>,
    pub notification: Option<NotificationDiagnostic>,
}

impl Diagnostic {
    pub fn render(&self, language: hfdx_core::storage::Language) -> String {
        use hfdx_core::storage::Language::{English, ZhHans, ZhHant};
        let mut messages = Vec::new();
        if self.logging_failed {
            messages.push(match language {
                English => "Logging unavailable",
                ZhHans => "日志不可用",
                ZhHant => "記錄功能無法使用",
            });
        }
        if let Some(error) = self.identity_error {
            use hfdx_core::notification::NotificationError;
            messages.push(match (language, error) {
                (English, NotificationError::IdentityConflict) => "Notification identity conflict: inspect the existing shortcut; do not overwrite it",
                (ZhHans, NotificationError::IdentityConflict) => "通知身份冲突：请检查已有快捷方式，不要覆盖",
                (ZhHant, NotificationError::IdentityConflict) => "通知身分衝突：請檢查現有捷徑，不要覆寫",
                (English, NotificationError::IdentityUnavailable) => "Notification identity unavailable; register explicitly with --register-notifications",
                (ZhHans, NotificationError::IdentityUnavailable) => "通知身份不可用；可使用 --register-notifications 显式注册",
                (ZhHant, NotificationError::IdentityUnavailable) => "通知身分無法使用；可使用 --register-notifications 明確註冊",
                (English, _) => "Notification identity could not be prepared",
                (ZhHans, _) => "无法准备通知身份",
                (ZhHant, _) => "無法準備通知身分",
            });
        }
        if let Some(notification) = self.notification {
            messages.push(match (language, notification) {
                (English, NotificationDiagnostic::QueueUnavailable) => {
                    "Notification queue full or closed"
                }
                (ZhHans, NotificationDiagnostic::QueueUnavailable) => "通知队列已满或已关闭",
                (ZhHant, NotificationDiagnostic::QueueUnavailable) => "通知佇列已滿或已關閉",
                (English, NotificationDiagnostic::TimedOut) => "Notification timed out",
                (ZhHans, NotificationDiagnostic::TimedOut) => "通知超时",
                (ZhHant, NotificationDiagnostic::TimedOut) => "通知逾時",
                (English, NotificationDiagnostic::DeliveryFailed) => "Notification unavailable",
                (ZhHans, NotificationDiagnostic::DeliveryFailed) => "通知不可用",
                (ZhHant, NotificationDiagnostic::DeliveryFailed) => "通知無法使用",
            });
        }
        messages.join("; ")
    }
}

enum Completion {
    QueueControl,
    Start(StartRequest),
    Starting,
    None,
    Load,
    Open,
    Resume(bool),
    Settings,
}

struct App {
    token_edit: Option<String>,
    fields: [String; 9],
    focus: usize,
    cursor: usize,
    offset: usize,
    dataset: bool,
    only_selected: bool,
    message: String,
    diagnostic: Diagnostic,
    message_error: Option<hfdx_core::task::UiError>,
    recent_index: usize,
    history_open: bool,
    pending: Vec<(
        oneshot::Receiver<Result<CommandReceipt, CoreError>>,
        Completion,
    )>,
    token_origin: String,
    settings_open: bool,
    settings_selection: usize,
    settings_pending: bool,
    status_scroll: u16,
    profiles_open: bool,
    profile_cursor: usize,
    queue_open: bool,
    queue_control_pending: bool,
    queue_cursor: usize,
    queue_scroll: u16,
    queue_collapsed: std::collections::BTreeSet<String>,
}
impl App {
    fn new(repo: String, snapshot: &AppSnapshot) -> Self {
        let settings = &snapshot.settings;
        Self {
            token_edit: None,
            fields: [
                repo,
                if settings.endpoint.is_modelscope() {
                    "master"
                } else {
                    "main"
                }
                .into(),
                settings
                    .endpoint
                    .url()
                    .as_str()
                    .trim_end_matches('/')
                    .into(),
                String::new(),
                settings
                    .proxy_address
                    .as_ref()
                    .map_or_else(String::new, |p| {
                        p.url().as_str().trim_end_matches('/').into()
                    }),
                String::new(),
                settings.options.connections_per_file().to_string(),
                settings.options.concurrent_files().to_string(),
                snapshot.selection.filter.clone(),
            ],
            focus: 0,
            cursor: 0,
            offset: 0,
            dataset: false,
            only_selected: snapshot.selection.selected_only,
            message: String::new(),
            diagnostic: Diagnostic::default(),
            message_error: None,
            recent_index: 0,
            history_open: false,
            pending: Vec::new(),
            token_origin: settings.endpoint.url().origin().ascii_serialization(),
            settings_open: false,
            settings_selection: 0,
            settings_pending: false,
            status_scroll: 0,
            profiles_open: false,
            profile_cursor: 0,
            queue_open: false,
            queue_control_pending: false,
            queue_cursor: 0,
            queue_scroll: 0,
            queue_collapsed: Default::default(),
        }
    }
    fn insert_field(&mut self, session: &CoreSession, text: &str) {
        if self.focus == 3 && self.fields[3].is_empty() {
            self.token_origin =
                Endpoint::parse(&self.fields[2], self.fields[2].starts_with("http://"))
                    .map_or_else(
                        |_| String::new(),
                        |endpoint| endpoint.url().origin().ascii_serialization(),
                    );
        }
        self.fields[self.focus].push_str(text);
        if self.focus == 8 {
            self.submit(session, CoreCommand::SetFilter(self.fields[8].clone()));
            self.cursor = 0;
            self.offset = 0;
        }
    }
    fn submit(&mut self, session: &CoreSession, command: CoreCommand) {
        let completion = match &command {
            CoreCommand::Load(_) | CoreCommand::LoadActive(_) => Completion::Load,
            CoreCommand::OpenTask(_) => Completion::Open,
            CoreCommand::Start(_) => Completion::Starting,
            CoreCommand::NewDownload => Completion::None,
            _ => Completion::None,
        };
        self.submit_with(session, command, completion);
    }
    fn queue_control(&mut self, session: &CoreSession, command: CoreCommand) {
        if self.queue_control_pending {
            return;
        }
        self.queue_control_pending = true;
        self.submit_with(session, command, Completion::QueueControl);
        if self.queue_control_pending {
            self.message = queue::text(
                session.snapshots.borrow().settings.effective_language(),
                "Working…",
                "正在处理…",
                "正在處理…",
            )
            .into();
        }
    }
    fn submit_with(&mut self, session: &CoreSession, command: CoreCommand, completion: Completion) {
        match session.handle.submit(command) {
            Ok(receiver) => {
                self.message.clear();
                self.message_error = None;
                if matches!(completion, Completion::Starting | Completion::Start(_)) {
                    self.queue_open = true;
                    self.message = queue::text(
                        session.snapshots.borrow().settings.effective_language(),
                        "Initializing download task…",
                        "正在初始化下载任务…",
                        "正在初始化下載工作…",
                    )
                    .into();
                }
                self.pending.push((receiver, completion));
            }
            Err(e) => {
                if matches!(completion, Completion::Settings) {
                    self.settings_pending = false;
                }
                if matches!(completion, Completion::QueueControl) {
                    self.queue_control_pending = false;
                }
                self.message = format!(
                    "{}: {e:?}",
                    locale::text(
                        session.snapshots.borrow().settings.effective_language(),
                        Message::CommandUnavailable
                    )
                );
            }
        }
    }
    fn access(&self, language: hfdx_core::storage::Language) -> Result<NetworkAccess, String> {
        Ok(NetworkAccess {
            token: if self.fields[3].is_empty() {
                None
            } else {
                Some(
                    Token::parse(&self.fields[3])
                        .map_err(|_| locale::text(language, Message::AuthenticationError))?,
                )
            },
            proxy: if self.fields[4].is_empty() {
                None
            } else {
                Some(
                    ProxySpec::parse(&self.fields[4])
                        .map_err(|_| locale::text(language, Message::NetworkError))?,
                )
            },
        })
    }
    fn opened(&mut self, request: &RepositoryRequest) {
        let origin = request.endpoint.url().origin().ascii_serialization();
        if origin != self.token_origin {
            self.fields[3].clear();
        }
        self.fields[0] = request.repo_id.as_str().to_owned();
        self.fields[1] = request.requested_revision.as_str().to_owned();
        self.fields[2] = request
            .endpoint
            .url()
            .as_str()
            .trim_end_matches('/')
            .to_owned();
        self.dataset = request.kind == RepoKind::Dataset;
    }
    fn load_accepted(&mut self) {
        self.fields[8].clear();
        self.only_selected = false;
        self.cursor = 0;
        self.offset = 0;
    }
    fn load(&mut self, session: &CoreSession) -> Result<(), String> {
        let language = session.snapshots.borrow().settings.effective_language();
        let endpoint = Endpoint::parse(&self.fields[2], self.fields[2].starts_with("http://"))
            .map_err(|_| locale::text(language, Message::UnsupportedEndpointError))?;
        let defaults = RepositoryRequest {
            endpoint,
            kind: if self.dataset {
                RepoKind::Dataset
            } else {
                RepoKind::Model
            },
            repo_id: RepoId::parse("placeholder")
                .map_err(|_| locale::text(language, Message::InternalError))?,
            requested_revision: Revision::parse(&self.fields[1])
                .map_err(|_| locale::text(language, Message::InputError))?,
        };
        let repository = parse_repository(&self.fields[0], &defaults)
            .map_err(|_| locale::text(language, Message::InputError))?;
        if repository.endpoint.url().origin().ascii_serialization() != self.token_origin
            && !self.fields[3].is_empty()
        {
            return Err(locale::text(
                session.snapshots.borrow().settings.effective_language(),
                Message::EndpointChanged,
            )
            .into());
        }
        self.fields[5] = hfdx_core::task::draft_destination(
            &session.snapshots.borrow(),
            &repository,
            std::path::Path::new(&self.fields[5]),
        )
        .map_err(|_| locale::text(language, Message::DiskError))?
        .display()
        .to_string();
        self.submit(
            session,
            CoreCommand::Load(LoadRequest {
                repository,
                access: self.access(language)?,
            }),
        );
        self.cursor = 0;
        self.offset = 0;
        Ok(())
    }
    fn start(&mut self, session: &CoreSession, snapshot: &AppSnapshot) -> Result<(), String> {
        let catalog = snapshot
            .selection
            .catalog
            .as_ref()
            .ok_or_else(|| {
                locale::text(
                    snapshot.settings.effective_language(),
                    Message::LoadRepositoryFirst,
                )
            })?
            .id;
        let language = snapshot.settings.effective_language();
        let connections = self.fields[6]
            .parse()
            .map_err(|_| locale::text(language, Message::InvalidConnections))?;
        let concurrent = self.fields[7]
            .parse()
            .map_err(|_| locale::text(language, Message::InvalidConcurrency))?;
        let options = DownloadOptions::new(connections, concurrent)
            .map_err(|_| locale::text(language, Message::InputError))?;
        if self.fields[5].is_empty() {
            return Err(locale::text(language, Message::EnterDestination).into());
        }
        let destination = PathBuf::from(&self.fields[5]);
        let request = StartRequest {
            catalog,
            destination,
            options,
            verify_after_download: snapshot.settings.verify_after_download,
        };
        if snapshot.queue.is_some() && snapshot.settings.options.concurrent_files() != concurrent {
            let mut settings = (*snapshot.settings).clone();
            settings.options =
                DownloadOptions::new(settings.options.connections_per_file(), concurrent)
                    .map_err(|_| locale::text(language, Message::InvalidConcurrency))?;
            self.submit_with(
                session,
                CoreCommand::SaveSettings(settings),
                Completion::Start(request),
            );
        } else {
            self.submit(session, CoreCommand::Start(request));
        }
        Ok(())
    }
}

fn shortcuts(app: &App, snapshot: &AppSnapshot) -> String {
    let language = snapshot.settings.effective_language();
    let text = |en, cn, tw| queue::text(language, en, cn, tw);
    let queue = snapshot.queue.as_deref();
    let controls = if app.token_edit.is_some() {
        locale::text(language, Message::TokenEditHelp)
    } else if app.settings_open {
        text(
            "↑/↓ Select | Enter/Space Change, edit or open | Esc Back",
            "↑/↓ 选择 | 回车/空格 修改、编辑或打开 | Esc 返回",
            "↑/↓ 選擇 | Enter/空白鍵 修改、編輯或開啟 | Esc 返回",
        )
    } else if app.history_open {
        text(
            "↑/↓ Select | Enter Open saved task | Esc Back",
            "↑/↓ 选择 | 回车 打开已保存任务 | Esc 返回",
            "↑/↓ 選擇 | Enter 開啟已儲存工作 | Esc 返回",
        )
    } else if queue.is_some_and(|q| q.conflicts.is_some()) && !app.profiles_open {
        return text(
            "1 Overwrite | 2 Only remote newer | 3 Archive conflicts\nPgUp/PgDn Scroll | Esc Cancel submission | Ctrl+C Quit",
            "1 覆盖 | 2 仅远端较新时覆盖 | 3 归档冲突文件\nPgUp/PgDn 滚动 | Esc 取消提交 | Ctrl+C 退出",
            "1 覆寫 | 2 僅遠端較新時覆寫 | 3 歸檔衝突檔案\nPgUp/PgDn 捲動 | Esc 取消提交 | Ctrl+C 離開",
        )
        .into();
    } else if app.queue_open && queue.is_some() && !app.profiles_open {
        text(
            "↑/↓ Select | Space Expand/collapse | PgUp/PgDn Scroll | Enter Focus task\np Pause | r Resume | a Append fixed version | P/R All | Esc Back",
            "↑/↓ 选择 | 空格 展开/收起 | PgUp/PgDn 滚动 | 回车 聚焦任务\np 暂停 | r 继续 | a 追加此固定版本 | P/R 全部 | Esc 返回",
            "↑/↓ 選擇 | 空白鍵 展開/收合 | PgUp/PgDn 捲動 | Enter 聚焦工作\np 暫停 | r 繼續 | a 追加此固定版本 | P/R 全部 | Esc 返回",
        )
    } else if queue.is_some_and(|q| q.preparing) && !app.profiles_open {
        text(
            "Ctrl+J Downloads | Esc Cancel checks (while available)",
            "Ctrl+J 下载任务 | Esc 取消检查（可取消时）",
            "Ctrl+J 下載工作 | Esc 取消檢查（可取消時）",
        )
    } else if app.profiles_open {
        text(
            "↑/↓ Select | Space Toggle | Enter View final files | Esc Back",
            "↑/↓ 选择 | 空格 勾选 | 回车 查看最终文件 | Esc 返回",
            "↑/↓ 選擇 | 空白鍵 勾選 | Enter 檢視最終檔案 | Esc 返回",
        )
    } else {
        text(
            "F2 Type | F3 Load files | F4 Start | Ctrl+G Combinations | F8 Selected only\nF5 Pause | F6 Resume | F7 Retry | Ctrl+V Verify | PgUp/PgDn Status\nTab/Shift+Tab Field | Ctrl+Shift+V Paste | Ctrl+U Clear field | Esc Files\n↑/↓ Space Files | Ctrl+A/D/X Select/Deselect/Clear | F10 Open recent\nCtrl+J New filter OR line | F11 Clear filter | F12 Show all",
            "F2 类型 | F3 查看文件 | F4 开始 | Ctrl+G 下载组合 | F8 仅已选\nF5 暂停 | F6 继续 | F7 重试 | Ctrl+V 校验 | PgUp/PgDn 状态\nTab/Shift+Tab 切换输入 | Ctrl+Shift+V 粘贴 | Ctrl+U 清空 | Esc 文件列表\n↑/↓ 空格 选文件 | Ctrl+A/D/X 批量选/取消/清空 | F10 打开最近任务\nCtrl+J 新增筛选条件（或） | F11 清除筛选 | F12 显示全部",
            "F2 類型 | F3 檢視檔案 | F4 開始 | Ctrl+G 下載組合 | F8 僅已選\nF5 暫停 | F6 繼續 | F7 重試 | Ctrl+V 驗證 | PgUp/PgDn 狀態\nTab/Shift+Tab 切換輸入 | Ctrl+Shift+V 貼上 | Ctrl+U 清除 | Esc 檔案列表\n↑/↓ 空白鍵 選檔 | Ctrl+A/D/X 批次選/取消/清除 | F10 開啟最近工作\nCtrl+J 新增篩選條件（或） | F11 清除篩選 | F12 顯示全部",
        )
    };
    let navigation = if app.token_edit.is_some() {
        text("Ctrl+C Quit", "Ctrl+C 退出", "Ctrl+C 離開")
    } else {
        text(
            "Ctrl+N New | Ctrl+D Downloads | F9 History | Ctrl+S Settings | Ctrl+C Quit",
            "Ctrl+N 新建 | Ctrl+D 下载任务 | F9 历史 | Ctrl+S 设置 | Ctrl+C 退出",
            "Ctrl+N 新增 | Ctrl+D 下載工作 | F9 歷史 | Ctrl+S 設定 | Ctrl+C 離開",
        )
    };
    format!("{controls}\n{navigation}")
}

fn render(frame: &mut ratatui::Frame<'_>, app: &App, snapshot: &AppSnapshot) {
    let area = frame.area();
    let language = snapshot.settings.effective_language();
    let t = |message| locale::text(language, message);
    frame.render_widget(
        Block::default().style(crate::settings::style(snapshot.settings.theme)),
        area,
    );
    if area.width < 80 || area.height < 24 {
        frame.render_widget(
            Paragraph::new(t(Message::TerminalTooSmall)).wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let shortcuts = shortcuts(app, snapshot);
    let [area, footer] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(shortcuts.lines().count() as u16),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(shortcuts), footer);
    if app.settings_open {
        let diagnostic = snapshot
            .diagnostics
            .log_directory
            .as_ref()
            .map_or_else(|| app.message.clone(), |path| path.display().to_string());
        crate::settings::render(
            frame,
            area,
            &snapshot.settings,
            app.settings_selection,
            &diagnostic,
            &app.message,
            app.token_edit.as_deref(),
        );
        return;
    }
    if app.history_open {
        let height = area.height.saturating_sub(2) as usize;
        let mut lines = snapshot
            .recent
            .iter()
            .enumerate()
            .skip(app.recent_index.saturating_sub(height.saturating_sub(1)))
            .take(height)
            .map(|(i, item)| {
                format!(
                    "{} {} · {} · {}",
                    if i == app.recent_index { ">" } else { " " },
                    item.state
                        .map_or(t(Message::Unknown), |state| locale::state(language, state)),
                    item.task_file.display(),
                    t(if item.destination_exists {
                        Message::Exists
                    } else {
                        Message::Missing
                    })
                )
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(
                queue::text(
                    language,
                    "No download history yet",
                    "暂无下载历史",
                    "尚無下載記錄",
                )
                .into(),
            );
        }
        frame.render_widget(
            Paragraph::new(lines.join("\n"))
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(queue::text(language, "History", "历史", "歷史"))),
            area,
        );
        return;
    }
    if queue::render(frame, area, app, snapshot) {
        return;
    }
    if app.profiles_open {
        let profiles = snapshot
            .selection
            .catalog
            .as_ref()
            .map(|c| hfdx_core::profiles::analyze(c))
            .unwrap_or_default();
        let text = profiles
            .iter()
            .enumerate()
            .skip(app.profile_cursor.saturating_sub(8))
            .take(16)
            .map(|(i, p)| {
                format!(
                    "{} [{}] {} · {}{}",
                    if i == app.profile_cursor { ">" } else { " " },
                    if p.files
                        .iter()
                        .all(|id| snapshot.selection.selected.contains(id))
                    {
                        "x"
                    } else {
                        " "
                    },
                    p.label,
                    bytes(p.bytes),
                    if p.complete { "" } else { " [incomplete]" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(
            Paragraph::new(text).block(Block::bordered().title(queue::text(
                language,
                "Combinations",
                "下载组合",
                "下載組合",
            ))),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(INPUT_HEIGHT),
        Constraint::Min(3),
        Constraint::Length(STATUS_HEIGHT),
    ])
    .split(area);
    let mut fields = String::new();
    for (index, name) in NAMES.iter().enumerate() {
        let value = if index == 3 || index == 4 {
            "*".repeat(app.fields[index].chars().count())
        } else {
            app.fields[index].replace('\n', " | ")
        };
        fields.push_str(&format!(
            "{} {}: {value}\n",
            if app.focus == index { ">" } else { " " },
            t(*name)
        ));
    }
    frame.render_widget(
        Paragraph::new(fields).block(Block::bordered().title(format!(
            " {} ({}) ",
            t(if app.dataset {
                Message::Dataset
            } else {
                Message::Model
            }),
            t(Message::TypeToggle)
        ))),
        rows[0],
    );
    let selection = &snapshot.selection;
    let height = usize::from(rows[1].height.saturating_sub(2));
    let mut lines = Vec::new();
    if let Some(catalog) = &selection.catalog {
        for (index, id) in selection
            .visible
            .iter()
            .enumerate()
            .skip(app.offset)
            .take(height)
        {
            if let Some(entry) = catalog.entry(*id) {
                lines.push(format!(
                    "{} [{}] {} ({})",
                    if app.focus == 9 && index == app.cursor {
                        ">"
                    } else {
                        " "
                    },
                    if selection.selected.contains(id) {
                        "x"
                    } else {
                        " "
                    },
                    entry.path.as_str(),
                    bytes(entry.size.get())
                ));
            }
        }
    }
    if selection.visible.is_empty() && selection.state == hfdx_core::selection::CatalogState::Ready
    {
        lines.push(if selection.matches.is_empty() {
            t(Message::NoMatches).into()
        } else if selection.selected_only {
            t(Message::SelectedOnlyHidesMatches).into()
        } else {
            String::new()
        });
    }
    frame.render_widget(
        Paragraph::new(lines.join("\n")).block(Block::bordered().title(format!(
            " {} {} | {} {} | {} {} ({}) | {} {} | {} {} ",
            t(Message::Match),
            selection.matches.len(),
            t(Message::Visible),
            selection.visible.len(),
            t(Message::Selected),
            selection.summary.selected_count,
            bytes(selection.summary.selected_bytes.get()),
            t(Message::Hidden),
            selection.summary.hidden_selected_count,
            t(Message::SelectedOnly),
            t(if selection.selected_only {
                Message::On
            } else {
                Message::Off
            })
        ))),
        rows[1],
    );
    let mut status = format!(
        "{} {}",
        locale::phase(language, snapshot.phase),
        app.message_error.as_ref().map_or_else(
            || app.message.clone(),
            |error| locale::error(language, error)
        )
    );
    if app.fields[3].is_empty()
        && Endpoint::parse(&app.fields[2], true)
            .ok()
            .is_some_and(|e| snapshot.settings.token_for(&e).is_none())
    {
        status.push_str(&format!("\n{}", t(Message::TokenMissing)));
    }
    if app.fields[2].starts_with("http://") {
        status.push_str(&format!(" | {}", t(Message::HttpCredentialsWarning)));
    }
    if let Some(error) = snapshot
        .queue
        .as_ref()
        .and_then(|q| q.draft_error.as_ref())
        .or(snapshot.error.as_ref())
    {
        status.push_str(&format!(" | {}", locale::error(language, error)));
    }
    if let Some(task) = &snapshot.task {
        status.push_str(&format!(
            " | {}: {}",
            t(Message::Task),
            locale::state(language, task.record.state)
        ));
        if task.restart_notice.is_some() {
            status.push_str(&format!(" | {}", t(Message::Interrupted)));
        }
        let p = &task.progress;
        status.push_str(&format!(
            "\n{} @ {} -> {}\n{}/{} {}, {}/{}, {}, ETA {}",
            task.record.source.request.repo_id.as_str(),
            task.record.source.resolved_commit.as_str(),
            task.destination.display(),
            p.completed_files,
            p.total_files,
            t(Message::Files),
            bytes(p.acquired_bytes.get()),
            bytes(p.total_bytes.get()),
            match task.record.state {
                TaskState::Paused => t(Message::Paused).to_owned(),
                TaskState::Running => speed(Some(p.bytes_per_second)),
                TaskState::Completed | TaskState::Failed => speed(None),
            },
            p.eta.map_or_else(
                || t(Message::Unknown).into(),
                |x| format!("{}s", x.as_secs())
            )
        ));
        if let Some(verification) = task.verification {
            status.insert_str(
                0,
                &format!(
                    "{} {}/{} ({})\n",
                    t(Message::Verifying),
                    verification.files_done,
                    verification.files_total,
                    bytes(verification.file_bytes_done)
                ),
            );
        } else if task.record.state == TaskState::Completed
            || task
                .record
                .files
                .iter()
                .any(|file| file.verification == VerificationStatus::Failed)
        {
            let mut passed = 0;
            let mut unavailable = 0;
            let mut failed = 0;
            for file in &task.record.files {
                match file.verification {
                    VerificationStatus::Passed => passed += 1,
                    VerificationStatus::Failed => failed += 1,
                    VerificationStatus::LegacyUnverified
                    | VerificationStatus::Unavailable
                    | VerificationStatus::Pending => unavailable += 1,
                }
            }
            let outcome = if failed > 0 {
                hfdx_core::task::CompletionOutcome::IntegrityFailed
            } else if task.record.verify_after_download == Some(false) {
                hfdx_core::task::CompletionOutcome::VerificationDisabled
            } else if unavailable > 0 {
                hfdx_core::task::CompletionOutcome::Unverified
            } else {
                hfdx_core::task::CompletionOutcome::Verified
            };
            status.insert_str(
                0,
                &format!(
                    "{}\n",
                    locale::completion(language, outcome, passed, unavailable, failed)
                ),
            );
        }
        for (index, failure) in p.failed.iter().take(2) {
            if let Some(file) = task.record.files.get(*index) {
                status.push_str(&format!(
                    "\n{}: {}",
                    file.entry.path.as_str(),
                    locale::file_failure(language, failure)
                ));
            }
        }
    }
    if let Some(recent) = snapshot.recent.get(app.recent_index) {
        status.push_str(&format!(
            "\n{} {}/{}: {} {} {} {}",
            t(Message::Recent),
            app.recent_index + 1,
            snapshot.recent.len(),
            recent.task_file.display(),
            recent
                .state
                .map_or("", |state| locale::state(language, state)),
            t(if recent.destination_exists {
                Message::Exists
            } else {
                Message::Missing
            }),
            recent.issue.as_ref().map_or("", |_| t(Message::InputError))
        ));
    }
    let diagnostic = app.diagnostic.render(language);
    if !diagnostic.is_empty() {
        status.insert_str(0, &format!("{diagnostic}\n"));
    }
    frame.render_widget(
        Paragraph::new(status)
            .scroll((app.status_scroll, 0))
            .block(Block::bordered().title(format!("{} (PgUp/PgDn)", t(Message::Status))))
            .wrap(Wrap { trim: false }),
        rows[2],
    );
}

fn paste(app: &mut App, session: &CoreSession, snapshot: &AppSnapshot, text: &str) {
    if let Some(value) = &mut app.token_edit {
        let text = text.trim();
        if !text.chars().any(char::is_control) {
            value.push_str(text);
        }
        return;
    }
    if app.settings_open
        || app.history_open
        || app.queue_open
        || app.profiles_open
        || app.focus >= app.fields.len()
        || snapshot
            .queue
            .as_ref()
            .is_some_and(|queue| queue.preparing || queue.conflicts.is_some())
    {
        return;
    }

    let normalized;
    let text = match app.focus {
        8 => {
            normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            &normalized
        }
        5 => text.trim_matches(['\r', '\n']),
        _ => text.trim(),
    };
    if text.is_empty() {
        return;
    }
    app.message_error = None;
    if text
        .chars()
        .any(|ch| ch.is_control() && !(app.focus == 8 && ch == '\n'))
    {
        app.message =
            locale::text(snapshot.settings.effective_language(), Message::InputError).into();
        return;
    }
    app.insert_field(session, text);
}

fn handle_key(
    app: &mut App,
    session: &CoreSession,
    snapshot: &AppSnapshot,
    key: crossterm::event::KeyEvent,
    height: u16,
) {
    let conflicts = snapshot
        .queue
        .as_ref()
        .is_some_and(|q| q.conflicts.is_some());
    let page = match key.code {
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(0),
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(1),
        KeyCode::F(9) => Some(2),
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(3),
        _ => None,
    };
    if let Some(page) = page.filter(|_| !conflicts && app.token_edit.is_none()) {
        app.queue_open = page == 1;
        app.history_open = page == 2;
        app.settings_open = page == 3;
        app.profiles_open = false;
        return;
    }
    if app.history_open {
        match key.code {
            KeyCode::Esc => app.history_open = false,
            KeyCode::Down => {
                app.recent_index =
                    (app.recent_index + 1).min(snapshot.recent.len().saturating_sub(1))
            }
            KeyCode::Up => app.recent_index = app.recent_index.saturating_sub(1),
            KeyCode::Enter => {
                if let Some(recent) = snapshot.recent.get(app.recent_index) {
                    app.submit(session, CoreCommand::OpenTask(recent.task_file.clone()));
                    app.history_open = false;
                    app.queue_open = snapshot.queue.is_some();
                }
            }
            _ => {}
        }
        return;
    }
    if !app.settings_open && !app.profiles_open && queue::key(app, session, snapshot, key) {
        return;
    }
    if app.profiles_open {
        let profiles = snapshot
            .selection
            .catalog
            .as_ref()
            .map(|c| hfdx_core::profiles::analyze(c))
            .unwrap_or_default();
        match key.code {
            KeyCode::Esc => app.profiles_open = false,
            KeyCode::Enter => {
                app.profiles_open = false;
                app.fields[8].clear();
                app.only_selected = true;
                app.submit(session, CoreCommand::SetFilter(String::new()));
                app.submit(session, CoreCommand::SetSelectedOnly(true));
            }
            KeyCode::Down => {
                app.profile_cursor = (app.profile_cursor + 1).min(profiles.len().saturating_sub(1))
            }
            KeyCode::Up => app.profile_cursor = app.profile_cursor.saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(p) = profiles.get(app.profile_cursor).filter(|p| p.complete) {
                    let selected = !p
                        .files
                        .iter()
                        .all(|id| snapshot.selection.selected.contains(id));
                    if let Some(catalog) = &snapshot.selection.catalog {
                        app.submit(
                            session,
                            CoreCommand::ChangeProfile {
                                catalog: catalog.id,
                                index: app.profile_cursor,
                                selected,
                            },
                        );
                    }
                }
            }
            _ => {}
        }
        return;
    }
    if app.settings_open {
        if let Some(value) = app.token_edit.as_mut() {
            match key.code {
                KeyCode::Esc => app.token_edit = None,
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.clear()
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    value.push(ch)
                }
                KeyCode::Enter => {
                    let token = if value.is_empty() {
                        Ok(None)
                    } else {
                        Token::parse(value).map(Some)
                    };
                    match token {
                        Ok(token) => {
                            let mut settings = (*snapshot.settings).clone();
                            if app.settings_selection == 6 {
                                settings.hf_token = token;
                            } else {
                                settings.modelscope_token = token;
                            }
                            app.token_edit = None;
                            app.fields[3].clear();
                            app.settings_pending = true;
                            app.submit_with(
                                session,
                                CoreCommand::SaveSettings(settings),
                                Completion::Settings,
                            );
                        }
                        Err(_) => {
                            app.message = locale::text(
                                snapshot.settings.effective_language(),
                                Message::AuthenticationError,
                            )
                            .into()
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc => app.settings_open = false,
            KeyCode::Down => app.settings_selection = (app.settings_selection + 1) % 10,
            KeyCode::Up => app.settings_selection = (app.settings_selection + 9) % 10,
            KeyCode::Enter | KeyCode::Char(' ') if !app.settings_pending => {
                if matches!(app.settings_selection, 6 | 7) {
                    app.token_edit = Some(
                        if app.settings_selection == 6 {
                            snapshot.settings.hf_token.as_ref()
                        } else {
                            snapshot.settings.modelscope_token.as_ref()
                        }
                        .map_or("", |t| t.expose())
                        .into(),
                    );
                    return;
                }
                if matches!(app.settings_selection, 8 | 9) {
                    let url = if app.settings_selection == 8 {
                        "https://huggingface.co/settings/tokens"
                    } else {
                        "https://modelscope.ai/my/myaccesstoken"
                    };
                    let program = if cfg!(windows) {
                        "explorer"
                    } else {
                        "xdg-open"
                    };
                    match std::process::Command::new(program).arg(url).spawn() {
                        Ok(mut child) => {
                            std::thread::spawn(move || child.wait());
                        }
                        Err(_) => {
                            app.message = locale::text(
                                snapshot.settings.effective_language(),
                                Message::NetworkError,
                            )
                            .into();
                        }
                    }
                    return;
                }
                let mut settings = (*snapshot.settings).clone();
                crate::settings::change(&mut settings, app.settings_selection);
                app.settings_pending = true;
                app.submit_with(
                    session,
                    CoreCommand::SaveSettings(settings),
                    Completion::Settings,
                );
            }
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.profiles_open = true;
            app.profile_cursor = 0;
        }
        KeyCode::PageDown => app.status_scroll = app.status_scroll.saturating_add(1),
        KeyCode::PageUp => app.status_scroll = app.status_scroll.saturating_sub(1),
        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.submit(session, CoreCommand::VerifyExisting)
        }
        KeyCode::Tab => app.focus = (app.focus + 1) % 10,
        KeyCode::BackTab => app.focus = (app.focus + 9) % 10,
        KeyCode::F(2) => app.dataset = !app.dataset,
        KeyCode::F(3) => {
            if let Err(e) = app.load(session) {
                app.message = e;
            }
        }
        KeyCode::F(4) => {
            if let Err(e) = app.start(session, snapshot) {
                app.message = e;
            }
        }
        KeyCode::F(5) => app.queue_control(session, CoreCommand::Pause),
        KeyCode::F(6) | KeyCode::F(7) => {
            let endpoint = Endpoint::parse(&app.fields[2], app.fields[2].starts_with("http://"));
            match endpoint
                .map_err(|_| {
                    locale::text(
                        snapshot.settings.effective_language(),
                        Message::UnsupportedEndpointError,
                    )
                    .to_owned()
                })
                .and_then(|endpoint| {
                    if !app.fields[3].is_empty()
                        && endpoint.url().origin().ascii_serialization() != app.token_origin
                    {
                        return Err(locale::text(
                            snapshot.settings.effective_language(),
                            Message::EndpointChanged,
                        )
                        .into());
                    }
                    Ok((
                        endpoint,
                        app.access(snapshot.settings.effective_language())?,
                    ))
                }) {
                Ok((endpoint, access)) => app.submit_with(
                    session,
                    CoreCommand::SetAccess { endpoint, access },
                    Completion::Resume(key.code == KeyCode::F(7)),
                ),
                Err(error) => app.message = error,
            }
        }
        KeyCode::F(8) => {
            app.only_selected = !app.only_selected;
            app.submit(session, CoreCommand::SetSelectedOnly(app.only_selected));
            app.cursor = 0;
            app.offset = 0;
        }
        KeyCode::F(11) => {
            app.fields[8].clear();
            app.submit(session, CoreCommand::SetFilter(String::new()));
            app.cursor = 0;
            app.offset = 0;
        }
        KeyCode::F(12) => {
            app.fields[8].clear();
            app.only_selected = false;
            app.submit(session, CoreCommand::SetFilter(String::new()));
            app.submit(session, CoreCommand::SetSelectedOnly(false));
            app.cursor = 0;
            app.offset = 0;
        }
        KeyCode::F(10) => {
            if let Some(recent) = snapshot.recent.get(app.recent_index) {
                app.submit(session, CoreCommand::OpenTask(recent.task_file.clone()));
            }
        }
        KeyCode::Esc => app.focus = 9,
        KeyCode::Down if app.focus == 9 => {
            app.cursor = app
                .cursor
                .saturating_add(1)
                .min(snapshot.selection.visible.len().saturating_sub(1));
            let height = usize::from(
                height.saturating_sub(INPUT_HEIGHT + STATUS_HEIGHT + DRAFT_SHORTCUT_HEIGHT + 2),
            )
            .max(1);
            if app.cursor >= app.offset + height {
                app.offset = app.cursor + 1 - height;
            }
        }
        KeyCode::Up if app.focus == 9 => {
            app.cursor = app.cursor.saturating_sub(1);
            app.offset = app.offset.min(app.cursor);
        }
        KeyCode::Char(' ') if app.focus == 9 => {
            if let Some(id) = snapshot.selection.visible.get(app.cursor).copied() {
                app.submit(
                    session,
                    CoreCommand::ChangeSelection(SelectionChange::Set {
                        id,
                        selected: !snapshot.selection.selected.contains(&id),
                    }),
                );
            }
        }
        KeyCode::Char(ch) if app.focus == 9 && key.modifiers.contains(KeyModifiers::CONTROL) => {
            let change = match ch {
                'a' => Some(SelectionChange::SelectMatches),
                'd' => Some(SelectionChange::DeselectMatches),
                'x' => Some(SelectionChange::Clear),
                _ => None,
            };
            if let Some(change) = change {
                app.submit(session, CoreCommand::ChangeSelection(change));
            }
        }
        KeyCode::Char('u') if app.focus < 9 && key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.fields[app.focus].clear();
            if app.focus == 8 {
                app.submit(session, CoreCommand::SetFilter(String::new()));
            }
        }
        KeyCode::Char('j') if app.focus == 8 && key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.insert_field(session, "\n");
        }
        KeyCode::Char(ch)
            if app.focus < 9
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            app.insert_field(session, ch.encode_utf8(&mut [0; 4]));
        }
        KeyCode::Backspace if app.focus < 9 => {
            app.fields[app.focus].pop();
            if app.focus == 8 {
                app.submit(session, CoreCommand::SetFilter(app.fields[8].clone()));
                app.cursor = 0;
                app.offset = 0;
            }
        }
        _ => {}
    }
}

pub fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    _runtime: &tokio::runtime::Runtime,
    session: &CoreSession,
    repo: String,
    task_path: Option<PathBuf>,
    mut diagnostics: tokio::sync::watch::Receiver<Diagnostic>,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    let mut app = App::new(repo, &session.snapshots.borrow());
    app.diagnostic = diagnostics.borrow_and_update().clone();
    if let Some(path) = task_path {
        app.submit(session, CoreCommand::OpenTask(path));
    }
    loop {
        if diagnostics.has_changed().is_ok_and(|changed| changed) {
            app.diagnostic = diagnostics.borrow_and_update().clone();
        }
        let pending = std::mem::take(&mut app.pending);
        for (mut receiver, completion) in pending {
            match receiver.try_recv() {
                Ok(Ok(_)) => match completion {
                    Completion::None => {}
                    Completion::Starting => app.message.clear(),
                    Completion::QueueControl => {
                        app.queue_control_pending = false;
                        app.message.clear();
                    }
                    Completion::Start(request) => app.submit(session, CoreCommand::Start(request)),
                    Completion::Settings => app.settings_pending = false,
                    Completion::Load => app.load_accepted(),
                    Completion::Open => {
                        if let Some(task) = &session.snapshots.borrow().task {
                            app.opened(&task.record.source.request);
                        }
                    }
                    Completion::Resume(retry) => app.queue_control(
                        session,
                        if retry {
                            CoreCommand::RetryFailed
                        } else {
                            CoreCommand::Resume
                        },
                    ),
                },
                Ok(Err(error)) => {
                    if matches!(completion, Completion::Settings) {
                        app.settings_pending = false;
                    }
                    if matches!(completion, Completion::QueueControl) {
                        app.queue_control_pending = false;
                    }
                    app.message.clear();
                    app.message_error = Some(error.ui_error());
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    app.pending.push((receiver, completion))
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    if matches!(completion, Completion::Settings) {
                        app.settings_pending = false;
                    }
                    if matches!(completion, Completion::QueueControl) {
                        app.queue_control_pending = false;
                    }
                    app.message = locale::text(
                        session.snapshots.borrow().settings.effective_language(),
                        Message::CoordinatorStopped,
                    )
                    .into()
                }
            }
        }
        let snapshot = session.snapshots.borrow().clone();
        terminal.draw(|frame| render(frame, &app, &snapshot))?;
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(key) => key,
            Event::Paste(text) => {
                paste(&mut app, session, &snapshot, &text);
                continue;
            }
            _ => continue,
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            break;
        }
        app.message_error = None;
        handle_key(&mut app, session, &snapshot, key, terminal.size()?.height);
    }
    Ok(())
}
