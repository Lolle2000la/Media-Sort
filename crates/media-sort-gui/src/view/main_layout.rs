use iced::widget::{column, container, mouse_area, row, text, tooltip};
use iced::{Color, Element, Length};
use media_sort_core::settings::keybindings::KeyAction;

use crate::message::{FolderMessage, MediaMessage, Message, SettingsMessage};
use crate::state::{AppState, FilterModeState};
use crate::view::{
    control_panel, folder_panel, media_grid, media_preview, metadata_panel, search_bar,
};

pub fn main_layout_view(state: &AppState) -> Element<'_, Message> {
    let folder_panel = folder_panel::folder_panel_view(state);
    let control_panel = control_panel::control_panel_view(state);

    let search_bar = search_bar::search_bar_view(
        &state.media_grid.search.query,
        &state.media_grid.search.placeholder,
    );
    let grid = media_grid::media_grid_view(state);
    let preview = media_preview::media_preview_view(state);
    let metadata = metadata_panel::metadata_panel_view(state);

    let can_move_or_copy = state.media_grid.selected_index.is_some()
        && state.folder.selected_folder.is_some()
        && state
            .folder
            .selected_folder
            .as_ref()
            .zip(state.folder.current_folder.as_ref())
            .is_none_or(|(sf, cf)| !media_sort_core::path_utils::paths_equal(sf, cf));

    let move_btn = {
        let btn_content = row![
            text(state.l10n.tr("ui-move")),
            text(char::from(lucide_icons::Icon::ArrowUp))
                .font(iced::Font::with_name("lucide"))
                .size(14),
        ]
        .spacing(2)
        .align_y(iced::Alignment::Center);
        let inner = iced::widget::button(btn_content).width(Length::Fill);
        let inner = if can_move_or_copy {
            inner.on_press(Message::Media(MediaMessage::MoveActive))
        } else {
            inner
        };
        container(inner)
            .id(iced::widget::Id::new("move_btn"))
            .width(Length::Fill)
    };

    let copy_btn = {
        let btn_content = row![
            text(state.l10n.tr("ui-copy")),
            text(char::from(lucide_icons::Icon::Copy))
                .font(iced::Font::with_name("lucide"))
                .size(14),
        ]
        .spacing(2)
        .align_y(iced::Alignment::Center);
        let btn = iced::widget::button(btn_content).width(Length::Fill);
        let inner = if can_move_or_copy {
            btn.on_press(Message::Media(MediaMessage::CopyActive))
        } else {
            btn
        };
        container(inner)
            .id(iced::widget::Id::new("copy_btn"))
            .width(Length::Fill)
    };

    // Rename (R) button next to search bar:
    let rename_btn = {
        let btn = iced::widget::button(text(state.l10n.tr("ui-rename")).size(13));
        if state.media_grid.selected_index.is_some() {
            btn.on_press(Message::Media(MediaMessage::TriggerRename))
        } else {
            btn
        }
    };

    // Delete (Down Arrow) button at the bottom:
    let delete_btn = {
        let btn_content = row![
            text(state.l10n.tr("ui-delete")),
            text(char::from(lucide_icons::Icon::ArrowDown))
                .font(iced::Font::with_name("lucide"))
                .size(14),
        ]
        .spacing(2)
        .align_y(iced::Alignment::Center);
        let btn = iced::widget::button(btn_content)
            .width(Length::Fill)
            .style(iced::widget::button::danger);
        let inner = if let Some(index) = state.media_grid.selected_index {
            let filtered = state.media_grid.filtered_entries();
            if let Some(entry) = filtered.get(index) {
                btn.on_press(Message::Media(MediaMessage::DeleteEntry(
                    entry.path.clone(),
                )))
            } else {
                btn
            }
        } else {
            btn
        };
        container(inner)
            .id(iced::widget::Id::new("delete_btn"))
            .width(Length::Fill)
    };

    // Metadata button:
    let metadata_btn = iced::widget::button(text(state.l10n.tr("ui-metadata")).size(13))
        .on_press(Message::Settings(SettingsMessage::ToggleMetadataPanel));

    // Row containing the search bar, Rename button, and Metadata button:
    let search_rename_row = row![search_bar, rename_btn, metadata_btn,]
        .spacing(8)
        .align_y(iced::Alignment::Center);

    // Center area (the preview area and bottom action area):
    let media_column = column![
        row![move_btn, copy_btn].spacing(4),
        container(preview).height(Length::Fill).width(Length::Fill),
        search_rename_row,
        filter_chips_view(state),
        grid,
        delete_btn,
    ]
    .spacing(8)
    .height(Length::Fill);

    let divider = divider_view(Message::Settings(SettingsMessage::StartDragFolderDivider));

    let media_metadata_row = if state.metadata.panel_expanded {
        row![
            container(media_column)
                .width(Length::Fill)
                .height(Length::Fill),
            divider_view(Message::Settings(SettingsMessage::StartDragMetadataDivider)),
            metadata,
        ]
        .spacing(0)
        .width(Length::Fill)
        .height(Length::Fill)
    } else {
        row![
            container(media_column)
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .spacing(0)
        .width(Length::Fill)
        .height(Length::Fill)
    };

    let main_content = row![
        folder_panel,
        divider,
        control_panel,
        iced::widget::Space::new().width(Length::Fixed(8.0)),
        media_metadata_row,
    ]
    .spacing(0)
    .height(Length::Fill);

    let result = container(main_content).padding(8);

    let mut overlays: Vec<(Element<'_, Message>, Message)> = Vec::new();

    if !matches!(state.settings_ui, crate::state::SettingsUiState::Hidden) {
        overlays.push((
            crate::view::settings_dialog::settings_dialog_view(state),
            Message::Settings(SettingsMessage::Close),
        ));
    }

    if state.show_credits {
        overlays.push((
            crate::view::credits_dialog::credits_dialog_view(state),
            Message::CloseCredits,
        ));
    }

    #[cfg(feature = "velopack")]
    if state.show_update_prompt {
        overlays.push((
            crate::view::update_prompt::update_prompt_view(state),
            Message::NoOp,
        ));
    }

    if let Some(ref path) = state.rename.path {
        overlays.push((
            crate::widgets::rename_modal::rename_modal_view(state, path).map(Message::Media),
            Message::Media(MediaMessage::CancelRename),
        ));
    }

    if let Some(ref parent) = state.create_folder.creating_folder_parent {
        overlays.push((
            crate::widgets::create_folder_modal::create_folder_modal_view(state, parent)
                .map(Message::Folder),
            Message::Folder(FolderMessage::CancelCreate),
        ));
    }

    let show_stack =
        !overlays.is_empty() || state.drag_drop.hovering || state.status_message.is_some();
    if show_stack {
        let mut stack = iced::widget::stack![result];
        for (overlay, close_msg) in overlays {
            stack = stack.push(crate::view::overlay::modal_overlay(overlay, close_msg));
        }
        if state.drag_drop.hovering {
            stack = stack.push(crate::view::drag_drop::drag_drop_overlay_view(state));
        }
        if let Some(status) = &state.status_message {
            stack = stack.push(crate::view::overlay::status_toast(&status.text));
        }
        return stack.into();
    }

    result.into()
}

fn divider_view<'a>(on_press: Message) -> Element<'a, Message> {
    mouse_area(
        container(
            container(
                iced::widget::Space::new()
                    .width(Length::Fixed(2.0))
                    .height(Length::Fill),
            )
            .style(|theme: &iced::Theme| {
                let palette = theme.palette();
                iced::widget::container::Style {
                    background: Some(iced::Background::Color(Color {
                        a: 0.15,
                        ..palette.text
                    })),
                    ..Default::default()
                }
            }),
        )
        .width(Length::Fixed(8.0))
        .height(Length::Fill)
        .center_x(8.0),
    )
    .on_press(on_press)
    .interaction(iced::mouse::Interaction::ResizingHorizontally)
    .into()
}

fn filter_chips_view(state: &AppState) -> Element<'_, Message> {
    let filter_mode = state.media_grid.filter_mode;
    let counts = state.media_grid.type_counts();
    let labels = [
        state.l10n.tr("filter-images"),
        state.l10n.tr("filter-video"),
        state.l10n.tr("filter-audio"),
        state.l10n.tr("filter-documents"),
    ];
    let shortcut = crate::subscriptions::keyboard::format_keybinding(
        KeyAction::ToggleFilterMode.binding(&state.settings.keybindings),
    );
    let tip_label = format!("{} ({shortcut})", state.l10n.tr("filter-mode-tooltip"));

    let mut items: Vec<Element<'_, Message>> = FilterModeState::FILTER_ORDER
        .into_iter()
        .zip(labels)
        .zip(counts)
        .enumerate()
        .map(|(idx, ((ty, label), count))| {
            let active = state.media_grid.type_filter.is_enabled(ty);
            let is_focused = filter_mode.active && filter_mode.focused_chip == idx;
            let chip_label = if filter_mode.active {
                format!("[{}] {label} ({count})", idx + 1)
            } else {
                format!("{label} ({count})")
            };
            let btn = iced::widget::button(text(chip_label).size(11))
                .padding([3, 8])
                .style(move |theme, status| filter_chip_style(theme, status, active, is_focused))
                .on_press(Message::Media(MediaMessage::ToggleTypeFilter(ty)));
            let tip_content = container(text(tip_label.clone()).size(11))
                .padding(6)
                .style(|theme: &iced::Theme| {
                    let palette = theme.palette();
                    let border_color = Color {
                        a: 0.15,
                        ..palette.text
                    };
                    iced::widget::container::Style {
                        background: Some(iced::Background::Color(palette.background)),
                        border: iced::Border {
                            radius: 4.0.into(),
                            width: 1.0,
                            color: border_color,
                        },
                        ..iced::widget::container::Style::default()
                    }
                });
            tooltip(btn, tip_content, tooltip::Position::Bottom).into()
        })
        .collect();

    if filter_mode.active {
        items.push(
            text(state.l10n.tr("filter-mode-hint"))
                .size(11)
                .style(|theme: &iced::Theme| iced::widget::text::Style {
                    color: Some(Color {
                        a: 0.65,
                        ..theme.palette().text
                    }),
                })
                .into(),
        );
    }

    row(items)
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .wrap()
        .into()
}

fn filter_chip_style(
    theme: &iced::Theme,
    status: iced::widget::button::Status,
    active: bool,
    is_focused: bool,
) -> iced::widget::button::Style {
    let mut style = if active {
        iced::widget::button::primary(theme, status)
    } else {
        iced::widget::button::secondary(theme, status)
    };
    if is_focused {
        style.border.width = 2.0;
        style.border.color = if active {
            theme.extended_palette().primary.base.text
        } else {
            theme.palette().primary
        };
    }
    style
}
