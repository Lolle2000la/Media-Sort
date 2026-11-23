pub(super) use super::scroll::{calculate_scroll_into_view_1d, relative_position_for};
pub(super) use super::*;
pub(super) use crate::message::{
    AudioMessage, DocumentMessage, FolderMessage, MediaMessage, Message, SettingsMessage,
};
pub(super) use crate::state::{AppState, SettingsUiState};
pub(super) use crate::update::keyboard::{handle_key_captured, is_modal_open};
pub(super) use media_sort_backend::filesystem::watcher::FileSystemEvent;
pub(super) use media_sort_core::actions::rename_action::RenameAction;
pub(super) use media_sort_core::actions::reversible::ReversibleAction;
pub(super) use media_sort_core::media_type::MediaType;
pub(super) use media_sort_core::models::MediaEntry;
pub(super) use media_sort_core::settings::keybindings::{Key, KeyAction};
pub(super) use media_sort_core::settings::store::SettingsStore;
pub(super) use std::path::PathBuf;
pub(super) use std::time::{Duration, Instant};

/// Drive `poll_background_channels` until the media scan started by
/// `open_folder` (or `start_async_media_scan` via Undo/Redo) finishes.
pub(super) fn drain_async_scan(state: &mut AppState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.media_grid.scan_receiver.is_some() && Instant::now() < deadline {
        let _ = poll_background_channels(state);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        state.media_grid.scan_receiver.is_none(),
        "async media scan did not complete within 10 s deadline"
    );
}

/// Drive `poll_background_channels` until the folder tree rebuild started
/// by `open_folder` (or a watcher-driven refresh) finishes, including any
/// chained `tree_refresh_pending` rebuilds.
pub(super) fn drain_folder_tree(state: &mut AppState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while (state.folder.folder_tree_receiver.is_some() || state.folder.tree_refresh_pending)
        && Instant::now() < deadline
    {
        let _ = poll_background_channels(state);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        state.folder.folder_tree_receiver.is_none() && !state.folder.tree_refresh_pending,
        "folder tree rebuild did not complete within 10 s deadline"
    );
}

pub(super) fn temp_media_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{}_{}", prefix, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(super) fn make_media_entry(path: &str, media_type: MediaType) -> MediaEntry {
    let pb = PathBuf::from(path);
    let file_name = pb
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    MediaEntry {
        path: pb,
        media_type,
        file_name,
        animated: None,
    }
}

pub(super) fn setup_temp_rename_action(dir_prefix: &str) -> (PathBuf, RenameAction) {
    let dir = std::env::temp_dir().join(format!("{}_{}", dir_prefix, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("test.txt");
    std::fs::write(&file, b"content").unwrap();
    let mut action = RenameAction::new(&file, "renamed").unwrap();
    action.execute().unwrap();
    (dir, action)
}

pub(super) fn setup_temp_dir_with_files(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("mediasort_{}_{}", name, std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("test_image.jpg");
    std::fs::write(&file, b"fake jpeg data").unwrap();
    let dest = root.join("subfolder");
    std::fs::create_dir_all(&dest).unwrap();
    (root, file, dest)
}

pub(super) fn setup_data_dir_with_files(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("media-sort")
        .join("test");
    let root = base.join(format!("{}_{}", name, std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("test_image.jpg");
    std::fs::write(&file, b"fake jpeg data").unwrap();
    let dest = root.join("subfolder");
    std::fs::create_dir_all(&dest).unwrap();
    (root, file, dest)
}

mod document;
mod file_operations;
mod filesystem_watcher;
mod keyboard_and_modals;
mod navigation_and_selection;
mod search_and_filter;
mod settings_and_ui;
