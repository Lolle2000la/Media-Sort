use iced::Task;

use crate::message::{MediaMessage, Message};
use crate::state::AppState;

/// Thin grid coordinator delegating file operations, thumbnails, metadata,
/// image loading, and entry selection to focused submodules.
pub fn handle_media_message(state: &mut AppState, msg: MediaMessage) -> Task<Message> {
    match msg {
        MediaMessage::SelectEntry(index) => super::preview::select_and_load_entry(state, index),
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
                state.clear_active_preview();
                Task::none()
            } else {
                let target_index = previously_selected_path
                    .and_then(|prev_path| filtered.iter().position(|entry| entry.path == prev_path))
                    .unwrap_or(0);
                drop(filtered);
                super::preview::select_and_load_entry(state, target_index)
            }
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
        MediaMessage::GoLeft => {
            if let Some(idx) = state.media_grid.selected_index
                && idx > 0
            {
                return super::preview::select_and_load_entry(state, idx - 1);
            }
            Task::none()
        }
        MediaMessage::GoRight => {
            if let Some(idx) = state.media_grid.selected_index {
                let filtered_len = state.media_grid.filtered_entries().len();
                if idx + 1 < filtered_len {
                    return super::preview::select_and_load_entry(state, idx + 1);
                }
            }
            Task::none()
        }
        MediaMessage::MoveToFolder(target_folder) => {
            super::file_ops::execute_move_to_folder(state, &target_folder)
        }
        MediaMessage::MoveActive => {
            if let Some(target_folder) = state.folder.selected_folder.clone() {
                return super::file_ops::execute_move_to_folder(state, &target_folder);
            }
            Task::none()
        }
        MediaMessage::CopyToFolder(target_folder) => {
            super::file_ops::execute_copy_to_folder(state, &target_folder)
        }
        MediaMessage::CopyActive => {
            if let Some(target_folder) = state.folder.selected_folder.clone() {
                return super::file_ops::execute_copy_to_folder(state, &target_folder);
            }
            Task::none()
        }
        MediaMessage::DeleteEntry(path) => super::file_ops::delete_entry(state, path),
        MediaMessage::TriggerRename => super::file_ops::trigger_rename(state),
        MediaMessage::RenameEntry(path, new_name) => {
            super::file_ops::rename_entry(state, path, new_name)
        }
        MediaMessage::RenameInputChanged(val) => super::file_ops::rename_input_changed(state, val),
        MediaMessage::SubmitRename => super::file_ops::submit_rename(state),
        MediaMessage::CancelRename => super::file_ops::cancel_rename(state),
        MediaMessage::Undo => super::file_ops::undo(state),
        MediaMessage::Redo => super::file_ops::redo(state),
        MediaMessage::MetadataLoaded(result) => {
            super::metadata::handle_metadata_loaded(state, result)
        }
        MediaMessage::ThumbnailReady(path, w, h, data) => {
            super::thumbnails::handle_thumbnail_ready(state, path, w, h, data)
        }
        MediaMessage::ThumbnailFailed(path, err) => {
            super::thumbnails::handle_thumbnail_failed(state, path, err)
        }
        MediaMessage::ThumbnailCancelled(path) => {
            super::thumbnails::handle_thumbnail_cancelled(state, path)
        }
        MediaMessage::ImageLoaded(path, result) => {
            super::image::handle_image_loaded(state, path, result)
        }
        MediaMessage::OpenExternal(path) => {
            if let Err(e) = media_sort_backend::platform::open_externally(&path) {
                tracing::error!("Failed to open file externally: {e}");
            }
            Task::none()
        }
        MediaMessage::RevealInExplorer(path) => {
            if let Err(e) = media_sort_backend::platform::reveal_in_file_manager(&path) {
                tracing::error!("Failed to reveal file in file manager: {e}");
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
                state.clear_active_preview();
                Task::none()
            } else {
                let preserved_idx = previously_selected_path.and_then(|prev_path| {
                    filtered.iter().position(|entry| entry.path == prev_path)
                });
                drop(filtered);
                if let Some(idx) = preserved_idx {
                    state.media_grid.selected_index = Some(idx);
                    Task::batch(vec![
                        super::thumbnails::load_visible_thumbnails(state),
                        super::scroll::scroll_to_selected_entry(state, idx),
                    ])
                } else {
                    Task::batch(vec![
                        super::thumbnails::load_visible_thumbnails(state),
                        super::preview::select_and_load_entry(state, 0),
                    ])
                }
            }
        }
    }
}

pub fn handle_media_scan_completed(
    state: &mut AppState,
    result: Result<Vec<media_sort_core::models::MediaEntry>, String>,
) -> Task<Message> {
    match result {
        Ok(entries) => {
            state.media_grid.entries = entries;
            state.media_grid.rebuild_lower_names();
            let select_idx = state.media_grid.pending_select_index.take().unwrap_or(0);
            Task::batch(vec![
                super::preview::select_and_load_entry(state, select_idx),
                super::thumbnails::load_visible_thumbnails(state),
            ])
        }
        Err(err) => {
            tracing::error!("Asynchronous media retrieval failed: {}", err);
            Task::none()
        }
    }
}
