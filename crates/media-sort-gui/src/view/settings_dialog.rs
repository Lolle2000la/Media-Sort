use iced::widget::{button, checkbox, column, container, pick_list, row, scrollable, text};
use iced::{Alignment, Color, Element, Length};
use media_sort_core::settings::keybindings::{KeyAction, KeyActionSection, KeyActionSubsection};

use crate::app::{THEME_OPTIONS, ThemeOption};
use crate::message::{Message, SettingsMessage};
use crate::state::{AppState, SettingsUiState};
use crate::subscriptions::keyboard::format_keybinding;

const BOLD_FONT: iced::Font = iced::Font {
    weight: iced::font::Weight::Bold,
    ..iced::Font::DEFAULT
};

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

fn keybinding_row<'a>(state: &'a AppState, action: KeyAction) -> Element<'a, Message> {
    let binding = action.binding(&state.settings.keybindings);
    let label = state.l10n.tr(action.l10n_key());
    let is_editing = matches!(
        &state.settings_ui,
        SettingsUiState::Keybindings {
            editing_keybinding: Some(active),
            ..
        } if *active == action
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
            .on_press(Message::Settings(SettingsMessage::EditKeyBinding(action)))
            .style(iced::widget::button::secondary)
            .width(Length::Fixed(120.0)),
    );
    #[cfg(feature = "demo")]
    let kb_container = kb_container.id(iced_automation::static_widget_id(format!(
        "keybinding_edit_{}",
        action.id()
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
                .style(if matches!(state.settings_ui, SettingsUiState::Settings) {
                    iced::widget::button::primary
                } else {
                    iced::widget::button::secondary
                }),
        )
        .id(iced::widget::Id::new("settings_tab_general")),
        container(
            button(text(state.l10n.tr("settings-tab-keybindings")).size(13))
                .on_press(Message::Settings(SettingsMessage::OpenKeybindings))
                .style(
                    if matches!(state.settings_ui, SettingsUiState::Keybindings { .. }) {
                        iced::widget::button::primary
                    } else {
                        iced::widget::button::secondary
                    }
                ),
        )
        .id(iced::widget::Id::new("settings_tab_keybindings")),
        container(
            button(text(state.l10n.tr("settings-tab-advanced")).size(13))
                .on_press(Message::Settings(SettingsMessage::OpenAdvanced))
                .style(if matches!(state.settings_ui, SettingsUiState::Advanced) {
                    iced::widget::button::primary
                } else {
                    iced::widget::button::secondary
                }),
        )
        .id(iced::widget::Id::new("settings_tab_advanced")),
    ]
    .spacing(10);

    let tab_content: Element<'_, Message> = match &state.settings_ui {
        SettingsUiState::Settings => {
            // General settings tab
            let animate_gifs_cb = checkbox(state.settings.general.animate_gifs)
                .label(state.l10n.tr("settings-play-gifs"))
                .on_toggle(|_| Message::Settings(SettingsMessage::ToggleAnimateGifs))
                .size(16);

            let session_pinned_cb = checkbox(state.settings.general.session_pinned_folders)
                .label(state.l10n.tr("settings-session-pinned-folders"))
                .on_toggle(|_| Message::Settings(SettingsMessage::ToggleSessionPinnedFolders))
                .size(16);

            let show_documents_cb = checkbox(state.settings.general.show_documents_by_default)
                .label(state.l10n.tr("settings-show-documents-by-default"))
                .on_toggle(|val| {
                    Message::Settings(SettingsMessage::ToggleShowDocumentsByDefault(val))
                })
                .size(16);
            let show_documents_desc =
                text(state.l10n.tr("settings-show-documents-by-default-desc"))
                    .size(11)
                    .style(|theme: &iced::Theme| text::Style {
                        color: Some(Color {
                            a: 0.7,
                            ..theme.palette().text
                        }),
                    });

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
                column![
                    text(state.l10n.tr("settings-pinned-folders"))
                        .font(BOLD_FONT)
                        .size(14),
                    session_pinned_cb,
                ]
                .spacing(8),
                column![
                    text(state.l10n.tr("filter-documents"))
                        .font(BOLD_FONT)
                        .size(14),
                    show_documents_cb,
                    show_documents_desc,
                ]
                .spacing(6),
            ]
            .spacing(12);

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
        SettingsUiState::Keybindings { .. } => {
            // Key bindings tab
            let restore_btn = button(text(state.l10n.tr("keybindings-restore-defaults")).size(12))
                .on_press(Message::Settings(
                    SettingsMessage::RestoreDefaultKeyBindings,
                ))
                .style(iced::widget::button::secondary);

            let sections = KeyActionSection::iter().map(|section| {
                let direct_rows: Vec<Element<'_, Message>> = KeyAction::iter()
                    .filter(|action| action.section() == section && action.subsection().is_none())
                    .map(|action| keybinding_row(state, action))
                    .collect();
                let subsections: Vec<Element<'_, Message>> = KeyActionSubsection::iter()
                    .filter_map(|subsection| {
                        let rows: Vec<Element<'_, Message>> = KeyAction::iter()
                            .filter(|action| {
                                action.section() == section
                                    && action.subsection() == Some(subsection)
                            })
                            .map(|action| keybinding_row(state, action))
                            .collect();
                        if rows.is_empty() {
                            None
                        } else {
                            Some(keybinding_subsection(
                                state.l10n.tr(subsection.l10n_key()),
                                rows,
                            ))
                        }
                    })
                    .collect();
                let items: Vec<Element<'_, Message>> =
                    direct_rows.into_iter().chain(subsections).collect();
                keybinding_section(state.l10n.tr(section.l10n_key()), items)
            });

            let bindings_column =
                column(std::iter::once(restore_btn.into()).chain(sections)).spacing(16);

            scrollable(bindings_column).height(Length::Fill).into()
        }
        SettingsUiState::Advanced => {
            // Advanced settings tab
            let disable_hw_decoding_cb =
                checkbox(state.settings.advanced.disable_hardware_decoding)
                    .label(state.l10n.tr("settings-disable-hw-decoding"))
                    .on_toggle(|_| Message::Settings(SettingsMessage::ToggleHardwareDecoding))
                    .size(16);

            let enable_libreoffice_cb =
                checkbox(state.settings.advanced.enable_libreoffice_preview)
                    .label(state.l10n.tr("settings-enable-libreoffice-preview"))
                    .on_toggle(|val| {
                        Message::Settings(SettingsMessage::ToggleLibreOfficePreview(val))
                    })
                    .size(16);
            let libreoffice_desc = text(state.l10n.tr("settings-enable-libreoffice-preview-desc"))
                .size(11)
                .style(|theme: &iced::Theme| text::Style {
                    color: Some(Color {
                        a: 0.7,
                        ..theme.palette().text
                    }),
                });
            let libreoffice_detected =
                media_sort_backend::media::document::soffice_pipe::is_libreoffice_available();
            let libreoffice_status = text(if libreoffice_detected {
                state.l10n.tr("settings-libreoffice-detected")
            } else {
                state.l10n.tr("settings-libreoffice-not-found")
            })
            .size(11)
            .style(move |theme: &iced::Theme| text::Style {
                color: Some(if libreoffice_detected {
                    theme.palette().success
                } else {
                    Color {
                        a: 0.7,
                        ..theme.palette().text
                    }
                }),
            });

            let advanced_col = column![
                column![
                    text(state.l10n.tr("settings-video"))
                        .font(BOLD_FONT)
                        .size(14),
                    disable_hw_decoding_cb,
                ]
                .spacing(8),
                column![
                    text(state.l10n.tr("filter-documents"))
                        .font(BOLD_FONT)
                        .size(14),
                    enable_libreoffice_cb,
                    libreoffice_desc,
                    libreoffice_status,
                ]
                .spacing(6),
            ]
            .spacing(16);

            scrollable(advanced_col).height(Length::Fill).into()
        }
        SettingsUiState::Hidden => {
            // The dialog is only rendered while the settings UI is open.
            column![].into()
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
