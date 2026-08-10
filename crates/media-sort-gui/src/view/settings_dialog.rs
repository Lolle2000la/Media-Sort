use iced::widget::{button, checkbox, column, container, pick_list, row, scrollable, text};
use iced::{Alignment, Color, Element, Length};

use crate::message::{Message, SettingsMessage};
use crate::state::AppState;
use crate::subscriptions::keyboard::format_keybinding;

const BOLD_FONT: iced::Font = iced::Font {
    weight: iced::font::Weight::Bold,
    ..iced::Font::DEFAULT
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeOption {
    key: &'static str,
    name: &'static str,
}

impl std::fmt::Display for ThemeOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

static THEME_OPTIONS: &[ThemeOption] = &[
    ThemeOption {
        key: "Light",
        name: "Light",
    },
    ThemeOption {
        key: "Dark",
        name: "Dark",
    },
    ThemeOption {
        key: "Dracula",
        name: "Dracula",
    },
    ThemeOption {
        key: "Nord",
        name: "Nord",
    },
    ThemeOption {
        key: "SolarizedLight",
        name: "Solarized Light",
    },
    ThemeOption {
        key: "SolarizedDark",
        name: "Solarized Dark",
    },
    ThemeOption {
        key: "GruvboxLight",
        name: "Gruvbox Light",
    },
    ThemeOption {
        key: "GruvboxDark",
        name: "Gruvbox Dark",
    },
    ThemeOption {
        key: "CatppuccinLatte",
        name: "Catppuccin Latte",
    },
    ThemeOption {
        key: "CatppuccinFrappe",
        name: "Catppuccin Frappe",
    },
    ThemeOption {
        key: "CatppuccinMacchiato",
        name: "Catppuccin Macchiato",
    },
    ThemeOption {
        key: "CatppuccinMocha",
        name: "Catppuccin Mocha",
    },
    ThemeOption {
        key: "TokyoNight",
        name: "Tokyo Night",
    },
    ThemeOption {
        key: "TokyoNightStorm",
        name: "Tokyo Night Storm",
    },
    ThemeOption {
        key: "TokyoNightLight",
        name: "Tokyo Night Light",
    },
    ThemeOption {
        key: "KanagawaWave",
        name: "Kanagawa Wave",
    },
    ThemeOption {
        key: "KanagawaDragon",
        name: "Kanagawa Dragon",
    },
    ThemeOption {
        key: "KanagawaLotus",
        name: "Kanagawa Lotus",
    },
    ThemeOption {
        key: "Moonfly",
        name: "Moonfly",
    },
    ThemeOption {
        key: "Nightfly",
        name: "Nightfly",
    },
    ThemeOption {
        key: "Oxocarbon",
        name: "Oxocarbon",
    },
    ThemeOption {
        key: "Ferra",
        name: "Ferra",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocaleOption {
    code: String,
    display: String,
}

impl std::fmt::Display for LocaleOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display)
    }
}

fn locale_options() -> Vec<LocaleOption> {
    media_sort_core::l10n::AVAILABLE_LOCALES
        .iter()
        .map(|&code| LocaleOption {
            code: code.to_string(),
            display: media_sort_core::l10n::locale_display_name(code).to_string(),
        })
        .collect()
}

fn get_keybinding(
    state: &AppState,
    idx: usize,
) -> &media_sort_core::settings::keybindings::KeyBinding {
    let kb = &state.settings.keybindings;
    match idx {
        0 => &kb.move_to_folder,
        1 => &kb.copy_to_folder,
        2 => &kb.delete,
        3 => &kb.rename,
        4 => &kb.go_left,
        5 => &kb.go_right,
        6 => &kb.create_folder,
        7 => &kb.folder_up,
        8 => &kb.folder_left,
        9 => &kb.folder_down,
        10 => &kb.folder_right,
        11 => &kb.undo,
        12 => &kb.redo,
        13 => &kb.open_folder,
        14 => &kb.open_selected_folder,
        15 => &kb.pin,
        16 => &kb.pin_selected,
        17 => &kb.unpin,
        18 => &kb.move_pinned_up,
        19 => &kb.move_pinned_down,
        20 => &kb.search_images,
        21 => &kb.toggle_metadata_panel,
        22 => &kb.reveal_in_file_manager,
        _ => panic!("Invalid keybinding index: {}", idx),
    }
}

