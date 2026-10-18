use iced::Task;

use crate::message::{MediaMessage, Message};
use crate::state::AppState;
use crate::view::folder_panel::FOLDER_TREE_SCROLLABLE_ID;
use crate::view::media_grid::{
    MEDIA_GRID_CARD_SPACING, MEDIA_GRID_CARD_WIDTH, MEDIA_GRID_SCROLLABLE_ID,
};
use media_sort_core::media_type::MediaType;

pub fn select_and_load_entry(state: &mut AppState, index: usize) -> Task<Message> {
    let filtered = state.media_grid.filtered_entries();
    let filtered_len = filtered.len();
    if filtered_len > 0 {
        let index = index.min(filtered_len - 1);
        let entry = filtered[index];
        let path = entry.path.clone();
        let media_type = entry.media_type;

        state.cache.media_errors.remove(&path);

        let start = index.saturating_sub(5);
        let end = (index + 6).min(filtered_len);
        let thumbnail_meta: Vec<(
            std::path::PathBuf,
            media_sort_core::media_type::MediaType,
            Option<bool>,
        )> = filtered[start..end]
            .iter()
            .map(|entry| (entry.path.clone(), entry.media_type, entry.animated))
            .collect();
        let thumbnail_paths: Vec<std::path::PathBuf> =
            thumbnail_meta.iter().map(|(p, _, _)| p.clone()).collect();

        let mut preload_tasks = Vec::new();
        if index + 1 < filtered_len {
            let next_entry = filtered[index + 1];
            if next_entry.media_type == media_sort_core::media_type::MediaType::Image
                && !state.cache.image_cache.contains(&next_entry.path)
            {
                preload_tasks.push(load_full_image(
                    next_entry.path.clone(),
                    next_entry.media_type,
                ));
            }
        }
        if index > 0 {
            let prev_entry = filtered[index - 1];
            if prev_entry.media_type == media_sort_core::media_type::MediaType::Image
                && !state.cache.image_cache.contains(&prev_entry.path)
            {
                preload_tasks.push(load_full_image(
                    prev_entry.path.clone(),
                    prev_entry.media_type,
                ));
            }
        }

        drop(filtered);

        state
            .cache
            .thumbnail_tracker
            .retain_paths(thumbnail_paths.clone());

        state.media_grid.selected_index = Some(index);
        state.metadata.current = None;

        state.settings.general.last_selected_media = Some(path.to_string_lossy().to_string());
        state.settings.mark_dirty();

        // The player itself decides what a selection means: loading a video
        // starts playback, selecting anything else stops and resets it.
        state.video.select(
            (media_type == media_sort_core::media_type::MediaType::Video).then(|| path.clone()),
        );

        if media_type == media_sort_core::media_type::MediaType::Video {
            if let Some(ref mut ap) = state.audio.player {
                ap.stop();
            }
            state.audio.playing = false;
            state.audio.position = 0.0;
        } else if media_type == media_sort_core::media_type::MediaType::Audio {
            if state.audio.playing
                && let Some(ref player) = state.audio.player
            {
                player.stop();
                if let Err(e) = player.play(&path) {
                    tracing::error!("Audio play failed: {e}");
                    state.audio.playing = false;
                } else {
                    state.audio.duration = player.duration();
                }
            }
        } else if state.audio.playing {
            if let Some(ref player) = state.audio.player {
                player.stop();
            }
            state.audio.playing = false;
            state.audio.position = 0.0;
        }

        let mut tasks = vec![load_metadata(state, index)];

        tasks.push(scroll_to_selected_entry(state, index));

        state.audio.selected_cover = None;
        if media_type == media_sort_core::media_type::MediaType::Audio
            && let Some(bytes) = media_sort_backend::media::thumbnail::extract_audio_cover(&path)
        {
            let img =
                media_sort_backend::media::image_decoder::decode_bytes_with_limits(&bytes).ok();
            if let Some(img) = img {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                state.audio.selected_cover = Some(iced::widget::image::Handle::from_rgba(
                    w,
                    h,
                    rgba.into_raw(),
                ));
            }
        }

        if let Some(handle) = state.cache.image_cache.get(&path) {
            state.cache.selected_image = Some((path, handle.clone()));
        } else {
            state.cache.selected_image = None;
            tasks.push(load_full_image(path, media_type));
        }

        tasks.extend(preload_tasks);

        tasks.extend(
            thumbnail_meta
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
        );
        Task::batch(tasks)
    } else {
        state.media_grid.selected_index = None;
        state.metadata.current = None;
        state.cache.selected_image = None;
        state.video.select(None);

        state.settings.general.last_selected_media = None;
        state.settings.mark_dirty();
        Task::none()
    }
}

