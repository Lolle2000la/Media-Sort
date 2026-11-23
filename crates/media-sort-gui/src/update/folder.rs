use iced::Task;

use crate::message::{FolderMessage, Message};
use crate::state::AppState;

pub fn handle_folder_message(state: &mut AppState, msg: FolderMessage) -> Task<Message> {
    match msg {
        FolderMessage::Open(path) => {
            state.open_folder(&path);
            Task::none()
        }
        FolderMessage::Pick => Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .pick_folder()
                    .await
                    .map(|handle| handle.path().to_path_buf())
            },
            |result| Message::Folder(FolderMessage::PickResult(result)),
        ),
        FolderMessage::PickResult(Some(path)) => {
            Task::done(Message::Folder(FolderMessage::Open(path)))
        }
        FolderMessage::PickResult(None) => Task::none(),
        FolderMessage::PickPin => Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .pick_folder()
                    .await
                    .map(|handle| handle.path().to_path_buf())
            },
            |result| Message::Folder(FolderMessage::PickPinResult(result)),
        ),
        FolderMessage::PickPinResult(Some(path)) => {
            state.pin_folder(&path);
            state.settings.mark_dirty();
            Task::none()
        }
        FolderMessage::PickPinResult(None) => Task::none(),
        FolderMessage::SelectedPinned(path, idx) => {
            state.folder.set_selected(path.clone(), idx);
            state.folder.dragging_pinned_folder = Some(path);
            Task::none()
        }
        FolderMessage::DragPinnedOver(target_path) => {
            if let Some(source_path) = state.folder.dragging_pinned_folder.clone()
                && source_path != target_path
                && let Some(pos_source) = state
                    .folder
                    .pinned_folders
                    .iter()
                    .position(|p| p.path == source_path)
                && let Some(pos_target) = state
                    .folder
                    .pinned_folders
                    .iter()
                    .position(|p| p.path == target_path)
            {
                state.swap_pinned_folders(pos_source, pos_target);
            }
            Task::none()
        }
        FolderMessage::DragPinnedReleased => {
            if state.folder.dragging_pinned_folder.is_some() {
                state.folder.dragging_pinned_folder = None;
                state.settings.mark_dirty();
            }
            Task::none()
        }
        FolderMessage::HoverPinned(path) => {
            state.folder.hovered_pinned_folder = Some(path);
            Task::none()
        }
        FolderMessage::HoverPinnedNone => {
            state.folder.hovered_pinned_folder = None;
            Task::none()
        }
        FolderMessage::Selected(path, idx) => {
            state.folder.set_selected(path, idx);
            Task::none()
        }
        FolderMessage::ToggleExpand(path, idx) => {
            state.toggle_folder_expand(&path, idx);
            Task::none()
        }
        FolderMessage::PinSelected => {
            let path_to_pin = state
                .folder
                .selected_folder
                .clone()
                .or(state.folder.current_folder.clone());
            if let Some(path) = path_to_pin {
                state.pin_folder(&path);
                state.settings.mark_dirty();
            }
            Task::none()
        }
        FolderMessage::UnpinCurrent(path) => {
            state.unpin_folder(&path);
            state.settings.mark_dirty();
            Task::none()
        }
        FolderMessage::MovePinnedUp(path) => {
            state.move_pinned_folder_up(&path);
            state.settings.mark_dirty();
            Task::none()
        }
        FolderMessage::MovePinnedDown(path) => {
            state.move_pinned_folder_down(&path);
            state.settings.mark_dirty();
            Task::none()
        }
        FolderMessage::TriggerCreate => {
            if let Some(p) = state
                .folder
                .selected_folder
                .as_ref()
                .or(state.folder.current_folder.as_ref())
            {
                state.create_folder.creating_folder_parent = Some(p.clone());
                state.create_folder.create_folder_input = String::new();
            }
            Task::none()
        }
        FolderMessage::CreateInputChanged(val) => {
            state.create_folder.create_folder_input = val;
            Task::none()
        }
        FolderMessage::SubmitCreate(_parent) => {
            if let Some(parent) = state.create_folder.creating_folder_parent.take() {
                let folder_name = state.create_folder.create_folder_input.trim().to_string();
                match media_sort_core::actions::rename_action::RenameAction::validate_stem(
                    &folder_name,
                ) {
                    Ok(()) => {
                        let new_dir = parent.join(&folder_name);
                        match std::fs::create_dir(&new_dir) {
                            Ok(()) => {
                                if state.folder.current_folder.is_some() {
                                    state.build_folder_tree();
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                                // `create_dir` (not `create_dir_all`) fails
                                // on an existing directory, so a no-op
                                // "create" is surfaced instead of silently
                                // succeeding. `validate_stem` guarantees a
                                // single path component, so a plain
                                // `create_dir` cannot fail on a missing
                                // parent mid-path.
                                state.set_status(
                                    state
                                        .l10n
                                        .get("status-target-exists", &[("name", &folder_name)]),
                                );
                            }
                            Err(e) => tracing::error!("Failed to create folder: {e}"),
                        }
                    }
                    Err(e) => {
                        // Rejects traversal (../, ., ..) and OS-illegal
                        // characters; the rename flow validates the same way.
                        tracing::warn!("Rejected folder name {folder_name:?}: {e}");
                        state.set_status(
                            state
                                .l10n
                                .get("status-folder-name-invalid", &[("name", &folder_name)]),
                        );
                    }
                }
                state.create_folder.create_folder_input.clear();
            }
            Task::none()
        }
        FolderMessage::CancelCreate => {
            state.create_folder.creating_folder_parent = None;
            state.create_folder.create_folder_input.clear();
            Task::none()
        }
        FolderMessage::PinShortcut(n) => {
            let pinned_idx = (n.saturating_sub(1)) as usize;
            if let Some(target_folder) = state
                .folder
                .pinned_folders
                .get(pinned_idx)
                .map(|p| p.path.clone())
            {
                return super::file_ops::execute_move_to_folder(state, &target_folder);
            }
            Task::none()
        }
        FolderMessage::TreeScrolled(offset, viewport_height, content_height) => {
            state.folder.scroll.offset_y = offset.y;
            state.folder.scroll.viewport_height = viewport_height;
            state.folder.scroll.content_height = content_height;
            Task::none()
        }
    }
}
