use std::path::Path;

use iced::widget::{button, column, container, markdown, row, scrollable, text};
use iced::{Alignment, Color, Element, Font, Length};
use media_sort_backend::media::document::{DocumentUnavailableReason, SheetData, SlideData};

use crate::message::{MediaMessage, Message};
use crate::state::AppState;
use crate::state::document::DocumentPreviewData;

const BOLD_FONT: Font = Font {
    weight: iced::font::Weight::Bold,
    ..Font::DEFAULT
};

/// Renders the active document preview pane for `path` from `state.document`.
pub fn document_preview_view<'a>(path: &'a Path, state: &'a AppState) -> Element<'a, Message> {
    if state.document.current_path.as_deref() != Some(path) {
        return centered_message(state.l10n.tr("doc-loading"));
    }

    let Some(data) = &state.document.data else {
        return centered_message(state.l10n.tr("doc-loading"));
    };

    match data {
        DocumentPreviewData::PagedImage {
            handle,
            page_index,
            total_pages,
            ..
        } => {
            let mut indicator = state.l10n.get(
                "doc-page-indicator",
                &[
                    ("current", &(page_index + 1).to_string()),
                    ("total", &total_pages.to_string()),
                ],
            );
            if state.document.loading {
                indicator.push_str(&format!(" ({})", state.l10n.tr("doc-loading")));
            }
            let nav = page_nav_bar(
                state.l10n.tr("doc-prev-page"),
                indicator,
                state.l10n.tr("doc-next-page"),
                *page_index > 0,
                *page_index + 1 < *total_pages,
            );
            column![
                container(
                    iced::widget::image(handle.clone())
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(4),
                nav,
            ]
            .spacing(6)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
        DocumentPreviewData::Markdown {
            items,
            chapter_index,
            total_chapters,
            cover,
        } => {
            let md_settings = markdown::Settings::with_text_size(14, crate::app::theme(state));
            let md_view = markdown::view(items, md_settings)
                .map(|uri| Message::Media(MediaMessage::DocumentLinkClicked(uri)));

            let body: Element<'_, Message> = if let Some(cover_handle) = cover {
                column![
                    container(
                        iced::widget::image(cover_handle.clone()).height(Length::Fixed(220.0))
                    )
                    .center_x(Length::Fill)
                    .padding(iced::Padding::new(0.0).bottom(12.0)),
                    md_view,
                ]
                .spacing(8)
                .width(Length::Fill)
                .into()
            } else {
                md_view
            };

            let content = scrollable(container(body).padding(16).width(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill);

            if let (Some(ch), Some(total)) = (*chapter_index, *total_chapters) {
                let mut indicator = state.l10n.get(
                    "doc-chapter-indicator",
                    &[
                        ("current", &(ch + 1).to_string()),
                        ("total", &total.to_string()),
                    ],
                );
                if state.document.loading {
                    indicator.push_str(&format!(" ({})", state.l10n.tr("doc-loading")));
                }
                let nav = page_nav_bar(
                    state.l10n.tr("doc-prev-page"),
                    indicator,
                    state.l10n.tr("doc-next-page"),
                    ch > 0,
                    ch + 1 < total,
                );
                column![content, nav]
                    .spacing(6)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            } else {
                content.into()
            }
        }
        DocumentPreviewData::Spreadsheet {
            sheets,
            active_sheet,
        } => spreadsheet_view(sheets, *active_sheet, state),
        DocumentPreviewData::Slides {
            slides,
            slide_index,
        } => slides_view(path, slides, *slide_index, state),
        DocumentPreviewData::PlainText {
            text: content,
            truncated,
        } => {
            let text_scroll = scrollable(
                container(
                    text(content)
                        .font(Font::MONOSPACE)
                        .size(13)
                        .shaping(text::Shaping::Advanced)
                        .wrapping(text::Wrapping::WordOrGlyph),
                )
                .padding(14)
                .width(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill);

            if *truncated {
                column![
                    truncation_banner(state.l10n.tr("doc-truncated")),
                    text_scroll
                ]
                .spacing(6)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
            } else {
                text_scroll.into()
            }
        }
        DocumentPreviewData::Unavailable(reason) => {
            let (headline, detail) = match reason {
                DocumentUnavailableReason::Encrypted => {
                    (state.l10n.tr("doc-unavailable-encrypted"), None)
                }
                DocumentUnavailableReason::BinaryOrUnreadable => {
                    (state.l10n.tr("doc-unavailable-binary"), None)
                }
                DocumentUnavailableReason::NoPreviewBackend => {
                    (state.l10n.tr("doc-unavailable-no-backend"), None)
                }
                DocumentUnavailableReason::TooLarge => {
                    (state.l10n.tr("doc-unavailable-too-large"), None)
                }
                DocumentUnavailableReason::UnsupportedOrCorrupt(msg) => (
                    state.l10n.tr("doc-unavailable-corrupt"),
                    (!msg.trim().is_empty()).then_some(msg.as_str()),
                ),
            };
            unavailable_card(path, headline, detail, state.l10n.tr("doc-open-externally"))
        }
        DocumentPreviewData::Error(err) => unavailable_card(
            path,
            state.l10n.tr("doc-unavailable-corrupt"),
            (!err.trim().is_empty()).then_some(err.as_str()),
            state.l10n.tr("doc-open-externally"),
        ),
    }
}

fn spreadsheet_view<'a>(
    sheets: &'a [SheetData],
    active_sheet: usize,
    state: &'a AppState,
) -> Element<'a, Message> {
    if sheets.is_empty() {
        return centered_message(state.l10n.tr("doc-empty-sheet"));
    }

    let selected_idx = active_sheet.min(sheets.len() - 1);
    let sheet = &sheets[selected_idx];

    let tab_buttons = sheets.iter().enumerate().map(|(i, s)| {
        let is_active = i == selected_idx;
        button(text(&s.name).size(11))
            .padding([4, 10])
            .style(if is_active {
                button::primary
            } else {
                button::secondary
            })
            .on_press(Message::Media(MediaMessage::DocumentSelectSheet(i)))
            .into()
    });
    let tabs_bar = container(scrollable(row(tab_buttons).spacing(4)).direction(
        scrollable::Direction::Horizontal(
            scrollable::Scrollbar::default().width(4).scroller_width(4),
        ),
    ))
    .padding([4, 8])
    .width(Length::Fill);

    let body: Element<'_, Message> = if sheet.headers.is_empty() && sheet.rows.is_empty() {
        centered_message(state.l10n.tr("doc-empty-sheet"))
    } else {
        let col_count = sheet
            .headers
            .len()
            .max(sheet.rows.iter().map(Vec::len).max().unwrap_or(0))
            .max(1);
        let mut col_widths = vec![80.0_f32; col_count];
        for (c, hdr) in sheet.headers.iter().enumerate() {
            let w = (hdr.chars().count() as f32 * 8.0 + 20.0).clamp(72.0, 260.0);
            col_widths[c] = col_widths[c].max(w);
        }
        for r in sheet.rows.iter().take(50) {
            for (c, cell) in r.iter().enumerate() {
                let w = (cell.chars().count() as f32 * 7.5 + 16.0).clamp(72.0, 260.0);
                col_widths[c] = col_widths[c].max(w);
            }
        }

        let mut table_rows: Vec<Element<'_, Message>> = Vec::with_capacity(sheet.rows.len() + 1);

        if !sheet.headers.is_empty() {
            let hdr_cells = (0..col_count).map(|c| {
                let val = sheet.headers.get(c).map_or("", String::as_str);
                container(
                    text(val)
                        .size(12)
                        .font(BOLD_FONT)
                        .wrapping(text::Wrapping::None)
                        .shaping(text::Shaping::Advanced),
                )
                .width(Length::Fixed(col_widths[c]))
                .padding([6, 8])
                .clip(true)
                .into()
            });
            table_rows.push(
                container(row(hdr_cells).spacing(0))
                    .style(|theme: &iced::Theme| {
                        let palette = theme.palette();
                        container::Style {
                            background: Some(
                                Color {
                                    a: 0.14,
                                    ..palette.primary
                                }
                                .into(),
                            ),
                            border: iced::Border {
                                width: 1.0,
                                color: Color {
                                    a: 0.2,
                                    ..palette.text
                                },
                                radius: 0.0.into(),
                            },
                            ..Default::default()
                        }
                    })
                    .into(),
            );
        }

        for (r_idx, row_vals) in sheet.rows.iter().enumerate() {
            let is_alt = !r_idx.is_multiple_of(2);
            let cells = (0..col_count).map(|c| {
                let val = row_vals.get(c).map_or("", String::as_str);
                container(
                    text(val)
                        .size(12)
                        .wrapping(text::Wrapping::None)
                        .shaping(text::Shaping::Advanced),
                )
                .width(Length::Fixed(col_widths[c]))
                .padding([5, 8])
                .clip(true)
                .into()
            });
            table_rows.push(
                container(row(cells).spacing(0))
                    .style(move |theme: &iced::Theme| {
                        let palette = theme.palette();
                        container::Style {
                            background: if is_alt {
                                Some(
                                    Color {
                                        a: 0.04,
                                        ..palette.text
                                    }
                                    .into(),
                                )
                            } else {
                                None
                            },
                            border: iced::Border {
                                width: 0.5,
                                color: Color {
                                    a: 0.1,
                                    ..palette.text
                                },
                                radius: 0.0.into(),
                            },
                            ..Default::default()
                        }
                    })
                    .into(),
            );
        }

        let grid = scrollable(column(table_rows).spacing(0))
            .direction(scrollable::Direction::Both {
                vertical: scrollable::Scrollbar::default(),
                horizontal: scrollable::Scrollbar::default(),
            })
            .width(Length::Fill)
            .height(Length::Fill);

        if sheet.truncated {
            column![grid, truncation_banner(state.l10n.tr("doc-truncated"))]
                .spacing(6)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            grid.into()
        }
    };

    column![tabs_bar, body]
        .spacing(4)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn slides_view<'a>(
    path: &'a Path,
    slides: &'a [SlideData],
    slide_index: usize,
    state: &'a AppState,
) -> Element<'a, Message> {
    if slides.is_empty() {
        return unavailable_card(
            path,
            state.l10n.tr("doc-unavailable-corrupt"),
            None,
            state.l10n.tr("doc-open-externally"),
        );
    }

    let idx = slide_index.min(slides.len() - 1);
    let slide = &slides[idx];

    let mut slide_content = column![].spacing(10).width(Length::Fill);
    if let Some(ref title) = slide.title {
        slide_content = slide_content.push(
            text(title)
                .size(18)
                .font(BOLD_FONT)
                .shaping(text::Shaping::Advanced),
        );
    }
    for para in &slide.body_paragraphs {
        slide_content = slide_content.push(text(para).size(14).shaping(text::Shaping::Advanced));
    }

    let card = container(slide_content)
        .padding([20, 24])
        .width(Length::Fill)
        .style(|theme: &iced::Theme| {
            let palette = theme.palette();
            container::Style {
                background: Some(
                    Color {
                        a: 0.05,
                        ..palette.text
                    }
                    .into(),
                ),
                border: iced::Border {
                    color: Color {
                        a: 0.2,
                        ..palette.text
                    },
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            }
        });

    let scroll = scrollable(container(card).padding(16).width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill);

    let indicator = state.l10n.get(
        "doc-slide-indicator",
        &[
            ("current", &(idx + 1).to_string()),
            ("total", &slides.len().to_string()),
        ],
    );
    let nav = page_nav_bar(
        state.l10n.tr("doc-prev-page"),
        indicator,
        state.l10n.tr("doc-next-page"),
        idx > 0,
        idx + 1 < slides.len(),
    );

    column![scroll, nav]
        .spacing(6)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn page_nav_bar<'a>(
    prev_label: String,
    indicator: String,
    next_label: String,
    can_prev: bool,
    can_next: bool,
) -> Element<'a, Message> {
    let prev_btn = button(text(prev_label).size(12))
        .padding([4, 10])
        .style(button::secondary)
        .on_press_maybe(can_prev.then_some(Message::Media(MediaMessage::DocumentPrevPage)));

    let next_btn = button(text(next_label).size(12))
        .padding([4, 10])
        .style(button::secondary)
        .on_press_maybe(can_next.then_some(Message::Media(MediaMessage::DocumentNextPage)));

    container(
        row![prev_btn, text(indicator).size(12), next_btn]
            .spacing(12)
            .align_y(Alignment::Center),
    )
    .center_x(Length::Fill)
    .padding([4, 8])
    .width(Length::Fill)
    .into()
}

