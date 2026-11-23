use std::collections::BTreeMap;

use iced::Task;
use media_sort_core::media_type::MediaType;

use crate::message::{MediaMessage, Message};
use crate::state::AppState;

pub fn load_metadata(state: &AppState, index: usize) -> Task<Message> {
    let entries = state.media_grid.filtered_entries();
    let Some(entry) = entries.get(index) else {
        return Task::none();
    };

    let path = entry.path.clone();
    let media_type = entry.media_type;

    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                let mut map = media_sort_backend::metadata::extract_metadata(&path, media_type)
                    .map_err(|e| e.to_string())?;
                if media_type == MediaType::Video
                    && let Some(rotation) = iced_mpv::detect_video_rotation(&path)
                    && rotation != iced_mpv::Rotation::R0
                {
                    map.entry("File".to_string()).or_default().insert(
                        "Orientation".to_string(),
                        format!("{}°", rotation.as_degrees()),
                    );
                }
                Ok(map)
            })
            .await
            .unwrap_or_else(|e| Err(format!("Join error: {e}")))
        },
        |result| Message::Media(MediaMessage::MetadataLoaded(result)),
    )
}

pub fn handle_metadata_loaded(
    state: &mut AppState,
    result: Result<BTreeMap<String, BTreeMap<String, String>>, String>,
) -> Task<Message> {
    match result {
        Ok(metadata) => {
            state.metadata.current = Some(metadata);
            Task::none()
        }
        Err(err) => {
            tracing::error!("Metadata load failed: {err}");
            state.metadata.current = None;
            Task::none()
        }
    }
}
