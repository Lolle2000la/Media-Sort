use std::path::Path;

use iced::Task;

use crate::message::{AudioMessage, Message};
use crate::state::AppState;

/// Handle audio playback controls and asynchronously decoded cover art.
pub fn handle_audio_message(state: &mut AppState, msg: AudioMessage) -> Task<Message> {
    match msg {
        AudioMessage::PlayPause => {
            if let Some(ref player) = state.audio.player {
                if player.is_paused() && !player.empty() {
                    player.resume();
                    state.audio.playing = true;
                } else if state.audio.playing {
                    player.pause();
                    state.audio.playing = false;
                } else if let Some(index) = state.media_grid.selected_index {
                    let entries = state.media_grid.filtered_entries();
                    if let Some(entry) = entries.get(index) {
                        if let Err(e) = player.play(&entry.path) {
                            tracing::error!("Audio play failed: {e}");
                        } else {
                            state.audio.playing = true;
                            state.audio.duration = player.duration();
                        }
                    }
                }
            }
            Task::none()
        }
        AudioMessage::Stop => {
            if let Some(ref player) = state.audio.player {
                player.stop();
            }
            state.audio.playing = false;
            state.audio.position = 0.0;
            Task::none()
        }
        AudioMessage::Seek(pos) => {
            if let Some(ref player) = state.audio.player
                && let Err(e) = player.seek(pos)
            {
                tracing::error!("Audio seek failed: {e}");
            }
            Task::none()
        }
        AudioMessage::SetVolume(vol) => {
            let clamped = vol.clamp(0.0, 100.0);
            state.audio.volume = clamped;
            if let Some(ref player) = state.audio.player
                && !state.audio.muted
            {
                player.set_volume(clamped as f32 / 100.0);
            }
            Task::none()
        }
        AudioMessage::ToggleMute => {
            state.audio.muted = !state.audio.muted;
            if let Some(ref player) = state.audio.player {
                if state.audio.muted {
                    player.set_volume(0.0);
                } else {
                    player.set_volume(state.audio.volume as f32 / 100.0);
                }
            }
            Task::none()
        }
        AudioMessage::CoverLoaded(path, cover) => {
            let still_selected = state.media_grid.selected_index.is_some_and(|idx| {
                state
                    .media_grid
                    .filtered_entries()
                    .get(idx)
                    .is_some_and(|entry| entry.path == path)
            });
            if still_selected {
                state.audio.selected_cover =
                    cover.map(|(w, h, rgba)| iced::widget::image::Handle::from_rgba(w, h, rgba));
            }
            Task::none()
        }
    }
}

/// Select an audio track (or `None` to stop and reset).
///
/// When `Some(path)` is passed, cover extraction + decoding runs off the UI
/// thread via `tokio::task::spawn_blocking` and delivers
/// [`AudioMessage::CoverLoaded`].
pub fn select(state: &mut AppState, path: Option<&Path>) -> Task<Message> {
    let Some(path) = path else {
        reset(state);
        return Task::none();
    };

    state.audio.position = 0.0;
    state.audio.duration = 0.0;
    state.audio.selected_cover = None;

    if let Some(ref player) = state.audio.player {
        player.stop();
        if state.audio.playing {
            if let Err(e) = player.play(path) {
                tracing::error!("Audio play failed: {e}");
                state.audio.playing = false;
            } else {
                state.audio.duration = player.duration();
            }
        }
    }

    let path_owned = path.to_path_buf();
    Task::perform(
        async move {
            let path_for_decode = path_owned.clone();
            let cover = tokio::task::spawn_blocking(move || {
                let bytes =
                    media_sort_backend::media::thumbnail::extract_audio_cover(&path_for_decode)?;
                let img =
                    media_sort_backend::media::image_decoder::decode_bytes_with_limits(&bytes)
                        .ok()?;
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Some((w, h, rgba.into_raw()))
            })
            .await
            .ok()
            .flatten();
            (path_owned, cover)
        },
        |(p, cover)| Message::Audio(AudioMessage::CoverLoaded(p, cover)),
    )
}

/// Stop playback and clear track progress/cover while preserving volume, mute,
/// and the player handle.
pub fn reset(state: &mut AppState) {
    if let Some(ref player) = state.audio.player {
        player.stop();
    }
    state.audio.playing = false;
    state.audio.position = 0.0;
    state.audio.duration = 0.0;
    state.audio.selected_cover = None;
}
