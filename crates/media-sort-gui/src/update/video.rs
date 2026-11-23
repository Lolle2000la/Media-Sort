use iced::Task;

use crate::message::Message;
use crate::state::AppState;

pub fn handle_video_message(state: &mut AppState, msg: iced_mpv::PlayerMessage) -> Task<Message> {
    if let Some(event) = state.video.update(msg) {
        match event {
            iced_mpv::VideoEvent::LoadFailed { path, error } => {
                tracing::error!("Video load failed for {}: {}", path.display(), error);
                state.cache.media_errors.record(path, error);
            }
            iced_mpv::VideoEvent::PlayExternally(path) => {
                if let Err(e) = media_sort_backend::platform::open_externally(&path) {
                    tracing::error!("Failed to open file externally: {e}");
                }
            }
            // The first committed frame proves the file is playable, so any
            // previously recorded error (e.g. from thumbnail generation) is
            // dropped.
            iced_mpv::VideoEvent::FrameReady { path } => {
                state.cache.media_errors.remove(&path);
            }
        }
    }
    Task::none()
}