/// Build a [`Task`] that scrolls the media grid so that the entry at `index`
/// is visible within the viewport with a comfortable margin (scroll padding).
///
/// If the card is already fully visible inside the viewport bounds, no scroll task
/// is executed. If it moves near or past the viewport edge, the scroll position
/// adjusts minimally so the item comes into view with margin.
pub fn scroll_to_selected_entry(state: &AppState, index: usize) -> Task<Message> {
    let total = state.media_grid.filtered_entries().len();
    if total <= 1 {
        return Task::none();
    }

    let clamped_index = index.min(total - 1);
    let card_stride = MEDIA_GRID_CARD_WIDTH + MEDIA_GRID_CARD_SPACING;
    let item_left = clamped_index as f32 * card_stride;
    let item_right = item_left + MEDIA_GRID_CARD_WIDTH;

    let scroll = &state.media_grid.scroll;

    let relative_x = if scroll.viewport_width > 0.0 {
        if scroll.content_width <= scroll.viewport_width {
            return Task::none();
        }

        let margin = card_stride * 1.5;
        let Some(target_offset) = calculate_scroll_into_view_1d(
            item_left,
            item_right,
            scroll.offset_x,
            scroll.viewport_width,
            scroll.content_width,
            margin,
        ) else {
            return Task::none();
        };

        let max_offset = scroll.content_width - scroll.viewport_width;
        (target_offset / max_offset).clamp(0.0, 1.0)
    } else {
        let Some(rel) = relative_position_for(clamped_index, total) else {
            return Task::none();
        };
        rel
    };

    iced::widget::operation::snap_to(
        MEDIA_GRID_SCROLLABLE_ID.clone(),
        iced::widget::scrollable::RelativeOffset {
            x: Some(relative_x),
            y: None,
        },
    )
}

/// Computes target 1D scroll offset to keep `[item_start, item_end]` visible within
/// `[current_offset, current_offset + viewport_size]` with at least `margin` padding.
/// Returns `Some(target_offset)` if scrolling is needed, or `None` if the item is
/// already comfortably in view.
pub fn calculate_scroll_into_view_1d(
    item_start: f32,
    item_end: f32,
    current_offset: f32,
    viewport_size: f32,
    content_size: f32,
    margin: f32,
) -> Option<f32> {
    if viewport_size <= 0.0 || content_size <= viewport_size {
        return None;
    }

    let max_offset = (content_size - viewport_size).max(0.0);
    let item_size = (item_end - item_start).max(0.0);
    let effective_margin = margin.min((viewport_size - item_size).max(0.0) / 2.0);

    let view_start = current_offset.clamp(0.0, max_offset);
    let view_end = view_start + viewport_size;

    let target_offset = if item_start - effective_margin < view_start {
        (item_start - effective_margin).max(0.0)
    } else if item_end + effective_margin > view_end {
        (item_end + effective_margin - viewport_size).min(max_offset)
    } else {
        return None;
    };

    if (target_offset - current_offset).abs() > 0.5 {
        Some(target_offset.clamp(0.0, max_offset))
    } else {
        None
    }
}