fn keybinding_row<'a>(state: &'a AppState, idx: usize, label: String) -> Element<'a, Message> {
    let binding = get_keybinding(state, idx);
    let is_editing = matches!(
        &state.settings_ui,
        crate::state::SettingsUiState::Keybindings {
            editing_keybinding: Some(i),
            ..
        } if *i == idx
    );
    let shortcut_text = if is_editing {
        state.l10n.tr("keybindings-press-key")
    } else {
        format_keybinding(binding)
    };

    let btn_label = if is_editing {
        text(shortcut_text)
            .color(Color::from_rgb(1.0, 0.8, 0.0))
            .size(12)
    } else {
        text(shortcut_text).size(12)
    };

    let kb_container = container(
        button(btn_label)
            .on_press(Message::Settings(SettingsMessage::EditKeyBinding(idx)))
            .style(iced::widget::button::secondary)
            .width(Length::Fixed(120.0)),
    );
    #[cfg(feature = "demo")]
    let kb_container = kb_container.id(iced_automation::static_widget_id(format!(
        "keybinding_edit_{}",
        idx
    )));

    row![
        text(label).size(12).width(Length::Fixed(240.0)),
        kb_container,
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn keybinding_section<'a>(title: String, items: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    column(std::iter::once(text(title).font(BOLD_FONT).size(14).into()).chain(items))
        .spacing(8)
        .into()
}

fn keybinding_subsection<'a>(
    title: String,
    items: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    column(std::iter::once(text(title).size(12).into()).chain(items))
        .spacing(6)
        .padding(iced::Padding::new(0.0).left(12.0))
        .into()
}

