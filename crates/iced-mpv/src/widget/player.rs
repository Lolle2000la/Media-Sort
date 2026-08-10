use crate::action::VideoAction;
use crate::state::VideoState;
use crate::widget::controls::{MediaControl, MediaControlsState, media_controls_view};
use crate::widget::shader::{video_shader_view, video_zero_copy_shader_view};
use iced::widget::{column, container};
use iced::{Element, Length};

/// The composite video player widget: the rendered frame (or thumbnail /
/// placeholder) on top, the transport controls below.
///
/// - `state` — the app's [`VideoState`]
/// - `thumb_handle` — a thumbnail shown while no frame is ready yet
/// - `placeholder` — a custom element shown when neither a frame nor a
///   thumbnail is available (falls back to a "Loading video..." label)
/// - `on_action` — maps user interaction ([`VideoAction`]) to your message
///
/// The transport controls are only rendered once the worker is connected
/// ([`VideoState::is_connected`]); until then the placeholder/thumbnail area
/// is shown without a dead-looking control bar.
pub fn video_player_view<'a, Message: 'a, F>(
    state: &VideoState,
    thumb_handle: Option<iced::widget::image::Handle>,
    placeholder: Option<Element<'a, Message>>,
    on_action: F,
) -> Element<'a, Message>
where
    F: Fn(VideoAction) -> Message + 'a + Clone,
{
    let video_content: Element<'_, Message> = if let Some(frame) = state.gpu_frame() {
        video_zero_copy_shader_view(
            state.width(),
            state.height(),
            state.rotation(),
            Some(frame.clone()),
        )
    } else if state.rgba().is_some() {
        video_shader_view(
            state.width(),
            state.height(),
            state.rotation(),
            state.rgba().cloned(),
        )
    } else if let Some(handle) = thumb_handle {
        iced::widget::image(handle)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else if let Some(ph) = placeholder {
        ph
    } else {
        container(iced::widget::text("Loading video..."))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };

    if !state.is_connected() {
        return container(video_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    let mut controls = MediaControlsState::from(state);
    controls.position = state.seek_position().unwrap_or(controls.position);

    let on_action_clone = on_action.clone();
    let controls_row = media_controls_view(controls).map(move |msg| match msg {
        MediaControl::PlayPause => on_action_clone(VideoAction::PlayPause),
        MediaControl::Stop => on_action_clone(VideoAction::Stop),
        MediaControl::Seek(v) => on_action_clone(VideoAction::Seek(v)),
        MediaControl::SetVolume(v) => on_action_clone(VideoAction::SetVolume(v)),
        MediaControl::ToggleMute => on_action_clone(VideoAction::ToggleMute),
    });

    column![
        container(video_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
        controls_row
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
