use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use iced::Task;
use media_sort_core::media_type::MediaType;

use crate::message::{MediaMessage, Message};
use crate::state::AppState;
use crate::thumbnails::{KeepPredicate, ThumbnailError, generate_thumbnail_with_options};

pub fn load_visible_thumbnails(state: &mut AppState) -> Task<Message> {
    let filtered = state.media_grid.filtered_entries();
    let entry_paths: Vec<PathBuf> = filtered.iter().map(|e| e.path.clone()).collect();
    let load_queue = state.cache.thumbnail_tracker.update_viewport(
        &state.media_grid.scroll,
        &entry_paths,
        state.settings.window_position.width,
    );

    let entry_meta: HashMap<PathBuf, (MediaType, Option<bool>)> = filtered
        .iter()
        .map(|e| (e.path.clone(), (e.media_type, e.animated)))
        .collect();

    Task::batch(
        load_queue
            .into_iter()
            .filter(|path| {
                !state.cache.thumbnail_cache.contains(path)
                    && !state.cache.media_errors.has_error(path)
            })
            .map(|path| {
                let (media_type, animated) = entry_meta
                    .get(&path)
                    .copied()
                    .unwrap_or((MediaType::Image, None));
                load_thumbnail(
                    path,
                    state.cache.thumbnail_tracker.clone_checker(),
                    media_type,
                    animated,
                )
            }),
    )
}

/// Retain `paths_meta` in the visibility tracker and schedule thumbnail
/// generation for any that are not yet cached.
pub fn load_around_selection(
    state: &mut AppState,
    paths_meta: Vec<(PathBuf, MediaType, Option<bool>)>,
) -> Task<Message> {
    state
        .cache
        .thumbnail_tracker
        .retain_paths(paths_meta.iter().map(|(p, _, _)| p.clone()));

    Task::batch(
        paths_meta
            .into_iter()
            .filter(|(p, _, _)| !state.cache.thumbnail_cache.contains(p))
            .map(|(p, mt, animated)| {
                load_thumbnail(
                    p,
                    state.cache.thumbnail_tracker.clone_checker(),
                    mt,
                    animated,
                )
            }),
    )
}

pub fn load_thumbnail(
    path: PathBuf,
    visible_tracker: Arc<RwLock<HashSet<PathBuf>>>,
    media_type: MediaType,
    animated: Option<bool>,
) -> Task<Message> {
    Task::perform(
        async move {
            let path_clone = path.clone();
            let keep: KeepPredicate =
                Arc::new(move |p| visible_tracker.read().is_ok_and(|guard| guard.contains(p)));
            let result = tokio::task::spawn_blocking(move || {
                generate_thumbnail_with_options(&path_clone, media_type, animated, Some(keep))
            })
            .await
            .unwrap_or(Ok(None));
            (path, result)
        },
        |(path, result)| {
            Message::Media(match result {
                Ok(Some((w, h, rgba))) => MediaMessage::ThumbnailReady(path, w, h, rgba),
                Ok(None) => MediaMessage::ThumbnailCancelled(path),
                Err(err) => MediaMessage::ThumbnailFailed(path, err),
            })
        },
    )
}

pub fn handle_thumbnail_ready(
    state: &mut AppState,
    path: PathBuf,
    w: u32,
    h: u32,
    data: Vec<u8>,
) -> Task<Message> {
    if !data.is_empty() && w > 0 && h > 0 {
        state.cache.media_errors.remove(&path);
        let handle = iced::widget::image::Handle::from_rgba(w, h, data);
        state.cache.thumbnail_cache.push(path, handle);
    }
    Task::none()
}

pub fn handle_thumbnail_failed(
    state: &mut AppState,
    path: PathBuf,
    err: ThumbnailError,
) -> Task<Message> {
    match err {
        ThumbnailError::Transient(rest) => {
            // Transient failure (queue timeout, dead worker pool), not a decode
            // failure: do NOT record it in `media_errors` so the next viewport
            // pass can retry it.
            tracing::debug!(
                "Thumbnail transient failure for {} (retried on next viewport pass): {rest}",
                path.display()
            );
        }
        ThumbnailError::Decode(msg) => {
            state.cache.media_errors.record(path, msg);
        }
    }
    Task::none()
}

pub fn handle_thumbnail_cancelled(_state: &mut AppState, _path: PathBuf) -> Task<Message> {
    Task::none()
}