pub fn settings_dialog_view(state: &AppState) -> Element<'_, Message> {
    let title = text(state.l10n.tr("settings-title"))
        .size(20)
        .font(BOLD_FONT);

    // Tab bar
    let tab_bar = row![
        container(
            button(text(state.l10n.tr("settings-tab-general")).size(13))
                .on_press(Message::Settings(SettingsMessage::Open))
                .style(
                    if matches!(state.settings_ui, crate::state::SettingsUiState::Settings) {
                        iced::widget::button::primary
                    } else {
                        iced::widget::button::secondary
                    }
                ),
        )
        .id(iced::widget::Id::new("settings_tab_general")),
        container(
            button(text(state.l10n.tr("settings-tab-advanced")).size(13))
                .on_press(Message::Settings(SettingsMessage::OpenAdvanced))
                .style(
                    if matches!(state.settings_ui, crate::state::SettingsUiState::Advanced) {
                        iced::widget::button::primary
                    } else {
                        iced::widget::button::secondary
                    }
                ),
        )
        .id(iced::widget::Id::new("settings_tab_advanced")),
        container(
            button(text(state.l10n.tr("settings-tab-keybindings")).size(13))
                .on_press(Message::Settings(SettingsMessage::OpenKeybindings))
                .style(
                    if matches!(
                        state.settings_ui,
                        crate::state::SettingsUiState::Keybindings { .. }
                    ) {
                        iced::widget::button::primary
                    } else {
                        iced::widget::button::secondary
                    }
                ),
        )
        .id(iced::widget::Id::new("settings_tab_keybindings")),
    ]
    .spacing(10);

    let tab_content: Element<'_, Message> = match &state.settings_ui {
        crate::state::SettingsUiState::Settings => {
            // General settings tab
            let animate_gifs_cb = checkbox(state.settings.general.animate_gifs)
                .label(state.l10n.tr("settings-play-gifs"))
                .on_toggle(|_| Message::Settings(SettingsMessage::ToggleAnimateGifs))
                .size(16);

            #[cfg(feature = "velopack")]
            let check_updates_cb = checkbox(state.settings.general.check_for_updates_on_startup)
                .label(state.l10n.tr("settings-check-updates"))
                .on_toggle(|_| Message::Settings(SettingsMessage::ToggleCheckForUpdates))
                .size(16);

            #[cfg(feature = "velopack")]
            let install_prerelease_cb = checkbox(state.settings.general.install_prerelease_builds)
                .label(state.l10n.tr("settings-install-prerelease"))
                .on_toggle(|_| Message::Settings(SettingsMessage::ToggleInstallPrerelease))
                .size(16);

            let reopen_folder_cb = checkbox(state.settings.general.reopen_last_opened_folder)
                .label(state.l10n.tr("settings-reopen-folder"))
                .on_toggle(|_| Message::Settings(SettingsMessage::ToggleReopenFolder))
                .size(16);

            let mut reopen_media_cb = checkbox(state.settings.general.reopen_last_selected_media)
                .label(state.l10n.tr("settings-reopen-media"))
                .size(16);

            if state.settings.general.reopen_last_opened_folder {
                reopen_media_cb = reopen_media_cb
                    .on_toggle(|_| Message::Settings(SettingsMessage::ToggleReopenMedia));
            }

            let current_theme = THEME_OPTIONS
                .iter()
                .find(|opt| opt.key == state.settings.general.theme)
                .cloned();

            let theme_picklist = column![
                text(state.l10n.tr("settings-theme")).size(12),
                container(
                    pick_list(THEME_OPTIONS, current_theme, |opt: ThemeOption| {
                        Message::Settings(SettingsMessage::SetTheme(opt.key.to_string()))
                    },)
                    .width(Length::Fixed(200.0))
                )
                .id(iced::widget::Id::new("theme_pick_list")),
            ]
            .spacing(4);

            #[allow(unused_mut)]
            let mut settings_col = column![
                column![
                    text(state.l10n.tr("settings-appearance"))
                        .font(BOLD_FONT)
                        .size(14),
                    theme_picklist,
                    reopen_folder_cb,
                    reopen_media_cb,
                ]
                .spacing(8),
                column![
                    text(state.l10n.tr("settings-animated-gifs"))
                        .font(BOLD_FONT)
                        .size(14),
                    animate_gifs_cb,
                ]
                .spacing(8),
            ];

            #[cfg(feature = "velopack")]
            {
                settings_col = settings_col.push(
                    column![
                        text(state.l10n.tr("settings-updates"))
                            .font(BOLD_FONT)
                            .size(14),
                        check_updates_cb,
                        install_prerelease_cb,
                    ]
                    .spacing(8),
                );
            }

            settings_col = settings_col.push(
                column![
                    text(state.l10n.tr("settings-language"))
                        .font(BOLD_FONT)
                        .size(14),
                    pick_list(
                        locale_options(),
                        Some(LocaleOption {
                            code: state.l10n.locale(),
                            display: media_sort_core::l10n::locale_display_name(
                                &state.l10n.locale()
                            )
                            .to_string(),
                        }),
                        |opt: LocaleOption| Message::Settings(SettingsMessage::ChangeLanguage(
                            opt.code
                        )),
                    )
                    .width(Length::Fixed(200.0)),
                ]
                .spacing(8),
            );

            #[cfg(target_os = "windows")]
            {
                let integration_with_windows_cb =
                    checkbox(state.settings.general.integration_with_windows)
                        .label(state.l10n.tr("settings-windows-context-menu"))
                        .on_toggle(toggle_integration_with_windows)
                        .size(16);

                settings_col = settings_col.push(
                    column![
                        text(state.l10n.tr("settings-windows-integration"))
                            .font(BOLD_FONT)
                            .size(14),
                        integration_with_windows_cb,
                    ]
                    .spacing(8),
                );
            }

            scrollable(settings_col).height(Length::Fill).into()
        }
        crate::state::SettingsUiState::Advanced => {
            // Advanced tab: video playback settings
            let video_hwdec_cb = checkbox(state.settings.general.video_hardware_decoding)
                .label(state.l10n.tr("settings-video-hwdec"))
                .on_toggle(|_| Message::Settings(SettingsMessage::ToggleVideoHardwareDecoding))
                .size(16);

            #[cfg(not(target_os = "macos"))]
            let video_zero_copy_element: Element<'_, Message> =
                checkbox(state.settings.general.video_zero_copy)
                    .label(state.l10n.tr("settings-video-zero-copy"))
                    .on_toggle(|_| Message::Settings(SettingsMessage::ToggleVideoZeroCopy))
                    .size(16)
                    .into();

            #[cfg(target_os = "macos")]
            let video_zero_copy_element: Element<'_, Message> = column![
                checkbox(state.settings.general.video_zero_copy)
                    .label(state.l10n.tr("settings-video-zero-copy"))
                    .size(16),
                text(state.l10n.tr("settings-video-zero-copy-macos-notice"))
                    .size(11)
                    .color(Color::from_rgb(0.6, 0.6, 0.6)),
            ]
            .spacing(4)
            .into();

            let advanced_col = column![
                column![
                    text(state.l10n.tr("settings-video-playback"))
                        .font(BOLD_FONT)
                        .size(14),
                    video_hwdec_cb,
                    video_zero_copy_element,
                ]
                .spacing(8),
            ]
            .spacing(16);

            scrollable(advanced_col).height(Length::Fill).into()
        }
        _ => {
            // Key bindings tab
            let restore_btn = button(text(state.l10n.tr("keybindings-restore-defaults")).size(12))
                .on_press(Message::Settings(
                    SettingsMessage::RestoreDefaultKeyBindings,
                ))
                .style(iced::widget::button::secondary);

            // Images Section
            let images_management = keybinding_subsection(
                state.l10n.tr("keybindings-management"),
                vec![
                    keybinding_row(state, 0, state.l10n.tr("keybindings-move")),
                    keybinding_row(state, 1, state.l10n.tr("keybindings-copy")),
                    keybinding_row(state, 2, state.l10n.tr("keybindings-delete")),
                    keybinding_row(state, 3, state.l10n.tr("keybindings-rename")),
                    keybinding_row(
                        state,
                        22,
                        state.l10n.tr("keybindings-reveal-in-file-manager"),
                    ),
                ],
            );
            let images_selection = keybinding_subsection(
                state.l10n.tr("keybindings-selection"),
                vec![
                    keybinding_row(state, 4, state.l10n.tr("keybindings-select-left")),
                    keybinding_row(state, 5, state.l10n.tr("keybindings-select-right")),
                ],
            );
            let images_search = keybinding_subsection(
                state.l10n.tr("keybindings-search"),
                vec![keybinding_row(
                    state,
                    20,
                    state.l10n.tr("keybindings-search-images"),
                )],
            );
            let images_metadata = keybinding_subsection(
                state.l10n.tr("keybindings-metadata"),
                vec![keybinding_row(
                    state,
                    21,
                    state.l10n.tr("keybindings-toggle-metadata"),
                )],
            );
            let images_section = keybinding_section(
                state.l10n.tr("keybindings-images"),
                vec![
                    images_management,
                    images_selection,
                    images_search,
                    images_metadata,
                ],
            );

            // Folders Section
            let folders_management = keybinding_subsection(
                state.l10n.tr("keybindings-management"),
                vec![keybinding_row(
                    state,
                    6,
                    state.l10n.tr("keybindings-create-folder"),
                )],
            );
            let folders_selection = keybinding_subsection(
                state.l10n.tr("keybindings-selection"),
                vec![
                    keybinding_row(state, 7, state.l10n.tr("keybindings-select-above")),
                    keybinding_row(state, 8, state.l10n.tr("keybindings-collapse")),
                    keybinding_row(state, 9, state.l10n.tr("keybindings-select-below")),
                    keybinding_row(state, 10, state.l10n.tr("keybindings-expand")),
                ],
            );
            let folders_open = keybinding_subsection(
                state.l10n.tr("keybindings-open"),
                vec![
                    keybinding_row(state, 13, state.l10n.tr("keybindings-open-folder")),
                    keybinding_row(state, 14, state.l10n.tr("keybindings-open-selected")),
                ],
            );
            let folders_pinned = keybinding_subsection(
                state.l10n.tr("keybindings-pinned"),
                vec![
                    keybinding_row(state, 15, state.l10n.tr("keybindings-pin")),
                    keybinding_row(state, 16, state.l10n.tr("keybindings-pin-selected")),
                    keybinding_row(state, 17, state.l10n.tr("keybindings-unpin")),
                    keybinding_row(state, 18, state.l10n.tr("keybindings-move-pinned-up")),
                    keybinding_row(state, 19, state.l10n.tr("keybindings-move-pinned-down")),
                ],
            );
            let folders_section = keybinding_section(
                state.l10n.tr("keybindings-folders"),
                vec![
                    folders_management,
                    folders_selection,
                    folders_open,
                    folders_pinned,
                ],
            );

            // Other Section
            let other_history = keybinding_subsection(
                state.l10n.tr("keybindings-history"),
                vec![
                    keybinding_row(state, 11, state.l10n.tr("keybindings-undo")),
                    keybinding_row(state, 12, state.l10n.tr("keybindings-redo")),
                ],
            );
            let other_section =
                keybinding_section(state.l10n.tr("keybindings-other"), vec![other_history]);

            let bindings_column =
                column![restore_btn, images_section, folders_section, other_section,].spacing(16);

            scrollable(bindings_column).height(Length::Fill).into()
        }
    };

    let close_btn = container(
        button(text(state.l10n.tr("settings-close")))
            .on_press(Message::Settings(SettingsMessage::Close))
            .style(iced::widget::button::primary),
    )
    .id(iced::widget::Id::new("close_settings_btn"));

    container(
        column![title, tab_bar, tab_content, close_btn,]
            .spacing(16)
            .align_x(Alignment::Start),
    )
    .padding(24)
    .style(|theme: &iced::Theme| {
        let palette = theme.palette();
        let border_color = Color {
            a: 0.2,
            ..palette.text
        };
        iced::widget::container::Style {
            background: Some(iced::Background::Color(palette.background)),
            border: iced::Border {
                radius: 8.0.into(),
                width: 1.0,
                color: border_color,
            },
            ..iced::widget::container::Style::default()
        }
    })
    .width(Length::Fixed(440.0))
    .height(Length::Fixed(500.0))
    .into()
}

#[cfg(target_os = "windows")]
fn toggle_integration_with_windows(_: bool) -> Message {
    Message::Settings(SettingsMessage::ToggleIntegrationWithWindows)
}