/// Compute the relative horizontal scroll position (in `[0.0, 1.0]`) that
/// corresponds to `index` in a list of `total` entries. Returns `None` when
/// the list has zero or one entries, in which case there is nothing to
/// scroll to.
pub fn relative_position_for(index: usize, total: usize) -> Option<f32> {
    if total <= 1 {
        return None;
    }
    let clamped_index = index.min(total - 1);
    Some(clamped_index as f32 / (total - 1) as f32)
}

pub fn scroll_to_selected_folder(state: &mut AppState) -> Task<Message> {
    let visible = state.folder.collect_visible_folders();
    let total = visible.len();
    let Some(idx) = state.folder.selected_folder_idx.filter(|i| *i < total) else {
        return Task::none();
    };

    if total <= 1 {
        return Task::none();
    }

    let scroll = &state.folder.scroll;

    let relative_y = if scroll.viewport_height > 0.0 {
        if scroll.content_height <= scroll.viewport_height {
            return Task::none();
        }

        let item_height = scroll.content_height / total as f32;
        let item_top = idx as f32 * item_height;
        let item_bottom = item_top + item_height;
        let margin = (scroll.viewport_height * 0.15).clamp(26.0, 78.0);

        let Some(target_offset) = calculate_scroll_into_view_1d(
            item_top,
            item_bottom,
            scroll.offset_y,
            scroll.viewport_height,
            scroll.content_height,
            margin,
        ) else {
            return Task::none();
        };

        let max_offset = scroll.content_height - scroll.viewport_height;
        (target_offset / max_offset).clamp(0.0, 1.0)
    } else {
        let Some(rel) = relative_position_for(idx, total) else {
            return Task::none();
        };
        rel
    };

    iced::widget::operation::snap_to(
        FOLDER_TREE_SCROLLABLE_ID.clone(),
        iced::widget::scrollable::RelativeOffset {
            x: None,
            y: Some(relative_y),
        },
    )
}

pub fn load_visible_thumbnails(state: &mut AppState) -> Task<Message> {
    let filtered = state.media_grid.filtered_entries();
    let entry_paths: Vec<std::path::PathBuf> = filtered.iter().map(|e| e.path.clone()).collect();
    let load_queue = state.cache.thumbnail_tracker.update_viewport(
        &state.media_grid.scroll,
        &entry_paths,
        state.settings.window_position.width,
    );

    // Per-path metadata lookup so `load_thumbnail` can skip the redundant
    // `detect_media_type(path, false)` + `is_animated_gif` re-discovery
    // inside `prefetch::generate_thumbnail` (the entry already paid for
    // both at scan time). Falls back to `(Image, None)` for paths the
    // filter dropped (shouldn't normally happen — `load_queue` is a subset
    // of `entry_paths`).
    let entry_meta: std::collections::HashMap<
        std::path::PathBuf,
        (media_sort_core::media_type::MediaType, Option<bool>),
    > = filtered
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
                    .unwrap_or((media_sort_core::media_type::MediaType::Image, None));
                load_thumbnail(
                    path,
                    state.cache.thumbnail_tracker.clone_checker(),
                    media_type,
                    animated,
                )
            }),
    )
}

