use iced::Task;

use crate::message::{MediaMessage, Message};
use crate::state::AppState;
use media_sort_core::actions::copy_action::CopyAction;
use media_sort_core::actions::move_action::MoveAction;
use media_sort_core::actions::rename_action::RenameAction;
use media_sort_core::actions::reversible::{ActionError, ReversibleAction};

pub fn handle_media_message(state: &mut AppState, msg: MediaMessage) -> Task<Message> {
    match msg {
        MediaMessage::SelectEntry(index) => super::tasks::select_and_load_entry(state, index),
        MediaMessage::SearchQueryChanged(query) => {
            let previously_selected_path = state.media_grid.selected_index.and_then(|idx| {
                state
                    .media_grid
                    .filtered_entries()
                    .get(idx)
                    .map(|entry| entry.path.clone())
            });

            state.media_grid.search.query = query;
            state.media_grid.search.focused = true;

            let filtered = state.media_grid.filtered_entries();
            if filtered.is_empty() {
                drop(filtered);
                state.media_grid.selected_index = None;
                state.metadata.current = None;
                state.cache.selected_image = None;
                state.video.select(None);
                state.document.reset();
                Task::none()
            } else {
                let target_index = previously_selected_path
                    .and_then(|prev_path| filtered.iter().position(|entry| entry.path == prev_path))
                    .unwrap_or(0);
                super::tasks::select_and_load_entry(state, target_index)
            }
        }
        MediaMessage::MoveToFolder(target_folder) => {
            if let Some(index) = state.media_grid.selected_index {
                let filtered = state.media_grid.filtered_entries();
                if let Some(entry) = filtered.get(index) {
                    let entry_path = entry.path.clone();
                    match MoveAction::new(&entry_path, &target_folder) {
                        Ok(mut action) => {
                            if let Err(e) = action.execute() {
                                tracing::error!("Move failed: {e}");
                            } else {
                                state.history.push_executed(Box::new(action));
                                state.media_grid.entries.retain(|e| e.path != entry_path);
                                state.media_grid.rebuild_lower_names();
                                return super::tasks::select_and_load_entry(state, index);
                            }
                        }
                        Err(e) => {
                            report_action_error(state, e);
                        }
                    }
                }
            }
            Task::none()
        }
        MediaMessage::DeleteEntry(path) => {
            let index_to_select = state.media_grid.selected_index.unwrap_or(0);
            // Entries are scanned from the canonicalized current folder, so
            // the retain comparison must use the canonicalized path. Resolve
            // BEFORE deleting: afterwards canonicalize() fails because the
            // file is gone. Symlink sources are deliberately NOT resolved —
            // resolving would bypass the trash layer's symlink refusal and
            // act on the link's target.
            let canonical_path = if media_sort_core::path_utils::is_symlink(&path) {
                path.clone()
            } else {
                path.canonicalize().unwrap_or(path)
            };
            match media_sort_backend::filesystem::trash::delete_to_trash(&canonical_path) {
                Ok(handle) => {
                    let action = media_sort_core::actions::delete_action::DeleteAction::new(
                        &canonical_path,
                        handle,
                    );
                    state.history.push_executed(Box::new(action));
                    state
                        .media_grid
                        .entries
                        .retain(|e| e.path != canonical_path);
                    state.media_grid.rebuild_lower_names();
                    return super::tasks::select_and_load_entry(state, index_to_select);
                }
                Err(e) => {
                    tracing::error!("Cannot delete to trash: {e}");
                }
            }
            Task::none()
        }
        MediaMessage::TriggerRename => {
            if let Some(index) = state.media_grid.selected_index {
                let filtered = state.media_grid.filtered_entries();
                if let Some(entry) = filtered.get(index) {
                    let stem = entry
                        .path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    state.rename.path = Some(entry.path.clone());
                    state.rename.input_value = stem;
                    state.rename.error = None;
                }
            }
            Task::none()
        }
        MediaMessage::CopyToFolder(target_folder) => {
            let Some(index) = state.media_grid.selected_index else {
                return Task::none();
            };
            let filtered = state.media_grid.filtered_entries();
            let Some(entry) = filtered.get(index) else {
                return Task::none();
            };
            match CopyAction::new(&entry.path, &target_folder) {
                Ok(mut action) => {
                    if let Err(e) = action.execute() {
                        tracing::error!("Copy failed: {e}");
                    } else {
                        state.history.push_executed(Box::new(action));
                    }
                }
                Err(e) => {
                    report_action_error(state, e);
                }
            }
            Task::none()
        }
        MediaMessage::RenameEntry(path, new_name) => {
            // Entries are scanned from the canonicalized current folder, so
            // the position lookup must compare against the canonicalized
            // source path — a non-canonical alias (e.g. macOS /var vs
            // /private/var) would otherwise miss the entry and leave the
            // stale path in the grid. Resolve BEFORE the rename: after
            // execute() the old path no longer exists and canonicalize()
            // would fail. Symlink sources are deliberately NOT resolved —
            // resolving would bypass RenameAction's SourceIsSymlink refusal
            // and rename the link's target.
            let canonical_path = if media_sort_core::path_utils::is_symlink(&path) {
                path.clone()
            } else {
                path.canonicalize().unwrap_or(path)
            };
            match RenameAction::new(&canonical_path, &new_name) {
                Ok(mut action) => {
                    if let Err(e) = action.execute() {
                        tracing::error!("Rename failed: {e}");
                    } else {
                        state.rename.error = None;
                        let new_path = action.new_path().to_path_buf();
                        state.history.push_executed(Box::new(action));
                        if let Some(pos) = state
                            .media_grid
                            .entries
                            .iter()
                            .position(|e| e.path == canonical_path)
                        {
                            state.media_grid.entries[pos].path = new_path.clone();
                            state.media_grid.entries[pos].file_name = new_path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| new_path.display().to_string());
                            state.rename.path = None;
                            state.rename.input_value.clear();
                            return super::tasks::select_and_load_entry(state, pos);
                        }
                    }
                }
                Err(e) => match &e {
                    ActionError::IllegalCharacters { character, .. } => {
                        state.rename.error = Some(state.l10n.get(
                            "ui-rename-illegal-char",
                            &[("char", &character.to_string())],
                        ));
                        tracing::error!("Cannot create rename action: {e}");
                    }
                    _ => report_action_error(state, e),
                },
            }
            Task::none()
        }
        MediaMessage::RenameInputChanged(val) => {
            let trimmed = val.trim().to_string();
            let error = RenameAction::validate_stem(&trimmed).err().map(|e| {
                match &e {
                    ActionError::IllegalCharacters { character, .. } if *character == '\0' => {
                        // Empty stem — no message, just disable submit button
                        String::new()
                    }
                    ActionError::IllegalCharacters { character, .. } => state.l10n.get(
                        "ui-rename-illegal-char",
                        &[("char", &character.to_string())],
                    ),
                    _ => e.to_string(),
                }
            });
            state.rename.input_value = val;
            state.rename.error = error;
            Task::none()
        }
        MediaMessage::SubmitRename => {
            let new_name = state.rename.input_value.trim().to_string();
            // Guard: don't submit empty or invalid stems
            if new_name.is_empty() || RenameAction::validate_stem(&new_name).is_err() {
                return Task::none();
            }
            if let Some(path) = state.rename.path.take() {
                state.rename.input_value.clear();
                return Task::done(Message::Media(MediaMessage::RenameEntry(path, new_name)));
            }
            Task::none()
        }
        MediaMessage::CancelRename => {
            state.rename.path = None;
            state.rename.input_value.clear();
            state.rename.error = None;
            Task::none()
        }
        MediaMessage::Undo => {
            let index = state.media_grid.selected_index.unwrap_or(0);
            if let Err(e) = state.history.undo() {
                tracing::error!("Undo failed: {e}");
                Task::none()
            } else {
                state.start_async_media_scan(index);
                Task::none()
            }
        }
        MediaMessage::Redo => {
            let index = state.media_grid.selected_index.unwrap_or(0);
            if let Err(e) = state.history.redo() {
                tracing::error!("Redo failed: {e}");
                Task::none()
            } else {
                state.start_async_media_scan(index);
                Task::none()
            }
        }
        MediaMessage::MetadataLoaded(result) => match result {
            Ok(metadata) => {
                state.metadata.current = Some(metadata);
                Task::none()
            }
            Err(err) => {
                tracing::error!("Metadata load failed: {err}");
                state.metadata.current = None;
                Task::none()
            }
        },
        MediaMessage::ThumbnailReady(path, w, h, data) => {
            state.cache.thumbnails_in_flight.remove(&path);
            if !data.is_empty() && w > 0 && h > 0 {
                state.cache.media_errors.remove(&path);
                let handle = iced::widget::image::Handle::from_rgba(w, h, data);
                state.cache.thumbnail_cache.push(path, handle);
            }
            Task::none()
        }
        MediaMessage::ThumbnailFailed(path, err) => {
            state.cache.thumbnails_in_flight.remove(&path);
            match err {
                crate::subscriptions::prefetch::ThumbnailError::Transient(rest) => {
                    // Transient failure (queue timeout, dead worker pool),
                    // not a decode failure: do NOT record it in
                    // `media_errors`. `load_visible_thumbnails` filters on
                    // `media_errors.has_error` (tasks.rs), so a recorded
                    // path would be skipped for the whole folder session
                    // with a permanent "failed" badge and never retried.
                    // Left unrecorded, the card simply stays without a
                    // thumbnail and the next viewport change / scroll pass
                    // re-queues it through `load_visible_thumbnails`.
                    tracing::debug!(
                        "Thumbnail transient failure for {} (retried on next viewport pass): {rest}",
                        path.display()
                    );
                }
                crate::subscriptions::prefetch::ThumbnailError::Decode(msg) => {
                    state.cache.media_errors.record(path, msg);
                }
            }
            Task::none()
        }
        MediaMessage::ThumbnailCancelled(path) => {
            state.cache.thumbnails_in_flight.remove(&path);
            Task::none()
        }
        MediaMessage::OpenExternal(path) => {
            super::tasks::open_externally(&path);
            Task::none()
        }
        MediaMessage::RevealInExplorer(path) => {
            super::tasks::reveal_in_file_manager(&path);
            Task::none()
        }
        MediaMessage::ImageLoaded(path, result) => {
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
        MediaMessage::GoLeft => {
            if let Some(idx) = state.media_grid.selected_index
                && idx > 0
            {
                return super::tasks::select_and_load_entry(state, idx - 1);
            }
            Task::none()
        }
        MediaMessage::GoRight => {
            if let Some(idx) = state.media_grid.selected_index {
                let filtered_len = state.media_grid.filtered_entries().len();
                if idx + 1 < filtered_len {
                    return super::tasks::select_and_load_entry(state, idx + 1);
                }
            }
            Task::none()
        }
        MediaMessage::MoveActive => {
            if let Some(index) = state.media_grid.selected_index
                && let Some(ref target_folder) = state.folder.selected_folder
            {
                let filtered = state.media_grid.filtered_entries();
                if let Some(entry) = filtered.get(index) {
                    let entry_path = entry.path.clone();
                    match MoveAction::new(&entry_path, target_folder) {
                        Ok(mut action) => {
                            if let Err(e) = action.execute() {
                                tracing::error!("Move failed: {e}");
                            } else {
                                state.history.push_executed(Box::new(action));
                                state.media_grid.entries.retain(|e| e.path != entry_path);
                                state.media_grid.rebuild_lower_names();
                                return super::tasks::select_and_load_entry(state, index);
                            }
                        }
                        Err(e) => {
                            report_action_error(state, e);
                        }
                    }
                }
            }
            Task::none()
        }
        MediaMessage::CopyActive => {
            if let Some(index) = state.media_grid.selected_index
                && let Some(ref target_folder) = state.folder.selected_folder
            {
                let filtered = state.media_grid.filtered_entries();
                if let Some(entry) = filtered.get(index) {
                    match CopyAction::new(&entry.path, target_folder) {
                        Ok(mut action) => {
                            if let Err(e) = action.execute() {
                                tracing::error!("Copy failed: {e}");
                            } else {
                                state.history.push_executed(Box::new(action));
                            }
                        }
                        Err(e) => {
                            report_action_error(state, e);
                        }
                    }
                }
            }
            Task::none()
        }
        MediaMessage::SearchFocused => {
            state.media_grid.search.focused = true;
            iced::widget::operation::focus(crate::view::search_bar::SEARCH_INPUT_ID.clone())
        }
        MediaMessage::SearchBlurred => {
            state.media_grid.search.focused = false;
            iced::advanced::widget::operate(iced::advanced::widget::operation::focusable::unfocus())
        }
        MediaMessage::GridScrolled(offset, viewport_width, content_width) => {
            state.media_grid.scroll.offset_x = offset.x;
            state.media_grid.scroll.viewport_width = viewport_width;
            state.media_grid.scroll.content_width = content_width;

            state.cache.thumbnail_tracker.handle_scroll();
            Task::none()
        }
        MediaMessage::AudioPlayPause => {
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
        MediaMessage::StopAudio => {
            if let Some(ref player) = state.audio.player {
                player.stop();
            }
            state.audio.playing = false;
            state.audio.position = 0.0;
            Task::none()
        }
        MediaMessage::AudioSeek(pos) => {
            if let Some(ref player) = state.audio.player
                && let Err(e) = player.seek(pos)
            {
                tracing::error!("Audio seek failed: {e}");
            }
            Task::none()
        }
        MediaMessage::AudioSetVolume(vol) => {
            if let Some(ref player) = state.audio.player {
                player.set_volume(vol as f32 / 100.0);
            }
            state.audio.volume = vol;
            Task::none()
        }
        MediaMessage::AudioToggleMute => {
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
        MediaMessage::ToggleTypeFilter(ty) => {
            let previously_selected_path = state.media_grid.selected_index.and_then(|idx| {
                state
                    .media_grid
                    .filtered_entries()
                    .get(idx)
                    .map(|entry| entry.path.clone())
            });

            state.media_grid.type_filter.toggle(ty);

            let filtered = state.media_grid.filtered_entries();
            if filtered.is_empty() {
                drop(filtered);
                state.media_grid.selected_index = None;
                state.metadata.current = None;
                state.cache.selected_image = None;
                state.video.select(None);
                if let Some(ref player) = state.audio.player {
                    player.stop();
                }
                state.audio.playing = false;
                state.audio.position = 0.0;
                state.audio.duration = 0.0;
                state.audio.selected_cover = None;
                state.document.reset();
                Task::none()
            } else {
                let preserved_idx = previously_selected_path.and_then(|prev_path| {
                    filtered.iter().position(|entry| entry.path == prev_path)
                });
                drop(filtered);
                if let Some(idx) = preserved_idx {
                    state.media_grid.selected_index = Some(idx);
                    Task::batch(vec![
                        super::tasks::load_visible_thumbnails(state),
                        super::tasks::scroll_to_selected_entry(state, idx),
                    ])
                } else {
                    Task::batch(vec![
                        super::tasks::load_visible_thumbnails(state),
                        super::tasks::select_and_load_entry(state, 0),
                    ])
                }
            }
        }
        MediaMessage::DocumentLoaded {
            path,
            page_index,
            result,
        } => {
            state
                .document
                .in_flight_pages
                .remove(&(path.clone(), page_index));
            if state.document.current_path.as_ref() != Some(&path) {
                return Task::none();
            }
            if page_index != state.document.page_index {
                if let Ok(preview) = result {
                    let actual_page = actual_page_from_preview(&preview, page_index);
                    let data = crate::state::document::DocumentPreviewData::from_backend(&preview);
                    state.cache.document_cache.push((path, actual_page), data);
                }
                return Task::none();
            }

            state.document.loading = false;
            match result {
                Ok(preview) => {
                    let actual_page = actual_page_from_preview(&preview, page_index);
                    let data = crate::state::document::DocumentPreviewData::from_backend(&preview);
                    let total_pages = super::tasks::document_total_pages(&data);
                    state
                        .cache
                        .document_cache
                        .push((path.clone(), actual_page), data.clone());
                    state.document.page_index = actual_page;
                    state.document.data = Some(data);
                    if let Some(total) = total_pages
                        && total > 1
                    {
                        return super::tasks::prefetch_adjacent_document_pages(
                            state,
                            &path,
                            actual_page,
                            total,
                        );
                    }
                }
                Err(err) => {
                    tracing::error!("Failed to load document preview: {err}");
                    state.document.data =
                        Some(crate::state::document::DocumentPreviewData::Error(err));
                }
            }
            Task::none()
        }
        MediaMessage::DocumentPrevPage => navigate_document_page(state, false),
        MediaMessage::DocumentNextPage => navigate_document_page(state, true),
        MediaMessage::DocumentSelectSheet(idx) => {
            if let Some(crate::state::document::DocumentPreviewData::Spreadsheet {
                sheets,
                active_sheet,
            }) = &mut state.document.data
                && idx < sheets.len()
            {
                *active_sheet = idx;
            }
            Task::none()
        }
        MediaMessage::DocumentLinkClicked(uri) => {
            if is_safe_http_url(&uri) {
                #[cfg(not(test))]
                if let Err(e) = open::that_detached(&uri) {
                    tracing::error!("Failed to open document link externally: {e}");
                }
            } else {
                tracing::warn!("Ignored non-http(s) document link");
            }
            Task::none()
        }
    }
}

fn actual_page_from_preview(
    preview: &media_sort_backend::media::document::DocumentPreview,
    fallback_page: usize,
) -> usize {
    use media_sort_backend::media::document::DocumentPreview;
    match preview {
        DocumentPreview::PagedImage { page_index, .. } => *page_index,
        DocumentPreview::Markdown {
            chapter_index: Some(chapter),
            ..
        } => *chapter,
        _ => fallback_page,
    }
}

fn step_index(current: usize, len: usize, forward: bool) -> usize {
    if forward {
        (current + 1).min(len - 1)
    } else {
        current.saturating_sub(1)
    }
}

fn navigate_document_page(state: &mut AppState, forward: bool) -> Task<Message> {
    use crate::state::document::DocumentPreviewData;

    match &mut state.document.data {
        Some(DocumentPreviewData::Slides {
            slides,
            slide_index,
        }) => {
            if !slides.is_empty() {
                *slide_index = step_index(*slide_index, slides.len(), forward);
                state.document.page_index = *slide_index;
            }
            return Task::none();
        }
        Some(DocumentPreviewData::Spreadsheet {
            sheets,
            active_sheet,
        }) => {
            if !sheets.is_empty() {
                *active_sheet = step_index(*active_sheet, sheets.len(), forward);
            }
            return Task::none();
        }
        _ => {}
    }

    let Some(path) = state.document.current_path.clone() else {
        return Task::none();
    };
    let Some(total) = state
        .document
        .data
        .as_ref()
        .and_then(super::tasks::document_total_pages)
    else {
        return Task::none();
    };
    if total == 0 {
        return Task::none();
    }
    let new_page = step_index(state.document.page_index, total, forward);
    if new_page == state.document.page_index {
        return Task::none();
    }

    state.document.page_index = new_page;
    if let Some(cached) = state
        .cache
        .document_cache
        .get(&(path.clone(), new_page))
        .cloned()
    {
        state.document.loading = false;
        state.document.data = Some(cached);
        super::tasks::prefetch_adjacent_document_pages(state, &path, new_page, total)
    } else {
        state.document.loading = true;
        if state
            .document
            .in_flight_pages
            .insert((path.clone(), new_page))
        {
            super::tasks::load_document_preview_task(
                path,
                new_page,
                state.settings.advanced.enable_libreoffice_preview,
            )
        } else {
            Task::none()
        }
    }
}

pub(crate) fn is_safe_http_url(uri: &str) -> bool {
    if uri.is_empty() || uri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let bytes = uri.as_bytes();
    let is_http = bytes.len() > 7 && bytes[..7].eq_ignore_ascii_case(b"http://");
    let is_https = bytes.len() > 8 && bytes[..8].eq_ignore_ascii_case(b"https://");
    is_http || is_https
}

/// Surfaces refused actions to the user. `TargetExists` gets a status
/// banner (a same-named file/folder in the destination is a user-visible
/// conflict — previously it only produced a log line and the action
/// silently did nothing); everything else is logged as before.
fn report_action_error(state: &mut AppState, e: ActionError) {
    if let ActionError::TargetExists(target) = e {
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| target.display().to_string());
        state.set_status(state.l10n.get("status-target-exists", &[("name", &name)]));
    } else {
        tracing::error!("Cannot create action: {e}");
    }
}

pub fn handle_media_scan_completed(
    state: &mut AppState,
    result: Result<Vec<media_sort_core::models::MediaEntry>, String>,
) -> Task<Message> {
    match result {
        Ok(entries) => {
            state.media_grid.entries = entries;
            let select_idx = state.media_grid.pending_select_index.take().unwrap_or(0);
            Task::batch(vec![
                super::tasks::select_and_load_entry(state, select_idx),
                super::tasks::load_visible_thumbnails(state),
            ])
        }
        Err(err) => {
            tracing::error!("Asynchronous media retrieval failed: {}", err);
            Task::none()
        }
    }
}
