use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Alignment, Color, Element, Length};

use crate::message::{MediaMessage, Message};
use crate::state::AppState;

/// Identifier used to programmatically scroll the media grid's horizontal
/// scrollable (e.g. to bring the currently selected entry into view).
pub static MEDIA_GRID_SCROLLABLE_ID: iced::widget::Id =
    iced::widget::Id::new("media_grid_scrollable");

/// Width of a single thumbnail card in the media grid.
pub const MEDIA_GRID_CARD_WIDTH: f32 = 60.0;
/// Horizontal spacing between adjacent thumbnail cards in the media grid row.
pub const MEDIA_GRID_CARD_SPACING: f32 = 8.0;
/// Extra vertical space appended at the bottom of the media grid so that the
/// horizontal scrollbar does not overlap with the file names below each
/// thumbnail.
const MEDIA_GRID_SCROLLBAR_CLEARANCE: f32 = 12.0;

/// The half-open index range `[start, end)` of cards to render for the
/// given scroll snapshot. Both bounds are clamped to `total` so a STALE
/// snapshot (the `on_scroll` callback has not fired yet for the current
/// content — e.g. right after switching to a smaller folder, or when a
/// search query shrinks `filtered`) can never slice past the entry list.
/// Shared with `thumbnail_tracker::update_viewport`.
pub fn viewport_window(offset_x: f32, viewport_width: f32, total: usize) -> (usize, usize) {
    const CARD_STRIDE: f32 = MEDIA_GRID_CARD_WIDTH + MEDIA_GRID_CARD_SPACING;
    let s = ((offset_x / CARD_STRIDE).floor() as usize).min(total);
    let e = ((offset_x + viewport_width) / CARD_STRIDE).ceil() as usize;
    (s.saturating_sub(5), (e + 5).min(total))
}