fn truncation_banner<'a>(label: String) -> Element<'a, Message> {
    container(
        text(label)
            .size(11)
            .style(|theme: &iced::Theme| text::Style {
                color: Some(theme.palette().warning),
            }),
    )
    .padding([4, 10])
    .width(Length::Fill)
    .style(|theme: &iced::Theme| {
        let palette = theme.palette();
        container::Style {
            background: Some(
                Color {
                    a: 0.12,
                    ..palette.warning
                }
                .into(),
            ),
            border: iced::Border {
                color: Color {
                    a: 0.35,
                    ..palette.warning
                },
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

fn unavailable_card<'a>(
    path: &'a Path,
    headline: String,
    detail: Option<&'a str>,
    open_label: String,
) -> Element<'a, Message> {
    let mut col = column![
        text(char::from(lucide_icons::Icon::FileText))
            .font(Font::with_name("lucide"))
            .size(36)
            .color(Color::from_rgb(0.6, 0.6, 0.65)),
        text(headline).size(14),
    ]
    .spacing(12)
    .align_x(Alignment::Center);

    if let Some(detail_str) = detail {
        col = col.push(
            container(
                text(detail_str)
                    .size(11)
                    .style(|theme: &iced::Theme| text::Style {
                        color: Some(theme.palette().danger),
                    }),
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
        );
    }

    col = col.push(
        button(text(open_label).size(13))
            .padding([6, 14])
            .on_press(Message::Media(MediaMessage::OpenExternal(
                path.to_path_buf(),
            ))),
    );

    container(col)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(20)
        .into()
}

fn centered_message<'a>(msg: String) -> Element<'a, Message> {
    container(text(msg).size(14))
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
