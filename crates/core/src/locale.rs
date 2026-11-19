//! Runtime text shared by the GUI and TUI. User and server supplied details stay unchanged.

use crate::{
    storage::{FailureKind, Language, TaskState},
    task::{CompletionOutcome, EngineFailure, ErrorCode, ErrorDetail, UiError, UiPhase},
};

macro_rules! catalog {
    ($($name:ident => [$en:literal, $hans:literal, $hant:literal]),+ $(,)?) => {
        /// Stable identifiers for application-owned runtime labels and help.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Message { $($name),+ }

        impl Message {
            /// Every message in the runtime catalog.
            pub const ALL: &'static [Self] = &[$(Self::$name),+];
        }

        /// Return the requested application-owned text in the selected language.
        pub const fn text(language: Language, message: Message) -> &'static str {
            let translations = match message { $(Message::$name => [$en, $hans, $hant]),+ };
            match language {
                Language::English => translations[0],
                Language::ZhHans => translations[1],
                Language::ZhHant => translations[2],
            }
        }
    };
}

catalog! {
    Repository => ["Repository", "仓库", "儲存庫"],
    Revision => ["Revision", "版本", "版本"],
    Endpoint => ["Endpoint", "端点", "端點"],
    Token => ["Endpoint token override", "临时端点令牌", "暫時端點權杖"],
    Proxy => ["Proxy", "代理", "代理伺服器"],
    Destination => ["Destination", "目标目录", "目標目錄"],
    ConnectionsPerFile => ["Connections/file", "每个文件连接数", "每個檔案的連線數"],
    ConcurrentFiles => ["Concurrent files", "并发文件数", "同時下載檔案數"],
    Filter => ["Filter files", "筛选文件", "篩選檔案"],
    FilterHint => ["Enter keywords or wildcards", "输入关键词或通配符", "輸入關鍵字或萬用字元"],
    TerminalTooSmall => ["Terminal too small: minimum 80x24. Ctrl+C exits.", "终端过小：至少需要 80x24。按 Ctrl+C 退出。", "終端機過小：至少需要 80x24。按 Ctrl+C 離開。"],
    Model => ["Model", "模型", "模型"],
    Dataset => ["Dataset", "数据集", "資料集"],
    TypeToggle => ["F2 toggles", "F2 切换", "F2 切換"],
    NoMatches => ["No matching files. F11 clears the filter.", "没有匹配的文件。按 F11 清空筛选。", "沒有符合條件的檔案。按 F11 清除篩選。"],
    SelectedOnlyHidesMatches => ["Selected-only hides all matches. F12 shows all.", "仅显示已选隐藏了所有匹配项。按 F12 显示全部。", "僅顯示已選項目隱藏了所有符合項目。按 F12 顯示全部。"],
    Match => ["Match", "匹配", "符合"],
    Visible => ["Visible", "可见", "可見"],
    Selected => ["Selected", "已选", "已選"],
    Hidden => ["Hidden", "隐藏", "隱藏"],
    SelectedOnly => ["Selected only", "仅显示已选", "僅顯示已選"],
    On => ["on", "开", "開"],
    Off => ["off", "关", "關"],
    Status => ["Status", "状态", "狀態"],
    Task => ["Task", "任务", "工作"],
    Recent => ["Recent", "最近任务", "最近工作"],
    Exists => ["exists", "存在", "存在"],
    Missing => ["missing", "不存在", "不存在"],
    Files => ["files", "个文件", "個檔案"],
    Active => ["Active", "进行中", "進行中"],
    Failed => ["Failed", "失败", "失敗"],
    NoActiveOrFailedFiles => ["No active or failed files.", "没有进行中或失败的文件。", "沒有進行中或失敗的檔案。"],
    Unknown => ["unknown", "未知", "未知"],
    HttpCredentialsWarning => ["HTTP sends credentials in cleartext", "HTTP 会以明文发送凭据", "HTTP 會以明文傳送憑證"],
    CommandUnavailable => ["Command unavailable", "命令不可用", "命令無法使用"],
    EndpointChanged => ["Endpoint changed: clear or re-enter the token for the new endpoint", "端点已更改：请清除令牌或重新输入新端点的令牌", "端點已變更：請清除權杖或重新輸入新端點的權杖"],
    LoadRepositoryFirst => ["Load a repository first", "请先加载仓库", "請先載入儲存庫"],
    InvalidConnections => ["Invalid connections", "连接数无效", "連線數無效"],
    InvalidConcurrency => ["Invalid concurrency", "并发数无效", "同時下載數無效"],
    EnterDestination => ["Enter a destination", "请输入目标目录", "請輸入目標目錄"],
    CoordinatorStopped => ["Coordinator stopped unexpectedly", "任务协调器意外停止", "工作協調器意外停止"],
    Settings => ["Settings", "设置", "設定"],
    AppearanceAndLanguage => ["Appearance and language", "外观与语言", "外觀與語言"],
    Language => ["Language", "语言", "語言"],
    Theme => ["Theme", "主题", "佈景主題"],
    System => ["System", "跟随系统", "跟隨系統"],
    Light => ["Light", "浅色", "淺色"],
    Dark => ["Dark", "深色", "深色"],
    Download => ["Download", "下载", "下載"],
    VerifyAfterDownload => ["Verify file integrity after download", "下载完成后校验文件完整性", "下載完成後驗證檔案完整性"],
    Notifications => ["Notifications", "通知", "通知"],
    NotifyOnCompletion => ["Show a notification when a task finishes", "任务完成时显示通知", "工作完成時顯示通知"],
    SoundOnCompletion => ["Play a sound when a task finishes", "任务完成时播放提示音", "工作完成時播放提示音"],
    Diagnostics => ["Diagnostics", "诊断", "診斷"],
    DebugLogging => ["Enable debug logging", "启用 Debug 日志", "啟用 Debug 記錄"],
    LogDirectory => ["Log directory", "日志目录", "記錄目錄"],
    AdvancedNetwork => ["Advanced network", "高级网络", "進階網路"],
    Idle => ["Idle", "空闲", "閒置"],
    Loading => ["Loading", "正在加载", "正在載入"],
    Browsing => ["Browsing", "浏览中", "瀏覽中"],
    Preparing => ["Preparing", "正在准备", "正在準備"],
    Downloading => ["Downloading", "正在下载", "正在下載"],
    Verifying => ["Verifying", "正在校验", "正在驗證"],
    Paused => ["Paused", "已暂停", "已暫停"],
    Stopping => ["Stopping", "正在停止", "正在停止"],
    Closed => ["Closed", "已关闭", "已關閉"],
    Running => ["Running", "运行中", "執行中"],
    Completed => ["Completed", "已完成", "已完成"],
    Verified => ["Download complete; integrity verified", "下载完成，校验通过", "下載完成，驗證通過"],
    VerificationDisabled => ["Download complete; verification disabled", "下载完成，校验已关闭", "下載完成，驗證已關閉"],
    Unverified => ["Download complete; some files could not be verified", "下载完成，部分文件未能校验", "下載完成，部分檔案無法驗證"],
    IntegrityFailed => ["Integrity verification failed", "完整性校验失败", "完整性驗證失敗"],
    PassedCount => ["verified", "已校验", "已驗證"],
    UnavailableCount => ["unverified", "未能校验", "無法驗證"],
    DisabledCount => ["verification disabled", "校验已关闭", "驗證已關閉"],
    PendingCount => ["pending verification", "待校验", "待驗證"],
    NoSupportedDigest => ["No supported digest available", "没有可用的受支持摘要", "沒有可用的受支援摘要"],
    FailedCount => ["failed", "失败", "失敗"],
    NetworkFailure => ["Network error", "网络错误", "網路錯誤"],
    AuthenticationFailure => ["Authentication error", "身份验证错误", "身分驗證錯誤"],
    DiskFailure => ["Disk error", "磁盘错误", "磁碟錯誤"],
    EngineFailure => ["Download engine error", "下载引擎错误", "下載引擎錯誤"],
    Interrupted => ["Download interrupted", "下载已中断", "下載已中斷"],
    InputError => ["Invalid input; check the supplied values", "输入无效，请检查所填内容", "輸入無效，請檢查所填內容"],
    AuthenticationError => ["Authentication failed; check access credentials", "身份验证失败，请检查访问凭据", "身分驗證失敗，請檢查存取憑證"],
    NotFoundError => ["Repository or file not found; check the repository and revision", "找不到仓库或文件，请检查仓库和版本", "找不到儲存庫或檔案，請檢查儲存庫和版本"],
    NetworkError => ["Network request failed; check the endpoint and network", "网络请求失败，请检查端点和网络", "網路請求失敗，請檢查端點和網路"],
    RateLimitedError => ["Rate limit reached; save the token for this platform in Settings, reduce connections and retry after the limit resets", "已触发平台限流；请在设置中保存对应平台的令牌，减少连接数，等待限额恢复后重试", "已觸發平台限流；請在設定中儲存對應平台的權杖，減少連線數，等待限額恢復後重試"],
    TokenHelp => ["For reliable bulk downloads, get and save the token for each platform in Settings. Anonymous access has lower limits; a token does not remove all limits. HF: https://huggingface.co/settings/tokens | ModelScope: https://modelscope.ai/my/myaccesstoken", "稳定、批量下载前，请在设置中获取并保存对应平台的令牌。匿名访问限额较低，令牌也不能免除全部限流。HF：https://huggingface.co/settings/tokens | ModelScope：https://modelscope.ai/my/myaccesstoken", "穩定、批次下載前，請在設定中取得並儲存對應平台的權杖。匿名存取限額較低，權杖也不能免除全部限流。HF：https://huggingface.co/settings/tokens | ModelScope：https://modelscope.ai/my/myaccesstoken"],
    TokenMissing => ["No token for this platform. Open Settings to get and save one before bulk downloading.", "尚未配置此平台的令牌。大量下载前，请打开设置获取并保存令牌。", "尚未設定此平台的權杖。大量下載前，請開啟設定取得並儲存權杖。"],
    TokenEditHelp => ["Ctrl+Shift+V Paste | Ctrl+U Clear | Enter Save token | Esc Cancel", "Ctrl+Shift+V 粘贴 | Ctrl+U 清空 | 回车 保存令牌 | Esc 取消", "Ctrl+Shift+V 貼上 | Ctrl+U 清除 | Enter 儲存權杖 | Esc 取消"],
    TokensSaved => ["Tokens saved to local configuration.", "令牌已保存到本地配置。", "權杖已儲存到本機設定。"],
    UnsupportedEndpointError => ["Unsupported endpoint; check endpoint compatibility", "不支持此端点，请检查端点兼容性", "不支援此端點，請檢查端點相容性"],
    UnsafePathError => ["Unsafe path; choose a safe path", "路径不安全，请选择安全的路径", "路徑不安全，請選擇安全的路徑"],
    DirectoryOtherRepository => ["This folder belongs to another repository. Choose a separate folder", "此目录属于另一仓库，请为新仓库选择独立文件夹", "此目錄屬於另一倉庫，請為新倉庫選擇獨立資料夾"],
    DirectoryOtherVersion => ["This folder has another version of this task. Append to that fixed version or choose a different folder", "此目录已有另一版本的任务，请追加到该固定版本或选择其他目录", "此目錄已有另一版本的任務，請追加至該固定版本或選擇其他目錄"],
    DestinationConflictError => ["Destination conflict; choose another destination or open the existing task", "目标目录冲突，请选择其他目录或打开已有任务", "目標目錄衝突，請選擇其他目錄或開啟現有工作"],
    LockedError => ["Task is locked; close the other application", "任务已锁定，请关闭另一个应用", "工作已鎖定，請關閉另一個應用程式"],
    CorruptRecordError => ["Task record is corrupt; preserve it for inspection", "任务记录已损坏，请保留记录以供检查", "工作紀錄已損壞，請保留紀錄以供檢查"],
    UnsupportedSchemaError => ["Unsupported task record version; use a compatible version", "不支持此任务记录版本，请使用兼容版本", "不支援此工作紀錄版本，請使用相容版本"],
    DiskError => ["Storage error; check disk space and permissions", "存储错误，请检查磁盘空间和权限", "儲存錯誤，請檢查磁碟空間和權限"],
    EngineError => ["Download engine error", "下载引擎错误", "下載引擎錯誤"],
    EngineLaunch => ["launch", "启动", "啟動"],
    EngineConfigure => ["configure", "配置", "設定"],
    EngineAdd => ["add download", "添加下载", "新增下載"],
    EngineStatus => ["check status", "查询状态", "查詢狀態"],
    EngineForget => ["remove completed job", "移除已结束的任务", "移除已結束的工作"],
    EngineShutdown => ["shutdown", "关闭", "關閉"],
    EngineUnsupportedProxy => ["aria2 supports HTTP proxy addresses only", "aria2 仅支持 HTTP 代理地址", "aria2 僅支援 HTTP 代理位址"],
    EngineIo => ["aria2 I/O failed", "aria2 输入输出失败", "aria2 輸入輸出失敗"],
    EngineProtocol => ["aria2 protocol failed", "aria2 协议错误", "aria2 協定錯誤"],
    EngineConnection => ["Could not communicate with the local aria2 process", "无法与本机 aria2 进程通信", "無法與本機 aria2 進程通訊"],
    EngineInvalidResponse => ["aria2 returned an invalid response", "aria2 返回了无效响应", "aria2 傳回了無效回應"],
    EngineExited => ["The local aria2 process exited", "本机 aria2 进程已退出", "本機 aria2 進程已結束"],
    EngineRpc => ["aria2 RPC error", "aria2 RPC 错误", "aria2 RPC 錯誤"],
    EngineTimeout => ["aria2 timed out", "aria2 请求超时", "aria2 請求逾時"],
    EngineShutdownFailure => ["aria2 shutdown failed", "aria2 关闭失败", "aria2 關閉失敗"],
    InvalidStateError => ["Command unavailable in the current state", "当前状态下无法执行此命令", "目前狀態下無法執行此命令"],
    CancelledError => ["Operation cancelled", "操作已取消", "操作已取消"],
    InternalError => ["Internal error; check the details", "内部错误，请查看详细信息", "內部錯誤，請查看詳細資訊"]
}

