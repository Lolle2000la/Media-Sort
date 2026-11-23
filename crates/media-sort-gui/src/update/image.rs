use std::path::{Path, PathBuf};

use iced::Task;
use media_sort_core::media_type::MediaType;
use media_sort_core::models::MediaEntry;

use crate::message::{MediaMessage, Message};
use crate::state::{AppState, CacheState};

/// Handle completion of an async preview-resolution image decode.
pub fn handle_image_loaded(
    state: &mut AppState,
    path: PathBuf,
    result: Result<(u32, u32, Vec<u8>), String>,
) -> Task<Message> {
    match result {
        Ok((w, h, pixels)) => {
            state.cache.media_errors.remove(&path);
            let handle = iced::widget::image::Handle::from_rgba(w, h, pixels);
            state.cache.image_cache.push(path.clone(), handle.clone());
            if let Some(idx) = state.media_grid.selected_index {
                let entries = state.media_grid.filtered_entries();
                if let Some(entry) = entries.get(idx)
                    && entry.path == path
                {
                    state.cache.selected_image = Some((path, handle));
                }
            }
        }
        Err(err) => {
            tracing::error!("Failed to load full image: {err}");
            state.cache.media_errors.record(path.clone(), err);
            if let Some(idx) = state.media_grid.selected_index {
                let entries = state.media_grid.filtered_entries();
                if let Some(entry) = entries.get(idx)
                    && entry.path == path
                {
                    state.cache.selected_image = None;
                }
            }
        }
    }
    Task::none()
}

/// Spawn an async preview-resolution image decode off the UI thread.
pub fn load_full_image(path: PathBuf, media_type: MediaType) -> Task<Message> {
    if media_type != MediaType::Image {
        return Task::none();
    }
    Task::perform(
        async move {
            let path_clone = path.clone();
            // Preview is capped at 1920x1440 (no zoom UI exists; iced scales the
            // widget anyway; ~97% less memory per cached preview).
            let res = tokio::task::spawn_blocking(move || {
                media_sort_backend::media::image_decoder::load_preview(&path_clone, 1920, 1440)
                    .map(|d| d.into_parts())
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(format!("Join error: {e}")));
            (path, res)
        },
        |(path, res)| Message::Media(MediaMessage::ImageLoaded(path, res)),
    )
}

/// Populate `selected_image` from cache or schedule decoding for the selected
/// entry, and preload adjacent `Image` entries (`index - 1` and `index + 1`).
pub fn select(
    cache: &mut CacheState,
    filtered: &[&MediaEntry],
    index: usize,
    path: &Path,
) -> Task<Message> {
    let mut tasks = Vec::new();
    let media_type = filtered[index].media_type;

    if let Some(handle) = cache.image_cache.get(path) {
        cache.selected_image = Some((path.to_path_buf(), handle.clone()));
    } else {
        cache.selected_image = None;
        tasks.push(load_full_image(path.to_path_buf(), media_type));
    }

    if index + 1 < filtered.len() {
        let next_entry = filtered[index + 1];
        if next_entry.media_type == MediaType::Image
            && !cache.image_cache.contains(&next_entry.path)
        {
            tasks.push(load_full_image(
                next_entry.path.clone(),
                next_entry.media_type,
            ));
        }
    }
    if index > 0 {
        let prev_entry = filtered[index - 1];
        if prev_entry.media_type == MediaType::Image
            && !cache.image_cache.contains(&prev_entry.path)
        {
            tasks.push(load_full_image(
                prev_entry.path.clone(),
                prev_entry.media_type,
            ));
        }
    }

    Task::batch(tasks)
}

/// Clear the currently displayed preview image handle.
#[allow(dead_code)]
pub fn reset(state: &mut AppState) {
    state.cache.selected_image = None;
}
