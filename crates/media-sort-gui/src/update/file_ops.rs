use std::path::{Path, PathBuf};

use iced::Task;
use media_sort_core::actions::copy_action::CopyAction;
use media_sort_core::actions::delete_action::DeleteAction;
use media_sort_core::actions::move_action::MoveAction;
use media_sort_core::actions::rename_action::RenameAction;
use media_sort_core::actions::reversible::{ActionError, ReversibleAction};

use crate::message::{MediaMessage, Message};
use crate::state::AppState;

/// Move the currently selected media entry into `target_folder`, record it in
/// undo history, remove it from the grid, and select the next entry.
pub fn execute_move_to_folder(state: &mut AppState, target_folder: &Path) -> Task<Message> {
    let Some(index) = state.media_grid.selected_index else {
        return Task::none();
    };
    let filtered = state.media_grid.filtered_entries();
    let Some(entry) = filtered.get(index) else {
        return Task::none();
    };
    let entry_path = entry.path.clone();
    drop(filtered);

    match MoveAction::new(&entry_path, target_folder) {
        Ok(mut action) => {
            if let Err(e) = action.execute() {
                tracing::error!("Move failed: {e}");
            } else {
                state.history.push_executed(Box::new(action));
                state.media_grid.entries.retain(|e| e.path != entry_path);
                state.media_grid.rebuild_lower_names();
                return super::preview::select_and_load_entry(state, index);
            }
        }
        Err(e) => report_action_error(state, e),
    }
    Task::none()
}

/// Copy the currently selected media entry into `target_folder` and record it
/// in undo history.
pub fn execute_copy_to_folder(state: &mut AppState, target_folder: &Path) -> Task<Message> {
    let Some(index) = state.media_grid.selected_index else {
        return Task::none();
    };
    let filtered = state.media_grid.filtered_entries();
    let Some(entry) = filtered.get(index) else {
        return Task::none();
    };
    let entry_path = entry.path.clone();
    drop(filtered);

    match CopyAction::new(&entry_path, target_folder) {
        Ok(mut action) => {
            if let Err(e) = action.execute() {
                tracing::error!("Copy failed: {e}");
            } else {
                state.history.push_executed(Box::new(action));
            }
        }
        Err(e) => report_action_error(state, e),
    }
    Task::none()
}

/// Move `path` to the system trash, record a [`DeleteAction`], and update
/// grid selection.
pub fn delete_entry(state: &mut AppState, path: PathBuf) -> Task<Message> {
    let index_to_select = state.media_grid.selected_index.unwrap_or(0);
    // Entries are scanned from the canonicalized current folder, so the retain
    // comparison must use the canonicalized path. Resolve BEFORE deleting:
    // afterwards canonicalize() fails because the file is gone. Symlink
    // sources are deliberately NOT resolved — resolving would bypass the trash
    // layer's symlink refusal and act on the link's target.
    let canonical_path = if media_sort_core::path_utils::is_symlink(&path) {
        path
    } else {
        path.canonicalize().unwrap_or(path)
    };
    match media_sort_backend::filesystem::trash::delete_to_trash(&canonical_path) {
        Ok(handle) => {
            let action = DeleteAction::new(&canonical_path, handle);
            state.history.push_executed(Box::new(action));
            state
                .media_grid
                .entries
                .retain(|e| e.path != canonical_path);
            state.media_grid.rebuild_lower_names();
            return super::preview::select_and_load_entry(state, index_to_select);
        }
        Err(e) => {
            tracing::error!("Cannot delete to trash: {e}");
        }
    }
    Task::none()
}

/// Open the rename modal pre-filled with the selected entry's stem.
pub fn trigger_rename(state: &mut AppState) -> Task<Message> {
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

/// Execute a rename of `path` to `new_name` (stem) and update the grid entry.
pub fn rename_entry(state: &mut AppState, path: PathBuf, new_name: String) -> Task<Message> {
    // Entries are scanned from the canonicalized current folder, so the position
    // lookup must compare against the canonicalized source path. Resolve
    // BEFORE the rename: after execute() the old path no longer exists.
    // Symlink sources are deliberately NOT resolved so RenameAction's
    // SourceIsSymlink refusal still triggers.
    let canonical_path = if media_sort_core::path_utils::is_symlink(&path) {
        path
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
                    state.media_grid.rebuild_lower_names();
                    state.rename.path = None;
                    state.rename.input_value.clear();
                    let target_idx = state
                        .media_grid
                        .filtered_entries()
                        .iter()
                        .position(|e| e.path == new_path)
                        .unwrap_or(pos);
                    return super::preview::select_and_load_entry(state, target_idx);
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

pub fn rename_input_changed(state: &mut AppState, val: String) -> Task<Message> {
    let trimmed = val.trim().to_string();
    let error = RenameAction::validate_stem(&trimmed)
        .err()
        .map(|e| match &e {
            ActionError::IllegalCharacters { character, .. } if *character == '\0' => String::new(),
            ActionError::IllegalCharacters { character, .. } => state.l10n.get(
                "ui-rename-illegal-char",
                &[("char", &character.to_string())],
            ),
            _ => e.to_string(),
        });
    state.rename.input_value = val;
    state.rename.error = error;
    Task::none()
}

pub fn submit_rename(state: &mut AppState) -> Task<Message> {
    let new_name = state.rename.input_value.trim().to_string();
    if new_name.is_empty() || RenameAction::validate_stem(&new_name).is_err() {
        return Task::none();
    }
    if let Some(path) = state.rename.path.take() {
        state.rename.input_value.clear();
        return Task::done(Message::Media(MediaMessage::RenameEntry(path, new_name)));
    }
    Task::none()
}

pub fn cancel_rename(state: &mut AppState) -> Task<Message> {
    state.rename.path = None;
    state.rename.input_value.clear();
    state.rename.error = None;
    Task::none()
}

pub fn undo(state: &mut AppState) -> Task<Message> {
    let index = state.media_grid.selected_index.unwrap_or(0);
    if let Err(e) = state.history.undo() {
        tracing::error!("Undo failed: {e}");
    } else {
        state.start_async_media_scan(index);
    }
    Task::none()
}

pub fn redo(state: &mut AppState) -> Task<Message> {
    let index = state.media_grid.selected_index.unwrap_or(0);
    if let Err(e) = state.history.redo() {
        tracing::error!("Redo failed: {e}");
    } else {
        state.start_async_media_scan(index);
    }
    Task::none()
}

/// Surfaces refused actions to the user. `TargetExists` gets a status banner;
/// everything else is logged.
pub fn report_action_error(state: &mut AppState, e: ActionError) {
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