/// Localize a coordinator phase without relying on debug formatting.
pub const fn phase(language: Language, value: UiPhase) -> &'static str {
    text(
        language,
        match value {
            UiPhase::Idle => Message::Idle,
            UiPhase::Loading => Message::Loading,
            UiPhase::Browsing => Message::Browsing,
            UiPhase::Preparing => Message::Preparing,
            UiPhase::Downloading => Message::Downloading,
            UiPhase::Verifying => Message::Verifying,
            UiPhase::Paused => Message::Paused,
            UiPhase::Stopping => Message::Stopping,
            UiPhase::Closed => Message::Closed,
        },
    )
}

/// Localize a persisted task state.
pub const fn state(language: Language, value: TaskState) -> &'static str {
    text(
        language,
        match value {
            TaskState::Paused => Message::Paused,
            TaskState::Running => Message::Running,
            TaskState::Completed => Message::Completed,
            TaskState::Failed => Message::Failed,
        },
    )
}

/// Localize a machine-readable file failure category.
pub const fn failure(language: Language, value: FailureKind) -> &'static str {
    text(
        language,
        match value {
            FailureKind::Network => Message::NetworkFailure,
            FailureKind::Authentication => Message::AuthenticationFailure,
            FailureKind::Disk => Message::DiskFailure,
            FailureKind::Engine => Message::EngineFailure,
            FailureKind::Integrity => Message::IntegrityFailed,
            FailureKind::Interrupted => Message::Interrupted,
        },
    )
}

