use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Color, Element, Length};

use crate::message::{MediaMessage, Message};
use crate::state::AppState;
use crate::widgets::video_player::video_player;

pub fn media_preview_view(state: &AppState) -> Element<'_, Message> {
    let Some(index) = state.media_grid.selected_index else {
        return container(text(state.l10n.tr("ui-select-file")).size(14))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    };

    let filtered = state.media_grid.filtered_entries();
    let Some(entry) = filtered.get(index) else {
        return container(text(state.l10n.tr("ui-file-not-found")).size(14))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    };

    if let Some(err_msg) = state.cache.media_errors.get_error(&entry.path) {
        let details_text = state
            .l10n
            .get("ui-file-error-details", &[("error", err_msg)]);
        return container(
            column![
                text(state.l10n.tr("ui-file-not-supported")).size(14),
                container(
                    text(details_text)
                        .size(11)
                        .style(|theme: &iced::Theme| text::Style {
                            color: Some(theme.palette().danger),
                        })
                )
                .padding([8, 12])
                .style(|theme: &iced::Theme| container::Style {
                    background: Some(theme.palette().background.into()),
                    border: iced::Border {
                        color: theme.palette().danger,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                }),
                button(text(state.l10n.tr("ui-open-externally"))).on_press(Message::Media(
                    MediaMessage::OpenExternal(entry.path.clone())
                )),
            ]
            .spacing(12)
            .align_x(Alignment::Center),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }

    let preview_element: Element<'_, Message> = match entry.media_type {
        media_sort_core::media_type::MediaType::Image => {
            if let Some((ref path, ref handle)) = state.cache.selected_image {
                if path == &entry.path {
                    iced::widget::image(handle.clone())
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .into()
                } else {
                    container(text(state.l10n.tr("ui-loading-image")).size(14))
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .into()
                }
            } else {
                container(text(state.l10n.tr("ui-loading-image")).size(14))
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            }
        }
        media_sort_core::media_type::MediaType::Video => {
            let thumb = state.cache.thumbnail_cache.peek(&entry.path).cloned();
            video_player(entry.path.clone(), state, thumb)
        }
        media_sort_core::media_type::MediaType::Audio => {
            let thumb = state.cache.thumbnail_cache.peek(&entry.path).cloned();
            crate::widgets::video_canvas::audio_controls_view(entry.path.clone(), state, thumb)
        }
        media_sort_core::media_type::MediaType::Document => {
            crate::widgets::document_preview::document_preview_view(&entry.path, state)
        }
    };

    let file_size_str = state
        .selected_file_size
        .map(format_file_size)
        .unwrap_or_else(|| "???".to_string());

    let file_info = row![
        container(
            text(&entry.file_name)
                .size(11)
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .width(Length::Fill)
        .clip(true),
        text(file_size_str).size(11),
        button(
            text(char::from(lucide_icons::Icon::FolderOpen))
                .font(iced::Font::with_name("lucide"))
                .size(12)
        )
        .padding([2, 4])
        .on_press(Message::Media(MediaMessage::RevealInExplorer(
            entry.path.clone()
        )))
        .style(iced::widget::button::text),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .padding([4, 6]);

    let file_info_bar =
        container(file_info)
            .width(Length::Fill)
            .style(move |theme: &iced::Theme| {
                let palette = theme.palette();
                iced::widget::container::Style {
                    background: Some(iced::Background::Color(Color {
                        a: 0.35,
                        ..palette.background
                    })),
                    text_color: Some(palette.text),
                    ..iced::widget::container::Style::default()
                }
            });

    container(
        column![
            container(preview_element)
                .width(Length::Fill)
                .height(Length::Fill),
            file_info_bar,
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .padding(4)
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
    .into()
}

fn format_file_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = size as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", size as u64, UNITS[unit_idx])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_file_size_bytes() {
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(1), "1 B");
        assert_eq!(format_file_size(1023), "1023 B");
    }

    #[test]
    fn test_format_file_size_kb() {
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(102400), "100.0 KB");
    }

    #[test]
    fn test_format_file_size_mb() {
        assert_eq!(format_file_size(1048576), "1.0 MB");
        assert_eq!(format_file_size(1572864), "1.5 MB");
    }

    #[test]
    fn test_format_file_size_gb() {
        assert_eq!(format_file_size(1073741824), "1.0 GB");
    }
}