pub fn media_grid_view(state: &AppState) -> Element<'_, Message> {
    let filtered = state.media_grid.filtered_entries();

    if filtered.is_empty() {
        return container(
            text(
                if state.media_grid.entries.is_empty() && state.media_grid.search.query.is_empty() {
                    state.l10n.tr("ui-no-images")
                } else {
                    state.l10n.tr("ui-no-results")
                },
            )
            .size(14),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into();
    }

    // Left navigation button
    let prev_btn = container(
        button(
            text(char::from(lucide_icons::Icon::ChevronLeft))
                .font(iced::Font::with_name("lucide"))
                .size(16),
        )
        .on_press(Message::Media(MediaMessage::GoLeft)),
    )
    .id(iced::widget::Id::new("prev_btn"));

    // Right navigation button
    let next_btn = container(
        button(
            text(char::from(lucide_icons::Icon::ChevronRight))
                .font(iced::Font::with_name("lucide"))
                .size(16),
        )
        .on_press(Message::Media(MediaMessage::GoRight)),
    )
    .id(iced::widget::Id::new("next_btn"));

    let mut entries_row = row![].spacing(MEDIA_GRID_CARD_SPACING);

    let total = filtered.len();
    const CARD_STRIDE: f32 = MEDIA_GRID_CARD_WIDTH + MEDIA_GRID_CARD_SPACING;
    // Virtualize: render only the windows of cards that fall within the
    // current scroll viewport (±5 cards of buffer, mirroring
    // `thumbnail_tracker::update_viewport`). Pad the leading / trailing
    // gaps with `space()` of the same total width so the scrollable's
    // content layout — and therefore the scroll offset math — stays
    // identical to the un-virtualized version. While the viewport_width
    // snapshot is still 0 (before the first `on_scroll` callback fires),
    // render the full list so the initial frame is never blank.
    let (start, end) = if state.media_grid.scroll.viewport_width > 0.0 {
        viewport_window(
            state.media_grid.scroll.offset_x,
            state.media_grid.scroll.viewport_width,
            total,
        )
    } else {
        (0, total)
    };

    if start > 0 {
        // Subtract one card spacing: the row's `spacing` inserts
        // `MEDIA_GRID_CARD_SPACING` between this spacer and the first
        // rendered card, so the card lands at `start * CARD_STRIDE`
        // exactly matching its un-virtualized position.
        let pad = (start as f32 * CARD_STRIDE) - MEDIA_GRID_CARD_SPACING;
        entries_row = entries_row.push(space().width(Length::Fixed(pad)));
    }

    for (local_i, entry) in filtered[start..end].iter().enumerate() {
        let i = local_i + start;
        let is_selected = state.media_grid.selected_index == Some(i);

        let thumbnail_content: Element<'_, Message> =
            if let Some(handle) = state.cache.thumbnail_cache.peek(&entry.path) {
                let img = iced::widget::image(handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill);

                let maybe_badge = match entry.media_type {
                    media_sort_core::media_type::MediaType::Video => {
                        Some((lucide_icons::Icon::Film, 16.0, 0.8))
                    }
                    media_sort_core::media_type::MediaType::Audio => {
                        Some((lucide_icons::Icon::Music, 16.0, 0.8))
                    }
                    media_sort_core::media_type::MediaType::Document => {
                        Some((document_icon(&entry.path), 11.0, 0.7))
                    }
                    media_sort_core::media_type::MediaType::Image => None,
                };

                if let Some((icon, icon_size, bg_alpha)) = maybe_badge {
                    let icon_overlay = container(
                        text(char::from(icon))
                            .font(iced::Font::with_name("lucide"))
                            .size(icon_size)
                            .color(Color::from_rgb(0.95, 0.95, 0.95)),
                    )
                    .padding([2, 4])
                    .style(move |_theme| iced::widget::container::Style {
                        background: Some(iced::Background::Color(Color::from_rgba(
                            0.0, 0.0, 0.0, bg_alpha,
                        ))),
                        border: iced::Border {
                            radius: 2.0.into(),
                            width: 0.0,
                            color: Color::TRANSPARENT,
                        },
                        ..iced::widget::container::Style::default()
                    });

                    let overlay = container(icon_overlay)
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .align_x(Alignment::End)
                        .align_y(Alignment::Start)
                        .padding(2);

                    iced::widget::stack![img, overlay].into()
                } else {
                    img.into()
                }
            } else {
                let fallback_icon = match entry.media_type {
                    media_sort_core::media_type::MediaType::Audio => lucide_icons::Icon::Music,
                    media_sort_core::media_type::MediaType::Video => lucide_icons::Icon::Film,
                    media_sort_core::media_type::MediaType::Image => lucide_icons::Icon::Image,
                    media_sort_core::media_type::MediaType::Document => document_icon(&entry.path),
                };

                text(char::from(fallback_icon))
                    .font(iced::Font::with_name("lucide"))
                    .size(24)
                    .color(Color::from_rgb(0.5, 0.5, 0.6))
                    .into()
            };

        let thumbnail_stacked: Element<'_, Message> = if is_selected {
            let check_badge = container(
                text(char::from(lucide_icons::Icon::Check))
                    .font(iced::Font::with_name("lucide"))
                    .size(10)
                    .color(Color::WHITE),
            )
            .padding([2, 3])
            .style(|theme: &iced::Theme| {
                let primary = theme.palette().primary;
                iced::widget::container::Style {
                    background: Some(iced::Background::Color(primary)),
                    border: iced::Border {
                        radius: 3.0.into(),
                        width: 1.0,
                        color: Color::WHITE,
                    },
                    shadow: iced::Shadow {
                        offset: iced::Vector::new(0.0, 1.0),
                        blur_radius: 3.0,
                        color: Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                    },
                    ..iced::widget::container::Style::default()
                }
            });

            let badge_overlay = container(check_badge)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Start)
                .align_y(Alignment::Start)
                .padding(2);

            iced::widget::stack![thumbnail_content, badge_overlay].into()
        } else {
            thumbnail_content
        };

        let thumbnail = container(thumbnail_stacked)
            .center_x(MEDIA_GRID_CARD_WIDTH)
            .center_y(50)
            .width(Length::Fixed(MEDIA_GRID_CARD_WIDTH))
            .height(Length::Fixed(50.0))
            .padding(if is_selected { 2 } else { 0 })
            .clip(true)
            .style(move |theme: &iced::Theme| {
                let palette = theme.palette();
                let primary = palette.primary;
                iced::widget::container::Style {
                    background: Some(iced::Background::Color(if is_selected {
                        primary
                    } else {
                        Color::from_rgb(0.08, 0.08, 0.1)
                    })),
                    border: iced::Border {
                        radius: 4.0.into(),
                        width: if is_selected { 2.5 } else { 1.0 },
                        color: if is_selected {
                            primary
                        } else {
                            Color::from_rgb(0.2, 0.2, 0.25)
                        },
                    },
                    shadow: if is_selected {
                        iced::Shadow {
                            offset: iced::Vector::new(0.0, 0.0),
                            blur_radius: 6.0,
                            color: Color { a: 0.6, ..primary },
                        }
                    } else {
                        iced::Shadow::default()
                    },
                    ..iced::widget::container::Style::default()
                }
            });

        let file_name = text(&entry.file_name)
            .size(10)
            .shaping(iced::widget::text::Shaping::Advanced)
            .wrapping(iced::widget::text::Wrapping::Glyph);

        let file_name_container = container(file_name)
            .width(Length::Fill)
            .height(Length::Fixed(26.0))
            .align_x(Alignment::Center)
            .padding(if is_selected { [2, 4] } else { [0, 0] })
            .clip(true)
            .style(move |theme: &iced::Theme| {
                let palette = theme.palette();
                if is_selected {
                    iced::widget::container::Style {
                        background: Some(iced::Background::Color(palette.primary)),
                        text_color: Some(Color::WHITE),
                        border: iced::Border {
                            radius: 4.0.into(),
                            width: 0.0,
                            color: Color::TRANSPARENT,
                        },
                        ..iced::widget::container::Style::default()
                    }
                } else {
                    iced::widget::container::Style::default()
                }
            });

        let card = column![thumbnail, file_name_container]
            .align_x(Alignment::Center)
            .spacing(2)
            .width(Length::Fixed(MEDIA_GRID_CARD_WIDTH));

        let entry_button = container(
            button(card)
                .on_press(Message::Media(MediaMessage::SelectEntry(i)))
                .padding(0)
                .style(iced::widget::button::text),
        );
        #[cfg(feature = "demo")]
        let entry_button = entry_button.id(iced_automation::static_widget_id(format!(
            "media_card_{}",
            i
        )));

        entries_row = entries_row.push(entry_button);
    }

    if end < total {
        // Subtract one card spacing to compensate for the gap the row
        // inserts between the last rendered card and this trailing
        // spacer, keeping the total content width identical to the
        // un-virtualized layout.
        let pad = ((total - end) as f32 * CARD_STRIDE) - MEDIA_GRID_CARD_SPACING;
        entries_row = entries_row.push(space().width(Length::Fixed(pad)));
    }

    // Wrap the row of cards in a column with empty space at the bottom. This
    // pushes the file names (which sit below each thumbnail) clear of the
    // horizontal scrollbar, so the bar can no longer cover them.
    let scrollable_content = column![
        entries_row,
        space().height(Length::Fixed(MEDIA_GRID_SCROLLBAR_CLEARANCE))
    ];

    let scrollable_row = scrollable(scrollable_content)
        .id(MEDIA_GRID_SCROLLABLE_ID.clone())
        .direction(iced::widget::scrollable::Direction::Horizontal(
            iced::widget::scrollable::Scrollbar::default(),
        ))
        .on_scroll(|viewport| {
            let offset = viewport.absolute_offset();
            Message::Media(MediaMessage::GridScrolled(
                offset,
                viewport.bounds().width,
                viewport.content_bounds().width,
            ))
        });

    container(
        row![
            prev_btn,
            container(scrollable_row).width(Length::Fill),
            next_btn
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .into()
}

fn document_icon(path: &std::path::Path) -> lucide_icons::Icon {
    use media_sort_core::media_type::DocumentKind;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Pdf) => lucide_icons::Icon::FileText,
        Some(DocumentKind::Comic) => lucide_icons::Icon::FileArchive,
        Some(DocumentKind::Epub) => lucide_icons::Icon::BookOpen,
        Some(DocumentKind::Markdown) => lucide_icons::Icon::FileCode,
        Some(DocumentKind::PlainText) => {
            if matches!(ext.as_str(), "csv" | "tsv") {
                lucide_icons::Icon::FileSpreadsheet
            } else {
                lucide_icons::Icon::FileText
            }
        }
        None => lucide_icons::Icon::FileText,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIDE: f32 = MEDIA_GRID_CARD_WIDTH + MEDIA_GRID_CARD_SPACING;

    #[test]
    fn test_viewport_window_normal_range() {
        // offset = card 100, viewport ~6 cards wide -> buffer of 5 both sides.
        let (start, end) = viewport_window(100.0 * STRIDE, 6.0 * STRIDE, 1000);
        assert_eq!(start, 95);
        assert_eq!(end, 111);
        assert!(start < end);
    }

    #[test]
    fn test_viewport_window_clamped_to_zero() {
        let (start, end) = viewport_window(0.0, 10.0 * STRIDE, 100);
        assert_eq!(start, 0);
        assert_eq!(end, 15);
    }

    #[test]
    fn test_viewport_window_stale_offset_beyond_small_list() {
        // Regression: switching from a ~4400-card folder to an 8-card one
        // leaves a stale offset_x snapshot until the next GridScrolled
        // event. The window must clamp to `total` instead of panicking on
        // `filtered[4399..]`.
        let (start, end) = viewport_window(4404.0 * STRIDE, 6.0 * STRIDE, 8);
        assert_eq!(start, 3);
        assert_eq!(end, 8);
        assert!(start <= end, "window must be a valid slice range");
    }

    #[test]
    fn test_viewport_window_offset_exactly_at_end() {
        let (start, end) = viewport_window(8.0 * STRIDE, 6.0 * STRIDE, 8);
        assert_eq!(start, 3);
        assert_eq!(end, 8);
    }

    #[test]
    fn test_viewport_window_search_shrink() {
        // A query that shrinks the filtered list must not panic either.
        let (start, end) = viewport_window(200.0 * STRIDE, 8.0 * STRIDE, 12);
        assert_eq!(start, 7);
        assert_eq!(end, 12);
    }

    #[test]
    fn test_viewport_window_zero_total() {
        let (start, end) = viewport_window(50.0 * STRIDE, 5.0 * STRIDE, 0);
        assert_eq!(start, 0);
        assert_eq!(end, 0);
    }
}