pub fn load_thumbnail(
    path: std::path::PathBuf,
    visible_tracker: std::sync::Arc<
        std::sync::RwLock<std::collections::HashSet<std::path::PathBuf>>,
    >,
    media_type: media_sort_core::media_type::MediaType,
    animated: Option<bool>,
) -> Task<Message> {
    Task::perform(
        async move {
            let path_clone = path.clone();
            let tracker = visible_tracker.clone();
            let result = tokio::task::spawn_blocking(move || {
                if let Ok(guard) = tracker.read()
                    && !guard.contains(&path_clone)
                {
                    return Ok(None);
                }
                crate::subscriptions::prefetch::generate_thumbnail(
                    &path_clone,
                    media_type,
                    animated,
                )
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

pub fn open_externally(path: &std::path::Path) {
    // The default handler for a file is a *host* application, and inside the
    // Flatpak sandbox `xdg-open` reaches it only by accident. Hand the file to
    // the OpenURI portal instead; outside Flatpak it reports `Unavailable` (a
    // web address is not a file, either) and the platform handler below runs as
    // before.
    #[cfg(target_os = "linux")]
    {
        if media_sort_backend::platform::desktop_portal::open_externally(path).is_ok() {
            return;
        }
    }

    // The `open` crate uses ShellExecuteW on Windows (a single typed
    // parameter, no cmd.exe re-tokenization), `open` on macOS and xdg-open
    // on Linux. The previous Windows branch ran `cmd /C start "" <path>`
    // with the path as a raw argv element: cmd.exe re-tokenizes the /C tail,
    // so a filename containing `&`, `|`, `<`, `>` or `^` executed a second
    // command. ShellExecuteW passes the path as-is.
    if let Err(e) = open::that(path) {
        tracing::error!("Failed to open file externally: {e}");
    }
}

pub fn reveal_in_file_manager(path: &std::path::Path) {
    // The file manager is a host application too, and inside Flatpak the
    // session-bus policy blocks `org.freedesktop.FileManager1` outright, so the
    // request goes through the OpenURI portal. It has no select-a-file call, so
    // this opens the containing folder rather than highlighting the entry.
    #[cfg(target_os = "linux")]
    {
        if media_sort_backend::platform::desktop_portal::reveal_in_file_manager(path).is_ok() {
            return;
        }
    }

    let res = if cfg!(target_os = "windows") {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map(|_| ())
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
    } else {
        // Percent-encode everything except RFC 3986 unreserved characters
        // plus '/' and ':' so dbus-send's `array:string:` argument cannot be
        // split or mangled by commas, quotes, ampersands or control chars.
        let mut uri = String::from("file://");
        for ch in path.to_string_lossy().chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~' | '/' | ':') {
                uri.push(ch);
            } else {
                for byte in ch.to_string().as_bytes() {
                    uri.push_str(&format!("%{:02X}", byte));
                }
            }
        }
        let mut success = false;
        if let Ok(output) = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{}", uri),
                "string:",
            ])
            .output()
        {
            success = output.status.success();
        }

        if success {
            Ok(())
        } else if let Some(parent) = path.parent() {
            std::process::Command::new("xdg-open")
                .arg(parent)
                .spawn()
                .map(|_| ())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No parent directory",
            ))
        }
    };
    if let Err(e) = res {
        tracing::error!("Failed to reveal file in file manager: {e}");
    }
}

pub fn load_full_image(path: std::path::PathBuf, media_type: MediaType) -> Task<Message> {
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

pub fn load_metadata(state: &AppState, index: usize) -> Task<Message> {
    let entries = state.media_grid.filtered_entries();
    let Some(entry) = entries.get(index) else {
        return Task::none();
    };

    let path = entry.path.clone();
    let media_type = entry.media_type;

    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || match media_type {
                MediaType::Image => {
                    media_sort_backend::metadata::image_meta::extract_image_metadata(&path)
                        .map_err(|e| e.to_string())
                }
                MediaType::Audio => {
                    media_sort_backend::metadata::audio_meta::extract_audio_metadata(&path)
                        .map_err(|e| e.to_string())
                }
                MediaType::Video => {
                    let mut map =
                        media_sort_backend::metadata::video_meta::extract_video_metadata(&path)
                            .map_err(|e| e.to_string())?;
                    // Orientation detection lives in iced-mpv (mp4 tkhd / EXIF /
                    // mp4ameta); the GUI only displays the result.
                    if let Some(rotation) = iced_mpv::detect_video_rotation(&path)
                        && rotation != iced_mpv::Rotation::R0
                    {
                        map.entry("File".to_string()).or_default().insert(
                            "Orientation".to_string(),
                            format!("{}°", rotation.as_degrees()),
                        );
                    }
                    Ok(map)
                }
            })
            .await
            .unwrap_or_else(|e| Err(format!("Join error: {e}")))
        },
        |result| Message::Media(MediaMessage::MetadataLoaded(result)),
    )
}
