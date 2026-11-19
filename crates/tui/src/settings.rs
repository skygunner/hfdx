use hfdx_core::{
    locale::{Message, text},
    storage::{Language, Settings, Theme},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Clear, Paragraph, Wrap},
};

pub fn change(settings: &mut Settings, selected: usize) {
    match selected {
        0 => {
            settings.language = match settings.language {
                None => Some(Language::English),
                Some(Language::English) => Some(Language::ZhHans),
                Some(Language::ZhHans) => Some(Language::ZhHant),
                Some(Language::ZhHant) => None,
            }
        }
        1 => {
            settings.theme = match settings.theme {
                Theme::System => Theme::Light,
                Theme::Light => Theme::Dark,
                Theme::Dark => Theme::System,
            }
        }
        2 => settings.verify_after_download = !settings.verify_after_download,
        3 => settings.notify_on_completion = !settings.notify_on_completion,
        4 => settings.sound_on_completion = !settings.sound_on_completion,
        5 => settings.debug_logging = !settings.debug_logging,
        _ => {}
    }
}

pub fn style(theme: Theme) -> Style {
    match theme {
        Theme::System => Style::default(),
        Theme::Light => Style::default().fg(Color::Black).bg(Color::White),
        Theme::Dark => Style::default().fg(Color::White).bg(Color::Black),
    }
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    settings: &Settings,
    selected: usize,
    diagnostic: &str,
    message: &str,
    token_edit: Option<&str>,
) {
    let language = settings.effective_language();
    let t = |message| text(language, message);
    let language_name = match settings.language {
        None => t(Message::System),
        Some(Language::English) => "English",
        Some(Language::ZhHans) => "简体中文",
        Some(Language::ZhHant) => "繁體中文",
    };
    let theme = t(match settings.theme {
        Theme::System => Message::System,
        Theme::Light => Message::Light,
        Theme::Dark => Message::Dark,
    });
    let toggle = |value| {
        if value {
            t(Message::On)
        } else {
            t(Message::Off)
        }
    };
    let values = [
        (Message::Language, language_name),
        (Message::Theme, theme),
        (
            Message::VerifyAfterDownload,
            toggle(settings.verify_after_download),
        ),
        (
            Message::NotifyOnCompletion,
            toggle(settings.notify_on_completion),
        ),
        (
            Message::SoundOnCompletion,
            toggle(settings.sound_on_completion),
        ),
        (Message::DebugLogging, toggle(settings.debug_logging)),
    ];
    let mut lines = String::new();
    for (index, (name, value)) in values.iter().enumerate() {
        lines.push_str(&format!(
            "{} {}: {}\n",
            if selected == index { ">" } else { " " },
            t(*name),
            value
        ));
    }
    for (index, (label, token)) in [
        ("Hugging Face", settings.hf_token.as_ref()),
        ("ModelScope.ai", settings.modelscope_token.as_ref()),
    ]
    .into_iter()
    .enumerate()
    {
        let value = if selected == index + 6 {
            token_edit.map(|s| "*".repeat(s.len()))
        } else {
            None
        }
        .unwrap_or_else(|| {
            if token.is_some() {
                "********".into()
            } else {
                "—".into()
            }
        });
        lines.push_str(&format!(
            "{} {label}: {value}\n",
            if selected == index + 6 { ">" } else { " " }
        ));
    }
    for (index, url) in [
        "https://huggingface.co/settings/tokens",
        "https://modelscope.ai/my/myaccesstoken",
    ]
    .into_iter()
    .enumerate()
    {
        lines.push_str(&format!(
            "{} ↗ {url}\n",
            if selected == index + 8 { ">" } else { " " }
        ));
    }
    lines.push_str(&format!(
        "\n{}: {}\n\n{}",
        t(Message::LogDirectory),
        diagnostic,
        t(Message::TokenHelp)
    ));
    if !message.is_empty() {
        lines.push_str(&format!("\n{message}"));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(style(settings.theme))
            .block(Block::bordered().title(t(Message::Settings))),
        area,
    );
}
