use std::path::PathBuf;

use iced::widget::{column, container, text};
use iced::{Color, Element, Font, Length};
use iced_mpv::{MediaControl, media_controls_view};

use crate::message::{AudioMessage, Message};
use crate::state::AppState;

pub fn audio_controls_view(
    _path: PathBuf,
    state: &AppState,
    thumb: Option<iced::widget::image::Handle>,
) -> Element<'_, Message> {
    let playing = state.audio.playing && !state.audio.player.as_ref().is_none_or(|p| p.is_paused());

    let cover = state.audio.selected_cover.clone().or(thumb);

    let audio_content: Element<'_, Message> = if let Some(handle) = cover {
        iced::widget::image(handle)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        container(
            text(char::from(lucide_icons::Icon::Music))
                .font(Font::with_name("lucide"))
                .size(48)
                .color(Color::from_rgb(0.4, 0.4, 0.45)),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    };

    let controls = media_controls_view(iced_mpv::MediaControlsState {
        position: state.audio.position,
        duration: state.audio.duration,
        volume: state.audio.volume,
        muted: state.audio.muted,
        playing,
    })
    .map(|msg| match msg {
        MediaControl::PlayPause => Message::Audio(AudioMessage::PlayPause),
        MediaControl::Stop => Message::Audio(AudioMessage::Stop),
        MediaControl::Seek(v) => Message::Audio(AudioMessage::Seek(v)),
        MediaControl::SetVolume(v) => Message::Audio(AudioMessage::SetVolume(v)),
        MediaControl::ToggleMute => Message::Audio(AudioMessage::ToggleMute),
    });

    let content = column![
        container(audio_content)
            .width(Length::Fill)
            .height(Length::Fill),
        controls
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
