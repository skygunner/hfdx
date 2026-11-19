use crate::AppWindow;
use hfdx_core::{
    locale::{self, Message},
    repository::{
        Endpoint, NetworkAccess, ProxySpec, RepoId, RepoKind, RepositoryRequest, Revision, Token,
        parse_repository,
    },
    storage::{DownloadOptions, Language, Settings, Theme},
    task::{AppSnapshot, CoreCommand, LoadRequest, StartRequest},
};
use std::path::PathBuf;

pub fn access(ui: &AppWindow) -> Result<NetworkAccess, String> {
    let mut token = ui.get_token();
    if token.is_empty() {
        let endpoint = Endpoint::parse(&ui.get_endpoint(), ui.get_allow_http())
            .map_err(|_| message(ui, Message::UnsupportedEndpointError))?;
        if endpoint.url().scheme() == "https" && endpoint.url().port().is_none() {
            if endpoint.is_modelscope() {
                token = ui.get_modelscope_token();
            } else if endpoint.url().host_str() == Some("huggingface.co") {
                token = ui.get_hf_token();
                if token.is_empty() {
                    token = std::env::var("HF_TOKEN").unwrap_or_default().into();
                }
            }
        }
    }
    let proxy = ui.get_proxy();
    Ok(NetworkAccess {
        token: if token.is_empty() {
            None
        } else {
            Some(Token::parse(&token).map_err(|_| message(ui, Message::AuthenticationError))?)
        },
        proxy: if proxy.is_empty() {
            None
        } else {
            Some(ProxySpec::parse(&proxy).map_err(|_| message(ui, Message::NetworkError))?)
        },
    })
}
pub fn set_access(ui: &AppWindow) -> Result<CoreCommand, String> {
    Ok(CoreCommand::SetAccess {
        endpoint: Endpoint::parse(&ui.get_endpoint(), ui.get_allow_http())
            .map_err(|_| message(ui, Message::UnsupportedEndpointError))?,
        access: access(ui)?,
    })
}
pub fn options(ui: &AppWindow) -> Result<DownloadOptions, String> {
    let connections =
        u8::try_from(ui.get_connections()).map_err(|_| message(ui, Message::InvalidConnections))?;
    let concurrent = u8::try_from(ui.get_concurrent_files())
        .map_err(|_| message(ui, Message::InvalidConcurrency))?;
    DownloadOptions::new(connections, concurrent)
        .map_err(|_| message(ui, Message::InputError).into())
}
fn repository(ui: &AppWindow) -> Result<RepositoryRequest, String> {
    let defaults = RepositoryRequest {
        endpoint: Endpoint::parse(&ui.get_endpoint(), ui.get_allow_http())
            .map_err(|_| message(ui, Message::UnsupportedEndpointError))?,
        kind: if ui.get_repository_kind() == 1 {
            RepoKind::Dataset
        } else {
            RepoKind::Model
        },
        repo_id: RepoId::parse("placeholder").map_err(|_| message(ui, Message::InternalError))?,
        requested_revision: Revision::parse(&ui.get_revision())
            .map_err(|_| message(ui, Message::InputError))?,
    };
    parse_repository(&ui.get_repository(), &defaults)
        .map_err(|_| message(ui, Message::InputError).into())
}
pub fn recent_destination(ui: &AppWindow, base: &std::path::Path) -> Result<PathBuf, String> {
    let request = repository(ui)?;
    let basename = request
        .repo_id
        .as_str()
        .rsplit('/')
        .next()
        .ok_or_else(|| message(ui, Message::InputError))?;
    Ok(base.join(basename))
}
pub fn load(ui: &AppWindow) -> Result<CoreCommand, String> {
    let repository = repository(ui)?;
    let endpoint = Endpoint::parse(&ui.get_endpoint(), ui.get_allow_http())
        .map_err(|_| message(ui, Message::UnsupportedEndpointError))?;
    if repository.endpoint != endpoint && !ui.get_token().is_empty() {
        ui.set_token("".into());
        ui.set_endpoint(repository.endpoint.url().as_str().into());
        return Err(message(ui, Message::EndpointChanged).into());
    }
    ui.set_endpoint(repository.endpoint.url().as_str().into());
    ui.set_revision(repository.requested_revision.as_str().into());
    ui.set_repository_kind(i32::from(repository.kind == RepoKind::Dataset));
    ui.set_filter("".into());
    ui.set_selected_only(false);
    ui.set_details("".into());
    Ok(CoreCommand::Load(LoadRequest {
        repository,
        access: access(ui)?,
    }))
}
pub fn start(ui: &AppWindow, snapshot: &AppSnapshot) -> Result<CoreCommand, String> {
    let catalog = snapshot
        .selection
        .catalog
        .as_ref()
        .ok_or_else(|| message(ui, Message::LoadRepositoryFirst))?
        .id;
    if ui.get_destination().trim().is_empty() {
        return Err(message(ui, Message::EnterDestination).into());
    }
    let connections = u8::try_from(ui.get_task_connections())
        .map_err(|_| message(ui, Message::InvalidConnections))?;
    let concurrent = u8::try_from(ui.get_task_concurrent_files())
        .map_err(|_| message(ui, Message::InvalidConcurrency))?;
    Ok(CoreCommand::Start(StartRequest {
        catalog,
        destination: PathBuf::from(ui.get_destination().as_str()),
        options: DownloadOptions::new(connections, concurrent)
            .map_err(|_| message(ui, Message::InputError))?,
        verify_after_download: ui.get_task_verify(),
    }))
}
pub fn settings(ui: &AppWindow, current: &Settings) -> Result<CoreCommand, String> {
    let mut settings = current.clone();
    let (hf, modelscope) = platform_tokens(ui)?;
    settings.hf_token = hf;
    settings.modelscope_token = modelscope;
    settings.verify_after_download = ui.get_verify();
    settings.notify_on_completion = ui.get_notify();
    settings.sound_on_completion = ui.get_sound();
    settings.auto_update_check = ui.get_auto_check();
    settings.update_interval =
        hfdx_core::updates::Interval::from_index(ui.get_update_frequency() as usize);
    settings.debug_logging = ui.get_debug();
    settings.theme = theme(ui);
    settings.language = Some(language(ui));
    settings.endpoint = Endpoint::parse(&ui.get_endpoint(), ui.get_allow_http())
        .map_err(|_| message(ui, Message::UnsupportedEndpointError))?;
    settings.options = options(ui)?;
    settings.proxy_address = access(ui)?.proxy.as_ref().map(ProxySpec::address);
    if !ui.get_destination().trim().is_empty() {
        let path = PathBuf::from(ui.get_destination().as_str());
        if let Some(base) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            settings.recent_directories.retain(|p| p != base);
            settings.recent_directories.insert(0, base.to_path_buf());
            settings.recent_directories.truncate(10);
        }
    }
    Ok(CoreCommand::SaveSettings(settings))
}

pub fn platform_tokens(ui: &AppWindow) -> Result<(Option<Token>, Option<Token>), String> {
    let parse = |value: &str| {
        let value = value.trim();
        if value.is_empty() {
            Ok(None)
        } else {
            Token::parse(value)
                .map(Some)
                .map_err(|_| message(ui, Message::AuthenticationError).to_owned())
        }
    };
    Ok((
        parse(&ui.get_hf_token())?,
        parse(&ui.get_modelscope_token())?,
    ))
}

pub fn language(ui: &AppWindow) -> Language {
    match ui.get_language_index() {
        1 => Language::ZhHans,
        2 => Language::ZhHant,
        _ => Language::English,
    }
}
pub fn theme(ui: &AppWindow) -> Theme {
    match ui.get_theme_index() {
        1 => Theme::Light,
        2 => Theme::Dark,
        _ => Theme::System,
    }
}
pub fn message(ui: &AppWindow, value: Message) -> &'static str {
    locale::text(language(ui), value)
}