/// Render only typed download diagnostics; raw server messages may contain credentials.
pub fn file_failure(language: Language, value: &crate::storage::FileFailure) -> String {
    let guidance = match value.http_status {
        Some(429) => text(language, Message::RateLimitedError),
        Some(401 | 403) => text(language, Message::AuthenticationError),
        Some(404) => text(language, Message::NotFoundError),
        _ => failure(language, value.kind),
    };
    let mut result = value
        .http_status
        .map(|status| format!("HTTP {status} | "))
        .unwrap_or_default();
    if let Some(code) = value.aria2_code {
        let descriptions = match code {
            2 => ["Request timed out", "请求超时", "請求逾時"],
            3 => ["Resource not found", "资源不存在", "資源不存在"],
            5 => [
                "Transfer speed stayed below the configured limit",
                "传输速度持续低于设定下限",
                "傳輸速度持續低於設定下限",
            ],
            8 => [
                "Server does not support resuming this transfer",
                "服务器不支持续传此文件",
                "伺服器不支援續傳此檔案",
            ],
            14 => [
                "Unable to rename file",
                "无法重命名文件",
                "無法重新命名檔案",
            ],
            15 => ["Unable to open file", "无法打开文件", "無法開啟檔案"],
            17 => ["File read or write failed", "文件读写失败", "檔案讀寫失敗"],
            18 => ["Unable to create directory", "无法创建目录", "無法建立目錄"],
            19 => [
                "Host name resolution failed",
                "域名解析失败",
                "網域名稱解析失敗",
            ],
            23 => ["Too many redirects", "重定向次数过多", "重新導向次數過多"],
            6 => ["Network connection failed", "网络连接失败", "網路連線失敗"],
            9 => ["Not enough disk space", "磁盘空间不足", "磁碟空間不足"],
            13 => ["File already exists", "文件已经存在", "檔案已經存在"],
            16 => [
                "Unable to create or truncate file",
                "无法创建或截断文件",
                "無法建立或截斷檔案",
            ],
            22 => ["HTTP response rejected", "HTTP 响应失败", "HTTP 回應失敗"],
            24 => [
                "HTTP authorization failed",
                "HTTP 身份验证失败",
                "HTTP 身分驗證失敗",
            ],
            29 => [
                "Server temporarily overloaded or unavailable",
                "服务器暂时过载或不可用",
                "伺服器暫時過載或無法使用",
            ],
            32 => ["Checksum mismatch", "校验摘要不一致", "校驗摘要不一致"],
            _ => ["Download failed", "下载失败", "下載失敗"],
        };
        let index = match language {
            Language::English => 0,
            Language::ZhHans => 1,
            Language::ZhHant => 2,
        };
        result.push_str(&format!("aria2 {code}: {} | ", descriptions[index]));
    }
    result.push_str(guidance);
    result
}

