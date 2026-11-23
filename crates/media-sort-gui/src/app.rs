use iced::{Element, Subscription, Task};

use crate::message::Message;
use crate::state::AppState;
use crate::subscriptions::filesystem::filesystem_subscription;
use crate::view;

#[cfg(feature = "demo")]
use iced_automation::AutomationStateTrait;

pub fn update(state: &mut AppState, message: Message) -> Task<Message> {
    crate::update::update(state, message)
}

pub fn view(state: &AppState) -> Element<'_, Message> {
    let base_view = view::main_layout::main_layout_view(state);
    #[cfg(feature = "demo")]
    if let Some(automation) = state.automation() {
        return iced_automation::wrap_view(base_view, automation);
    }
    base_view
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThemeOption {
    pub key: &'static str,
    pub name: &'static str,
    pub theme: Option<iced::Theme>,
}

impl std::fmt::Display for ThemeOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

pub static THEME_OPTIONS: &[ThemeOption] = &[
    ThemeOption {
        key: "Auto",
        name: "Auto",
        theme: None,
    },
    ThemeOption {
        key: "Light",
        name: "Light",
        theme: Some(iced::Theme::Light),
    },
    ThemeOption {
        key: "Dark",
        name: "Dark",
        theme: Some(iced::Theme::Dark),
    },
    ThemeOption {
        key: "Dracula",
        name: "Dracula",
        theme: Some(iced::Theme::Dracula),
    },
    ThemeOption {
        key: "Nord",
        name: "Nord",
        theme: Some(iced::Theme::Nord),
    },
    ThemeOption {
        key: "SolarizedLight",
        name: "Solarized Light",
        theme: Some(iced::Theme::SolarizedLight),
    },
    ThemeOption {
        key: "SolarizedDark",
        name: "Solarized Dark",
        theme: Some(iced::Theme::SolarizedDark),
    },
    ThemeOption {
        key: "GruvboxLight",
        name: "Gruvbox Light",
        theme: Some(iced::Theme::GruvboxLight),
    },
    ThemeOption {
        key: "GruvboxDark",
        name: "Gruvbox Dark",
        theme: Some(iced::Theme::GruvboxDark),
    },
    ThemeOption {
        key: "CatppuccinLatte",
        name: "Catppuccin Latte",
        theme: Some(iced::Theme::CatppuccinLatte),
    },
    ThemeOption {
        key: "CatppuccinFrappe",
        name: "Catppuccin Frappe",
        theme: Some(iced::Theme::CatppuccinFrappe),
    },
    ThemeOption {
        key: "CatppuccinMacchiato",
        name: "Catppuccin Macchiato",
        theme: Some(iced::Theme::CatppuccinMacchiato),
    },
    ThemeOption {
        key: "CatppuccinMocha",
        name: "Catppuccin Mocha",
        theme: Some(iced::Theme::CatppuccinMocha),
    },
    ThemeOption {
        key: "TokyoNight",
        name: "Tokyo Night",
        theme: Some(iced::Theme::TokyoNight),
    },
    ThemeOption {
        key: "TokyoNightStorm",
        name: "Tokyo Night Storm",
        theme: Some(iced::Theme::TokyoNightStorm),
    },
    ThemeOption {
        key: "TokyoNightLight",
        name: "Tokyo Night Light",
        theme: Some(iced::Theme::TokyoNightLight),
    },
    ThemeOption {
        key: "KanagawaWave",
        name: "Kanagawa Wave",
        theme: Some(iced::Theme::KanagawaWave),
    },
    ThemeOption {
        key: "KanagawaDragon",
        name: "Kanagawa Dragon",
        theme: Some(iced::Theme::KanagawaDragon),
    },
    ThemeOption {
        key: "KanagawaLotus",
        name: "Kanagawa Lotus",
        theme: Some(iced::Theme::KanagawaLotus),
    },
    ThemeOption {
        key: "Moonfly",
        name: "Moonfly",
        theme: Some(iced::Theme::Moonfly),
    },
    ThemeOption {
        key: "Nightfly",
        name: "Nightfly",
        theme: Some(iced::Theme::Nightfly),
    },
    ThemeOption {
        key: "Oxocarbon",
        name: "Oxocarbon",
        theme: Some(iced::Theme::Oxocarbon),
    },
    ThemeOption {
        key: "Ferra",
        name: "Ferra",
        theme: Some(iced::Theme::Ferra),
    },
];

pub fn theme(state: &AppState) -> iced::Theme {
    if state.settings.general.theme == "Auto" {
        return match state.system_theme {
            iced::theme::Mode::Dark => iced::Theme::Dark,
            iced::theme::Mode::Light | iced::theme::Mode::None => iced::Theme::Light,
        };
    }

    THEME_OPTIONS
        .iter()
        .find(|opt| opt.key == state.settings.general.theme)
        .and_then(|opt| opt.theme.clone())
        .unwrap_or(iced::Theme::Light)
}

pub fn subscription(state: &AppState) -> Subscription<Message> {
    Subscription::batch([
        base_subscription(),
        video_subscription(state),
        filesystem_subscription(state),
    ])
}

fn base_subscription() -> Subscription<Message> {
    Subscription::batch([
        iced::time::every(std::time::Duration::from_millis(16)).map(Message::Tick),
        crate::subscriptions::keyboard::keyboard_subscription(),
        iced::event::listen().map(Message::EventOccurred),
        iced::system::theme_changes().map(Message::SystemThemeChanged),
    ])
}

fn video_subscription(state: &AppState) -> Subscription<Message> {
    let config = iced_mpv::PlayerConfig {
        hardware_decoding: !state.settings.advanced.disable_hardware_decoding,
        ..iced_mpv::PlayerConfig::default()
    };
    iced_mpv::VideoPlayer::subscription_with_config_and_map(config, Message::Video)
}

/// Like [`subscription`], but without the video worker. The headless demo
/// export never plays videos, so skipping the worker avoids spawning one
/// libmpv `MpvContext` per render.
#[cfg(feature = "demo")]
pub(crate) fn demo_subscription(_state: &AppState) -> Subscription<Message> {
    base_subscription()
}
