use std::path::PathBuf;

use iced::Task;
use media_sort_core::media_type::MediaType;

use crate::message::Message;
use crate::state::AppState;

/// Select the filtered media entry at `index` (clamped) and coordinate all
/// per-entry side effects: clearing stale preview state, updating selection
/// and file size, dispatching video/audio/image loading, metadata extraction,
/// grid scroll-into-view, and nearby thumbnail prefetching.
pub fn select_and_load_entry(state: &mut AppState, index: usize) -> Task<Message> {
    let filtered = state.media_grid.filtered_entries();
    let filtered_len = filtered.len();
    if filtered_len == 0 {
        drop(filtered);
        state.media_grid.selected_index = None;
        state.clear_active_preview();
        state.settings.general.last_selected_media = None;
        state.settings.mark_dirty();
        return Task::none();
    }

    let index = index.min(filtered_len - 1);
    let entry = filtered[index];
    let path = entry.path.clone();
    let media_type = entry.media_type;

    let start = index.saturating_sub(5);
    let end = (index + 6).min(filtered_len);
    let thumbnail_meta: Vec<(PathBuf, MediaType, Option<bool>)> = filtered[start..end]
        .iter()
        .map(|entry| (entry.path.clone(), entry.media_type, entry.animated))
        .collect();
    drop(filtered);

    let continue_audio = media_type == MediaType::Audio && state.audio.playing;
    state.clear_active_preview();
    if continue_audio {
        state.audio.playing = true;
    }

    state.cache.media_errors.remove(&path);
    state.media_grid.selected_index = Some(index);
    state.selected_file_size = std::fs::metadata(&path).ok().map(|m| m.len());
    state.settings.general.last_selected_media = Some(path.to_string_lossy().to_string());
    state.settings.mark_dirty();

    // The player itself decides what a selection means: loading a video
    // starts playback, selecting anything else stops and resets it.
    state
        .video
        .select((media_type == MediaType::Video).then(|| path.clone()));

    let audio_task = super::audio::select(
        state,
        (media_type == MediaType::Audio).then_some(path.as_path()),
    );
    let image_task = {
        let filtered = state.media_grid.filtered_entries();
        super::image::select(&mut state.cache, &filtered, index, &path)
    };
    let metadata_task = super::metadata::load_metadata(state, index);
    let scroll_task = super::scroll::scroll_to_selected_entry(state, index);
    let thumbnail_task = super::thumbnails::load_around_selection(state, thumbnail_meta);

    Task::batch([
        audio_task,
        image_task,
        metadata_task,
        scroll_task,
        thumbnail_task,
    ])
}