/// Localize error guidance and render typed diagnostic details.
pub fn error(language: Language, value: &UiError) -> String {
    let category = match value.code {
        ErrorCode::Input => Message::InputError,
        ErrorCode::Authentication => Message::AuthenticationError,
        ErrorCode::NotFound => Message::NotFoundError,
        ErrorCode::Network => Message::NetworkError,
        ErrorCode::RateLimited => Message::RateLimitedError,
        ErrorCode::UnsupportedEndpoint => Message::UnsupportedEndpointError,
        ErrorCode::UnsafePath => Message::UnsafePathError,
        ErrorCode::DestinationConflict => Message::DestinationConflictError,
        ErrorCode::Locked => Message::LockedError,
        ErrorCode::CorruptRecord => Message::CorruptRecordError,
        ErrorCode::UnsupportedSchema => Message::UnsupportedSchemaError,
        ErrorCode::Disk => Message::DiskError,
        ErrorCode::Engine => Message::EngineError,
        ErrorCode::InvalidState => Message::InvalidStateError,
        ErrorCode::Cancelled => Message::CancelledError,
        ErrorCode::Internal => Message::InternalError,
    };
    let mut result = format!("{} [{:?}]", text(language, category), value.code);
    match &value.detail {
        Some(ErrorDetail::Destination {
            path,
            owner,
            same_repository,
        }) => {
            return format!(
                "{}\n{}\n{} @ {} · {}",
                text(
                    language,
                    if *same_repository {
                        Message::DirectoryOtherVersion
                    } else {
                        Message::DirectoryOtherRepository
                    }
                ),
                path.display(),
                owner.request.repo_id.as_str(),
                owner.resolved_commit.as_str(),
                owner.request.endpoint.url()
            );
        }
        Some(ErrorDetail::Http { endpoint, status }) => {
            result.push_str(&format!(" | HTTP {status} | {}", endpoint.url()));
        }
        Some(ErrorDetail::Engine { operation, reason }) => {
            let operation = match *operation {
                "launch" => text(language, Message::EngineLaunch),
                "configure" => text(language, Message::EngineConfigure),
                "add" => text(language, Message::EngineAdd),
                "status" => text(language, Message::EngineStatus),
                "forget" => text(language, Message::EngineForget),
                "shutdown" => text(language, Message::EngineShutdown),
                other => other,
            };
            let reason = match reason {
                EngineFailure::UnsupportedProxy => {
                    text(language, Message::EngineUnsupportedProxy).to_owned()
                }
                EngineFailure::Io => text(language, Message::EngineIo).to_owned(),
                EngineFailure::Protocol => text(language, Message::EngineProtocol).to_owned(),
                EngineFailure::Connection => text(language, Message::EngineConnection).to_owned(),
                EngineFailure::Response(part) => format!(
                    "{} ({part})",
                    text(language, Message::EngineInvalidResponse)
                ),
                EngineFailure::Exited(code) => code.map_or_else(
                    || text(language, Message::EngineExited).to_owned(),
                    |code| format!("{} ({code})", text(language, Message::EngineExited)),
                ),
                EngineFailure::Rpc(code) => {
                    format!("{} ({code})", text(language, Message::EngineRpc))
                }
                EngineFailure::Timeout => text(language, Message::EngineTimeout).to_owned(),
                EngineFailure::Shutdown => {
                    text(language, Message::EngineShutdownFailure).to_owned()
                }
            };
            result.push_str(&format!(" | {operation}: {reason}"));
        }
        None => {}
    }
    result
}

/// Format the final outcome and all verification counts.
pub fn completion(
    language: Language,
    outcome: CompletionOutcome,
    passed: u64,
    unavailable: u64,
    failed: u64,
) -> String {
    let message = match outcome {
        CompletionOutcome::Verified => Message::Verified,
        CompletionOutcome::VerificationDisabled => Message::VerificationDisabled,
        CompletionOutcome::Unverified => Message::Unverified,
        CompletionOutcome::IntegrityFailed => Message::IntegrityFailed,
    };
    format!(
        "{} ({}: {passed}, {}: {unavailable}, {}: {failed})",
        text(language, message),
        text(language, Message::PassedCount),
        text(language, Message::UnavailableCount),
        text(language, Message::FailedCount)
    )
}
