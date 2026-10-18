use super::tasks::{calculate_scroll_into_view_1d, relative_position_for};
use super::*;
use crate::message::{FolderMessage, MediaMessage, Message, SettingsMessage};
use crate::state::{AppState, SettingsUiState};
use crate::update::keyboard::handle_key_captured;
use media_sort_backend::filesystem::watcher::FileSystemEvent;
use media_sort_core::actions::rename_action::RenameAction;
use media_sort_core::actions::reversible::ReversibleAction;
use media_sort_core::media_type::MediaType;
use media_sort_core::models::MediaEntry;
use media_sort_core::settings::keybindings::Key;
use media_sort_core::settings::store::SettingsStore;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Drive `poll_background_channels` until the media scan started by
/// `open_folder` (or `start_async_media_scan` via Undo/Redo) finishes.
/// Production never blocks on the scan — the app keeps rendering empty
/// until `poll_background_channels` drains the receiver on subsequent
/// `Tick`s — but tests need `entries` populated before they assert, so
/// this helper drains synchronously with a 10 s deadline (generous
/// enough to absorb scheduler contention under parallel test execution
/// where every test spawns its own scanner thread).
fn drain_async_scan(state: &mut AppState) {
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
fn drain_folder_tree(state: &mut AppState) {
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

fn temp_media_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{}_{}", prefix, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn test_select_entry_in_bounds() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![MediaEntry {
        path: PathBuf::from("/test/a.jpg"),
        media_type: MediaType::Image,
        file_name: "a.jpg".into(),
        animated: None,
    }];
    state.media_grid.search.query = String::new();
    let _task = update(&mut state, Message::Media(MediaMessage::SelectEntry(0)));
    assert_eq!(state.media_grid.selected_index, Some(0));
    assert!(state.metadata.current.is_none());
}

#[test]
fn test_select_entry_out_of_bounds() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![
        MediaEntry {
            path: PathBuf::from("/test/a.jpg"),
            media_type: MediaType::Image,
            file_name: "a.jpg".into(),
            animated: None,
        },
        MediaEntry {
            path: PathBuf::from("/test/b.jpg"),
            media_type: MediaType::Image,
            file_name: "b.jpg".into(),
            animated: None,
        },
    ];
    state.media_grid.search.query = String::new();
    state.media_grid.selected_index = None;
    let _task = update(&mut state, Message::Media(MediaMessage::SelectEntry(99)));
    assert_eq!(state.media_grid.selected_index, Some(1));
}

#[test]
fn test_select_entry_filtered_empty() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![MediaEntry {
        path: PathBuf::from("/test/a.jpg"),
        media_type: MediaType::Image,
        file_name: "a.jpg".into(),
        animated: None,
    }];
    state.media_grid.search.query = "nomatch".into();
    state.media_grid.selected_index = Some(0);
    let _task = update(&mut state, Message::Media(MediaMessage::SelectEntry(0)));
    assert_eq!(state.media_grid.selected_index, None);
}

fn setup_temp_rename_action(dir_prefix: &str) -> (std::path::PathBuf, RenameAction) {
    let dir = std::env::temp_dir().join(format!("{}_{}", dir_prefix, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("test.txt");
    std::fs::write(&file, b"content").unwrap();
    let mut action = RenameAction::new(&file, "renamed").unwrap();
    action.execute().unwrap();
    (dir, action)
}

#[test]
fn test_keycaptured_undo_when_history_has_actions() {
    let mut state = AppState::new(SettingsStore::default());
    let (dir, action) = setup_temp_rename_action("mediasort_undo");

    state.history.push_executed(Box::new(action));
    assert!(state.history.can_undo());

    let _ = update(
        &mut state,
        Message::KeyCaptured(Key::Character('Q'), false, false, false),
    );
    let _ = update(&mut state, Message::Media(MediaMessage::Undo));
    assert!(state.history.can_redo());
    assert!(!state.history.can_undo());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_keycaptured_undo_when_history_empty() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.history.can_undo());

    let _task = update(
        &mut state,
        Message::KeyCaptured(Key::Character('Q'), false, false, false),
    );
    assert!(!state.history.can_undo());
    assert!(!state.history.can_redo());
}

#[test]
fn test_keycaptured_redo_when_history_has_undone() {
    let mut state = AppState::new(SettingsStore::default());
    let (dir, action) = setup_temp_rename_action("mediasort_redo");

    state.history.push_executed(Box::new(action));
    state.history.undo().unwrap();
    assert!(state.history.can_redo());
    assert!(!state.history.can_undo());

    let _ = update(
        &mut state,
        Message::KeyCaptured(Key::Character('E'), false, false, false),
    );
    let _ = update(&mut state, Message::Media(MediaMessage::Redo));
    assert!(!state.history.can_redo());
    assert!(state.history.can_undo());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_keycaptured_capture_mode_updates_binding() {
    let mut state = AppState::new(SettingsStore::default());
    state.settings_ui = SettingsUiState::Keybindings {
        editing_keybinding: Some(0),
        waiting_for_key: true,
    };

    let _task = update(
        &mut state,
        Message::KeyCaptured(Key::Character('X'), true, false, false),
    );

    assert!(matches!(
        state.settings_ui,
        SettingsUiState::Keybindings {
            editing_keybinding: None,
            waiting_for_key: false,
        }
    ));
    let kb = &state.settings.keybindings;
    assert_eq!(kb.move_to_folder.key, Key::Character('X'));
    assert!(kb.move_to_folder.ctrl);
    assert!(!kb.move_to_folder.shift);
    assert!(!kb.move_to_folder.alt);
}

#[test]
fn test_keycaptured_capture_mode_clears_editing_state() {
    let mut state = AppState::new(SettingsStore::default());
    state.settings_ui = SettingsUiState::Keybindings {
        editing_keybinding: Some(3),
        waiting_for_key: true,
    };

    let _task = update(
        &mut state,
        Message::KeyCaptured(Key::ArrowLeft, false, false, false),
    );

    assert!(matches!(
        state.settings_ui,
        SettingsUiState::Keybindings {
            editing_keybinding: None,
            waiting_for_key: false,
        }
    ));
}

#[test]
fn test_keycaptured_toggle_metadata_panel() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.metadata.panel_expanded);

    let _ = update(
        &mut state,
        Message::KeyCaptured(Key::Character('M'), false, false, false),
    );
    let _ = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleMetadataPanel),
    );
    assert!(state.metadata.panel_expanded);

    let _ = update(
        &mut state,
        Message::KeyCaptured(Key::Character('M'), false, false, false),
    );
    let _ = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleMetadataPanel),
    );
    assert!(!state.metadata.panel_expanded);
}

#[test]
fn test_keycaptured_unpin_triggers_unpin() {
    let mut state = AppState::new(SettingsStore::default());
    let folder = PathBuf::from("/test/unpin_dir");
    state.folder.current_folder = Some(folder.clone());
    state.pin_current_folder();
    assert_eq!(state.folder.pinned_folders.len(), 1);

    let _ = update(
        &mut state,
        Message::KeyCaptured(Key::Character('U'), false, false, false),
    );
    let _ = update(
        &mut state,
        Message::Folder(FolderMessage::UnpinCurrent(folder.clone())),
    );
    assert!(state.folder.pinned_folders.is_empty());
}

#[test]
fn test_keycaptured_pin_without_folder_is_noop() {
    let mut state = AppState::new(SettingsStore::default());
    state.folder.current_folder = None;
    assert!(state.folder.pinned_folders.is_empty());

    let _task = update(
        &mut state,
        Message::KeyCaptured(Key::Character('P'), false, false, false),
    );
    assert!(state.folder.pinned_folders.is_empty());
}

#[test]
fn test_keycaptured_unknown_binding_is_noop() {
    let mut state = AppState::new(SettingsStore::default());
    let saved_undo = state.history.can_undo();
    let _task = update(
        &mut state,
        Message::KeyCaptured(Key::F9, false, false, false),
    );
    assert_eq!(state.history.can_undo(), saved_undo);
    assert!(!state.metadata.panel_expanded);
}

fn setup_temp_dir_with_files(
    name: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("mediasort_{}_{}", name, std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("test_image.jpg");
    std::fs::write(&file, b"fake jpeg data").unwrap();
    let dest = root.join("subfolder");
    std::fs::create_dir_all(&dest).unwrap();
    (root, file, dest)
}

fn setup_data_dir_with_files(
    name: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
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

#[test]
fn test_move_to_folder_success() {
    let (root, file, dest) = setup_temp_dir_with_files("move_ok");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    drain_async_scan(&mut state);
    state.media_grid.selected_index = Some(0);

    assert!(file.exists());
    let dest_file = dest.join("test_image.jpg");
    assert!(!dest_file.exists());

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::MoveToFolder(dest.clone())),
    );

    assert!(!file.exists());
    assert!(dest_file.exists());
    assert!(state.history.can_undo());
    assert_eq!(state.history.done_len(), 1);
    assert_eq!(state.media_grid.selected_index, None);
    assert!(state.media_grid.entries.is_empty());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_move_to_folder_no_selection_is_noop() {
    let (root, _file, dest) = setup_temp_dir_with_files("move_nosel");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    state.media_grid.selected_index = None;

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::MoveToFolder(dest.clone())),
    );

    assert!(!state.history.can_undo());
    assert!(state.media_grid.selected_index.is_none());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_move_to_folder_index_out_of_bounds() {
    let (root, _file, dest) = setup_temp_dir_with_files("move_oob");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    state.media_grid.selected_index = Some(999);

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::MoveToFolder(dest.clone())),
    );

    assert!(!state.history.can_undo());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_move_to_folder_nonexistent_target() {
    let (root, file, _dest) = setup_temp_dir_with_files("move_nodir");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    state.media_grid.selected_index = Some(0);

    let nonexistent = root.join("does_not_exist");

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::MoveToFolder(nonexistent)),
    );

    assert!(file.exists());
    assert!(!state.history.can_undo());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_delete_entry_success() {
    let (root, file, _dest) = setup_data_dir_with_files("delete_ok");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);

    assert!(file.exists());

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::DeleteEntry(file.clone())),
    );

    assert!(!file.exists());
    assert!(state.history.can_undo());
    assert_eq!(state.history.done_len(), 1);
    assert_eq!(state.media_grid.selected_index, None);
    assert!(state.media_grid.entries.is_empty());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_delete_entry_nonexistent_file() {
    let (root, _file, _dest) = setup_data_dir_with_files("delete_nofile");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);

    let nonexistent = root.join("does_not_exist.jpg");

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::DeleteEntry(nonexistent)),
    );

    assert!(!state.history.can_undo());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_undo_after_move() {
    let (root, file, dest) = setup_temp_dir_with_files("undo_move");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    drain_async_scan(&mut state);
    state.media_grid.selected_index = Some(0);

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::MoveToFolder(dest.clone())),
    );
    assert!(!file.exists());
    let dest_file = dest.join("test_image.jpg");
    assert!(dest_file.exists());
    assert!(state.history.can_undo());
    let _task = update(&mut state, Message::Media(MediaMessage::Undo));

    drain_async_scan(&mut state);

    assert!(file.exists());
    assert!(!dest_file.exists());
    assert!(!state.history.can_undo());
    assert!(state.history.can_redo());
    assert_eq!(state.media_grid.selected_index, Some(0));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_undo_after_delete() {
    let (root, file, _dest) = setup_data_dir_with_files("undo_delete");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::DeleteEntry(file.clone())),
    );
    assert!(!file.exists());
    assert!(state.history.can_undo());

    let _task = update(&mut state, Message::Media(MediaMessage::Undo));

    assert!(file.exists());
    assert!(!state.history.can_undo());
    assert!(state.history.can_redo());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_redo_after_undo_move() {
    let (root, file, dest) = setup_temp_dir_with_files("redo_move");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    drain_async_scan(&mut state);
    state.media_grid.selected_index = Some(0);

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::MoveToFolder(dest.clone())),
    );
    let _ = update(&mut state, Message::Media(MediaMessage::Undo));
    assert!(file.exists());
    assert!(state.history.can_redo());

    let _task = update(&mut state, Message::Media(MediaMessage::Redo));

    assert!(!file.exists());
    let dest_file = dest.join("test_image.jpg");
    assert!(dest_file.exists());
    assert!(state.history.can_undo());
    assert!(!state.history.can_redo());
    assert!(state.media_grid.entries.is_empty());
    assert_eq!(state.media_grid.selected_index, None);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_undo_empty_history_no_panic() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.history.can_undo());

    let _task = update(&mut state, Message::Media(MediaMessage::Undo));
    assert!(!state.history.can_undo());
}

#[test]
fn test_redo_empty_undone_no_panic() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.history.can_redo());

    let _task = update(&mut state, Message::Media(MediaMessage::Redo));
    assert!(!state.history.can_redo());
}

#[test]
fn test_rename_entry_success() {
    let (root, file, _dest) = setup_temp_dir_with_files("rename_ok");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    drain_async_scan(&mut state);
    state.media_grid.selected_index = Some(0);

    assert!(file.exists());

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::RenameEntry(
            file.clone(),
            "renamed_image".to_string(),
        )),
    );

    assert!(!file.exists());
    let renamed = root.join("renamed_image.jpg");
    assert!(renamed.exists());
    assert!(state.history.can_undo());
    assert_eq!(state.history.done_len(), 1);
    assert!(state.history.last_done_name(&state.l10n).is_some());
    assert_eq!(state.media_grid.entries.len(), 1);
    // Canonicalize: on macOS temp_dir is /var/... (symlink to /private/var)
    // while the app stores canonicalized paths.
    assert_eq!(
        state.media_grid.entries[0].path,
        renamed.canonicalize().unwrap()
    );
    assert_eq!(state.media_grid.entries[0].file_name, "renamed_image.jpg");

    std::fs::remove_dir_all(&root).ok();
}

#[cfg(unix)]
#[test]
fn test_rename_entry_via_symlinked_folder_alias() {
    // Regression: open_folder canonicalizes the current folder, so entries
    // carry canonical paths. A rename message carrying the non-canonical
    // alias (here: a path through a symlinked dir, like macOS /var vs
    // /private/var) must still find the entry and update its path.
    let base = std::env::temp_dir().join(format!("mediasort_rename_alias_{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let real = base.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let file = real.join("test_image.jpg");
    std::fs::write(&file, b"fake jpeg data").unwrap();
    let alias = base.join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&alias);
    drain_async_scan(&mut state);
    state.media_grid.selected_index = Some(0);

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::RenameEntry(
            alias.join("test_image.jpg"),
            "renamed_image".to_string(),
        )),
    );

    assert!(!file.exists());
    let renamed = real.join("renamed_image.jpg");
    assert!(renamed.exists());
    assert_eq!(
        state.media_grid.entries[0].path,
        renamed.canonicalize().unwrap()
    );
    assert_eq!(state.media_grid.entries[0].file_name, "renamed_image.jpg");

    std::fs::remove_dir_all(&base).ok();
}

#[cfg(unix)]
#[test]
fn test_rename_entry_symlink_source_refused() {
    // The handler must not pre-canonicalize symlink sources: resolving the
    // link would bypass RenameAction's SourceIsSymlink refusal and rename
    // the link's TARGET.
    let base =
        std::env::temp_dir().join(format!("mediasort_rename_symlink_{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let victim = base.join("victim.jpg");
    std::fs::write(&victim, b"do not touch").unwrap();
    let link = base.join("link.jpg");
    std::os::unix::fs::symlink(&victim, &link).unwrap();

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&base);
    drain_async_scan(&mut state);

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::RenameEntry(
            link.clone(),
            "renamed".to_string(),
        )),
    );

    assert!(victim.exists(), "the symlink target must be untouched");
    assert_eq!(std::fs::read(&victim).unwrap(), b"do not touch");
    assert!(link.exists());
    assert!(!state.history.can_undo());

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn test_rename_entry_target_exists_is_noop() {
    let root =
        std::env::temp_dir().join(format!("mediasort_rename_conflict_{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file1 = root.join("a.jpg");
    let file2 = root.join("b.jpg");
    std::fs::write(&file1, b"a").unwrap();
    std::fs::write(&file2, b"b").unwrap();

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::RenameEntry(file1.clone(), "b".to_string())),
    );

    assert!(file1.exists());
    assert!(file2.exists());
    assert!(!state.history.can_undo());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_move_across_filesystems() {
    let root = std::env::temp_dir().join(format!("mediasort_xdev_src_{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("test.jpg");
    std::fs::write(&file, b"cross-filesystem data").unwrap();

    let dest = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("mediasort_xdev_dst_{}", std::process::id()));
    std::fs::create_dir_all(&dest).unwrap();

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    drain_async_scan(&mut state);
    state.media_grid.selected_index = Some(0);

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::MoveToFolder(dest.clone())),
    );

    assert!(!file.exists());
    let moved_file = dest.join("test.jpg");
    assert!(moved_file.exists());
    assert!(state.history.can_undo());

    let content = std::fs::read_to_string(&moved_file).unwrap();
    assert_eq!(content, "cross-filesystem data");

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&dest).ok();
}

#[test]
fn test_rename_or_copy_same_filesystem() {
    let dir = std::env::temp_dir().join(format!("mediasort_samefs_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("source.txt");
    let dst = dir.join("dest.txt");
    std::fs::write(&src, b"test data").unwrap();

    media_sort_core::path_utils::rename_or_copy_and_delete(&src, &dst).unwrap();
    assert!(!src.exists());
    assert!(dst.exists());
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "test data");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_rename_or_copy_cross_filesystem() {
    let src_dir =
        std::env::temp_dir().join(format!("mediasort_xdev_test_src_{}", std::process::id()));
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("xdev_file.txt");
    std::fs::write(&src, b"cross-fs content").unwrap();

    let dst_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("mediasort_xdev_test_dst_{}", std::process::id()));
    std::fs::create_dir_all(&dst_dir).unwrap();
    let dst = dst_dir.join("xdev_file.txt");

    let result = media_sort_core::path_utils::rename_or_copy_and_delete(&src, &dst);
    assert!(result.is_ok(), "Failed: {:?}", result.err());
    assert!(!src.exists());
    assert!(dst.exists());
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "cross-fs content");

    std::fs::remove_dir_all(&src_dir).ok();
    let _ = std::fs::remove_file(&dst);
    let _ = std::fs::remove_dir(&dst_dir);
}

#[test]
fn test_delete_undo_cross_filesystem() {
    let root = std::env::temp_dir().join(format!("mediasort_del_xdev_{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("delete_me.jpg");
    std::fs::write(&file, b"delete me data").unwrap();

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    assert!(file.exists());

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::DeleteEntry(file.clone())),
    );
    assert!(!file.exists());
    assert!(state.history.can_undo());

    let _task = update(&mut state, Message::Media(MediaMessage::Undo));
    assert!(file.exists());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "delete me data");
    assert!(!state.history.can_undo());
    assert!(state.history.can_redo());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_thumbnail_ready_empty_data() {
    let mut state = AppState::new(SettingsStore::default());
    let cache_size_before = state.cache.thumbnail_cache.len();

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailReady(
            std::path::PathBuf::from("/test/empty.jpg"),
            0,
            0,
            Vec::new(),
        )),
    );
    assert_eq!(state.cache.thumbnail_cache.len(), cache_size_before);
}

#[test]
fn test_thumbnail_ready_valid_data() {
    let mut state = AppState::new(SettingsStore::default());
    let path = std::path::PathBuf::from("/test/thumb.jpg");

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailReady(
            path.clone(),
            1,
            1,
            vec![255, 0, 0, 255],
        )),
    );
    assert_eq!(state.cache.thumbnail_cache.len(), 1);
    assert!(state.cache.thumbnail_cache.contains(&path));
}

#[test]
fn test_thumbnail_cancelled_leaves_entry_usable() {
    // An audio file without embedded cover art yields no thumbnail at all
    // (`Ok(None)` at the prefetch boundary), which the app maps to
    // `ThumbnailCancelled`. It must NOT be recorded in `media_errors`, or the
    // preview pane would render the "file not supported" box and the audio
    // could not be played.
    let mut state = AppState::new(SettingsStore::default());
    let path = std::path::PathBuf::from("/test/audio_without_cover.wav");

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailCancelled(path.clone())),
    );

    assert!(
        state.cache.media_errors.get_error(&path).is_none(),
        "a file with no thumbnail image must not be marked as failed/unsupported"
    );
}

#[test]
fn test_metadata_loaded_error_clears_metadata() {
    let mut state = AppState::new(SettingsStore::default());
    let mut existing = std::collections::BTreeMap::new();
    let mut inner = std::collections::BTreeMap::new();
    inner.insert("Width".to_string(), "1920".to_string());
    existing.insert("EXIF".to_string(), inner);
    state.metadata.current = Some(existing);

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::MetadataLoaded(Err("load failed".to_string()))),
    );
    assert!(state.metadata.current.is_none());
}

#[test]
fn test_metadata_loaded_success() {
    let mut state = AppState::new(SettingsStore::default());
    let mut metadata = std::collections::BTreeMap::new();
    let mut section = std::collections::BTreeMap::new();
    section.insert("Width".to_string(), "1920".to_string());
    metadata.insert("EXIF".to_string(), section);

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::MetadataLoaded(Ok(metadata))),
    );
    assert!(state.metadata.current.is_some());
    let m = state.metadata.current.as_ref().unwrap();
    assert_eq!(m.get("EXIF").unwrap().get("Width").unwrap(), "1920");
}

#[test]
fn test_grid_scrolled_updates_viewport_state() {
    let mut state = AppState::new(SettingsStore::default());
    assert_eq!(state.media_grid.scroll.viewport_width, 0.0);

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::GridScrolled(
            iced::widget::scrollable::AbsoluteOffset { x: 120.0, y: 0.0 },
            400.0,
            1200.0,
        )),
    );
    assert_eq!(state.media_grid.scroll.offset_x, 120.0);
    assert_eq!(state.media_grid.scroll.viewport_width, 400.0);
    assert_eq!(state.media_grid.scroll.content_width, 1200.0);
}

#[test]
fn test_relative_position_for_scrolling() {
    assert_eq!(relative_position_for(0, 7), Some(0.0));
    assert_eq!(relative_position_for(6, 7), Some(1.0));
    assert!((relative_position_for(3, 7).unwrap() - 0.5).abs() < 1e-6);
    assert_eq!(relative_position_for(0, 0), None);
    assert_eq!(relative_position_for(0, 1), None);
    assert_eq!(relative_position_for(99, 7), Some(1.0));
}

#[test]
fn test_calculate_scroll_into_view_1d() {
    // When viewport size is 0 or content fits in viewport, no scrolling needed
    assert_eq!(
        calculate_scroll_into_view_1d(0.0, 60.0, 0.0, 0.0, 1000.0, 50.0),
        None
    );
    assert_eq!(
        calculate_scroll_into_view_1d(0.0, 60.0, 0.0, 600.0, 500.0, 50.0),
        None
    );

    // Item already comfortably visible with margin: no scroll needed
    assert_eq!(
        calculate_scroll_into_view_1d(200.0, 260.0, 100.0, 600.0, 2000.0, 50.0),
        None
    );

    // Item too close to or past trailing edge: scroll forward to bring into view with margin
    assert_eq!(
        calculate_scroll_into_view_1d(600.0, 660.0, 100.0, 600.0, 2000.0, 50.0),
        Some(110.0)
    );

    // Item past leading edge: scroll backward to bring into view with margin
    assert_eq!(
        calculate_scroll_into_view_1d(60.0, 120.0, 200.0, 600.0, 2000.0, 50.0),
        Some(10.0)
    );

    // Stale scroll offset beyond content bounds (e.g. after list shrinks) is clamped safely
    assert_eq!(
        calculate_scroll_into_view_1d(50.0, 100.0, 800.0, 400.0, 600.0, 20.0),
        Some(30.0)
    );
}

#[test]
fn test_tick_should_exit_saves_settings() {
    let tmp = std::env::temp_dir().join(format!("mediasort_test_tick_save_{}", std::process::id()));
    let settings = SettingsStore {
        custom_path: Some(tmp.clone()),
        ..SettingsStore::default()
    };
    let mut state = AppState::new(settings);
    state.settings.general.theme = "Dark".to_string();
    state.should_exit = true;

    let _task = update(&mut state, Message::Tick(Instant::now()));

    let data = std::fs::read_to_string(&tmp).unwrap();
    let reloaded: SettingsStore = toml::from_str(&data).unwrap();
    assert_eq!(reloaded.general.theme, "Dark");

    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_pinned_folder_drag_and_drop() {
    let mut state = AppState::new(SettingsStore::default());
    let path1 = PathBuf::from("/pinned1");
    let path2 = PathBuf::from("/pinned2");

    state.folder.pinned_folders = vec![
        media_sort_core::models::PinnedFolder {
            path: path1.clone(),
            name: "p1".into(),
        },
        media_sort_core::models::PinnedFolder {
            path: path2.clone(),
            name: "p2".into(),
        },
    ];
    state.folder.current_folder = Some(PathBuf::from("/current"));
    state.build_folder_tree();

    let _ = update(
        &mut state,
        Message::Folder(FolderMessage::HoverPinned(path1.clone())),
    );
    assert_eq!(state.folder.hovered_pinned_folder, Some(path1.clone()));

    let _ = update(&mut state, Message::Folder(FolderMessage::HoverPinnedNone));
    assert_eq!(state.folder.hovered_pinned_folder, None);

    let _ = update(
        &mut state,
        Message::Folder(FolderMessage::SelectedPinned(path1.clone(), 1)),
    );
    assert_eq!(state.folder.selected_folder, Some(path1.clone()));
    assert_eq!(state.folder.dragging_pinned_folder, Some(path1.clone()));

    let _ = update(
        &mut state,
        Message::Folder(FolderMessage::DragPinnedOver(path2.clone())),
    );
    assert_eq!(state.folder.pinned_folders[0].path, path2);
    assert_eq!(state.folder.pinned_folders[1].path, path1);
    assert_eq!(state.folder.dragging_pinned_folder, Some(path1.clone()));

    let _ = update(
        &mut state,
        Message::Folder(FolderMessage::DragPinnedReleased),
    );
    assert_eq!(state.folder.dragging_pinned_folder, None);
}

// ============================================================================
// Video message tests
// ============================================================================
//
// `VideoState` behavior (Ready wiring, Action dispatch, WorkerEvent handling,
// seek storage, no-sender paths) is tested in the iced-mpv crate itself
// (src/state.rs). The GUI only owns the app-level handling of the
// `VideoEvent` domain events returned by the state machine: load failures are
// recorded in `media_errors`, and the first committed frame clears them.

#[test]
fn test_video_load_failed_records_error_and_frame_ready_clears() {
    let mut state = AppState::new(SettingsStore::default());
    let path = PathBuf::from("/videos/broken.mp4");

    let fail = iced_mpv::PlayerMessage::Event(iced_mpv::WorkerEvent::LoadFailed {
        path: path.clone(),
        error: "demo error".into(),
    });
    let _task = update(&mut state, Message::Video(fail));
    assert!(state.cache.media_errors.get_error(&path).is_some());

    // A successful load cycle: select the file, the worker reports readiness,
    // and the first committed frame clears the recorded error.
    state.video.select(Some(path.clone()));
    let progress = iced_mpv::PlayerMessage::Event(iced_mpv::WorkerEvent::PlaybackProgress {
        position: 0.0,
        duration: 10.0,
    });
    let _task = update(&mut state, Message::Video(progress));
    let ready = iced_mpv::PlayerMessage::Event(iced_mpv::WorkerEvent::FrameReady {
        path: path.clone(),
        width: 640,
        height: 360,
        rotation: iced_mpv::Rotation::R0,
        rgba: std::sync::Arc::new(vec![0u8; 640 * 360 * 4]),
    });
    let _task = update(&mut state, Message::Video(ready));
    assert!(state.cache.media_errors.get_error(&path).is_none());
}

// ============================================================================
// Keyboard handler tests
// ============================================================================

#[test]
fn test_key_captured_waiting_for_key_clears_state() {
    let mut state = AppState::new(SettingsStore::default());
    state.settings_ui = SettingsUiState::Keybindings {
        editing_keybinding: Some(0),
        waiting_for_key: true,
    };
    let _task = handle_key_captured(&mut state, Key::Character('A'), true, false, false);
    assert!(matches!(
        state.settings_ui,
        SettingsUiState::Keybindings {
            editing_keybinding: None,
            waiting_for_key: false,
        }
    ));
}

// ============================================================================
// Update.rs main dispatch tests
// ============================================================================

#[test]
fn test_update_quit_message() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.should_exit);
    let _task = update(&mut state, Message::Quit);
    assert!(state.should_exit);
}

#[test]
fn test_update_open_credits() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.show_credits);
    let _task = update(&mut state, Message::OpenCredits);
    assert!(state.show_credits);
}

#[test]
fn test_update_close_credits() {
    let mut state = AppState::new(SettingsStore::default());
    state.show_credits = true;
    let _task = update(&mut state, Message::CloseCredits);
    assert!(!state.show_credits);
}

#[test]
fn test_update_open_url() {
    let mut state = AppState::new(SettingsStore::default());
    let _task = update(
        &mut state,
        Message::OpenUrl("https://example.com".to_string()),
    );
    // Verifies that it processes without errors and does not change show_credits or should_exit.
    assert!(!state.show_credits);
    assert!(!state.should_exit);
}

#[test]
fn test_update_tick_should_not_exit_initially() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.should_exit);
    let _task = update(&mut state, Message::Tick(Instant::now()));
    assert!(!state.should_exit);
}

#[test]
fn test_update_tick_should_exit() {
    let tmp = std::env::temp_dir().join(format!("mediasort_tick_exit_{}", std::process::id()));
    let settings = SettingsStore {
        custom_path: Some(tmp.clone()),
        ..SettingsStore::default()
    };
    let mut state = AppState::new(settings);
    state.settings.general.theme = "Dark".to_string();
    state.should_exit = true;
    let _task = update(&mut state, Message::Tick(Instant::now()));
    assert!(state.should_exit);

    let data = std::fs::read_to_string(&tmp).unwrap();
    let reloaded: SettingsStore = toml::from_str(&data).unwrap();
    assert_eq!(reloaded.general.theme, "Dark");

    let _ = std::fs::remove_file(&tmp);
}

#[cfg(not(feature = "demo"))]
#[test]
fn test_tick_applies_external_settings_changes() {
    let tmp = std::env::temp_dir().join(format!("mediasort_tick_reload_{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let settings = SettingsStore {
        custom_path: Some(tmp.clone()),
        ..SettingsStore::default()
    };
    let mut state = AppState::new(settings);
    state.settings.save().unwrap();

    // A second instance changes theme, locale and a pinned folder.
    let mut other: SettingsStore = toml::from_str(&std::fs::read_to_string(&tmp).unwrap()).unwrap();
    other.custom_path = Some(tmp.clone());
    other.general.theme = "Dark".to_string();
    other.general.locale = Some("de".to_string());
    other.pinned_folders.paths = vec!["/tmp/pinned".to_string()];
    other.save().unwrap();

    // Force the once-per-second reload poll to run on the next tick.
    state.settings_reload_at = Instant::now();
    let _task = update(&mut state, Message::Tick(Instant::now()));

    assert_eq!(state.settings.general.theme, "Dark");
    assert_eq!(state.l10n.locale(), "de");
    assert_eq!(
        state.settings.pinned_folders.paths,
        vec!["/tmp/pinned".to_string()]
    );
    assert_eq!(state.folder.pinned_folders.len(), 1);
    assert_eq!(
        state.folder.pinned_folders[0].path,
        PathBuf::from("/tmp/pinned")
    );

    let _ = std::fs::remove_file(&tmp);
}

#[cfg(not(feature = "demo"))]
#[test]
fn test_tick_reload_preserves_unflushed_local_change() {
    let tmp = std::env::temp_dir().join(format!(
        "mediasort_tick_reload_local_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    let settings = SettingsStore {
        custom_path: Some(tmp.clone()),
        ..SettingsStore::default()
    };
    let mut state = AppState::new(settings);
    state.settings.save().unwrap();

    // A second instance disables GIF animation.
    let mut other: SettingsStore = toml::from_str(&std::fs::read_to_string(&tmp).unwrap()).unwrap();
    other.custom_path = Some(tmp.clone());
    other.general.animate_gifs = false;
    other.save().unwrap();

    // This instance changed the theme but has not flushed yet.
    state.settings.general.theme = "Dark".to_string();
    state.settings.mark_dirty();

    state.settings_reload_at = Instant::now();
    let _task = update(&mut state, Message::Tick(Instant::now()));

    // The unflushed local change survived the reload, the external change
    // was adopted, and the tick's dirty flush persisted the merged result.
    assert_eq!(state.settings.general.theme, "Dark");
    assert!(!state.settings.general.animate_gifs);
    let merged: SettingsStore = toml::from_str(&std::fs::read_to_string(&tmp).unwrap()).unwrap();
    assert_eq!(merged.general.theme, "Dark");
    assert!(!merged.general.animate_gifs);

    let _ = std::fs::remove_file(&tmp);
}

#[cfg(not(feature = "demo"))]
#[test]
fn test_tick_session_pinned_folders_are_not_synced() {
    let tmp = std::env::temp_dir().join(format!(
        "mediasort_tick_session_pins_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    let settings = SettingsStore {
        custom_path: Some(tmp.clone()),
        ..SettingsStore::default()
    };
    let mut state = AppState::new(settings);
    state.settings.save().unwrap();

    // A second instance enables per-session pins and pins its own folder.
    let mut other: SettingsStore = toml::from_str(&std::fs::read_to_string(&tmp).unwrap()).unwrap();
    other.custom_path = Some(tmp.clone());
    other.general.session_pinned_folders = true;
    other.pinned_folders.paths = vec!["/other/pin".to_string()];
    other.save().unwrap();

    state.settings_reload_at = Instant::now();
    let _task = update(&mut state, Message::Tick(Instant::now()));

    // The toggle itself is adopted; the pins are not.
    assert!(state.settings.general.session_pinned_folders);
    assert!(state.settings.pinned_folders.paths.is_empty());
    assert!(state.folder.pinned_folders.is_empty());

    let _ = std::fs::remove_file(&tmp);
}

// ============================================================================
// Settings message handler tests
// ============================================================================

#[test]
fn test_settings_show_dialog() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(matches!(state.settings_ui, SettingsUiState::Hidden));
    let _task = update(&mut state, Message::Settings(SettingsMessage::Open));
    assert!(!matches!(state.settings_ui, SettingsUiState::Hidden));
}

#[test]
fn test_settings_close_dialog() {
    let mut state = AppState::new(SettingsStore::default());
    state.settings_ui = SettingsUiState::Settings;
    let _task = update(&mut state, Message::Settings(SettingsMessage::Close));
    assert!(matches!(state.settings_ui, SettingsUiState::Hidden));
}

#[test]
fn test_settings_toggle_metadata_panel() {
    let mut state = AppState::new(SettingsStore::default());
    let was_expanded = state.metadata.panel_expanded;
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleMetadataPanel),
    );
    assert_eq!(state.metadata.panel_expanded, !was_expanded);
}

#[test]
fn test_settings_toggle_animate_gifs() {
    let mut state = AppState::new(SettingsStore::default());
    let was_animating = state.settings.general.animate_gifs;
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleAnimateGifs),
    );
    assert_eq!(state.settings.general.animate_gifs, !was_animating);
}

#[test]
fn test_settings_toggle_session_pinned_folders() {
    let mut state = AppState::new(SettingsStore::default());
    let was_session = state.settings.general.session_pinned_folders;
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleSessionPinnedFolders),
    );
    assert_eq!(state.settings.general.session_pinned_folders, !was_session);
    assert!(state.settings.dirty);
}

#[test]
fn test_settings_open_advanced_tab() {
    let mut state = AppState::new(SettingsStore::default());
    let _task = update(&mut state, Message::Settings(SettingsMessage::OpenAdvanced));
    assert!(matches!(state.settings_ui, SettingsUiState::Advanced));
}

#[test]
fn test_settings_toggle_hardware_decoding() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.settings.advanced.disable_hardware_decoding);
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleHardwareDecoding),
    );
    assert!(state.settings.advanced.disable_hardware_decoding);
    assert!(state.settings.dirty);
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleHardwareDecoding),
    );
    assert!(!state.settings.advanced.disable_hardware_decoding);
}

#[test]
fn test_settings_toggle_reopen_folder() {
    let mut state = AppState::new(SettingsStore::default());
    let initial = state.settings.general.reopen_last_opened_folder;
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleReopenFolder),
    );
    assert_eq!(state.settings.general.reopen_last_opened_folder, !initial);
}

#[test]
fn test_settings_toggle_reopen_media() {
    let mut state = AppState::new(SettingsStore::default());
    let initial = state.settings.general.reopen_last_selected_media;
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleReopenMedia),
    );
    assert_eq!(state.settings.general.reopen_last_selected_media, !initial);
}

#[test]
fn test_reopen_last_selected_media_behavior() {
    use std::sync::mpsc::channel;

    let mut state = AppState::new(SettingsStore::default());
    state.settings.general.reopen_last_opened_folder = true;
    state.settings.general.reopen_last_selected_media = true;

    let file1 = std::path::PathBuf::from("a.jpg");
    let file2 = std::path::PathBuf::from("b.jpg");
    state.settings.general.last_selected_media = Some(file2.to_string_lossy().to_string());

    // Setup mock scan channel
    let (tx, rx) = channel();
    tx.send(file1.clone()).unwrap();
    tx.send(file2.clone()).unwrap();
    drop(tx);
    state.media_grid.scan_receiver = Some(rx);
    state.media_grid.pending_select_index = Some(0);

    // Run poll_background_channels (which processes scan completions)
    let _task = poll_background_channels(&mut state);

    // Verify b.jpg (index 1) is selected because both settings are true
    assert_eq!(state.media_grid.selected_index, Some(1));

    // Test case 2: reopen_last_opened_folder is false
    state.settings.general.reopen_last_opened_folder = false;
    state.settings.general.reopen_last_selected_media = true;
    state.media_grid.selected_index = None;
    let (tx, rx) = channel();
    tx.send(file1.clone()).unwrap();
    tx.send(file2.clone()).unwrap();
    drop(tx);
    state.media_grid.scan_receiver = Some(rx);
    state.media_grid.pending_select_index = Some(0);
    let _task = poll_background_channels(&mut state);
    // Should fall back to index 0
    assert_eq!(state.media_grid.selected_index, Some(0));

    // Test case 3: reopen_last_selected_media is false
    state.settings.general.reopen_last_opened_folder = true;
    state.settings.general.reopen_last_selected_media = false;
    state.media_grid.selected_index = None;
    let (tx, rx) = channel();
    tx.send(file1.clone()).unwrap();
    tx.send(file2.clone()).unwrap();
    drop(tx);
    state.media_grid.scan_receiver = Some(rx);
    state.media_grid.pending_select_index = Some(0);
    let _task = poll_background_channels(&mut state);
    // Should fall back to index 0
    assert_eq!(state.media_grid.selected_index, Some(0));
}

#[test]
fn test_settings_set_theme() {
    let mut state = AppState::new(SettingsStore::default());
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::SetTheme("Slate".into())),
    );
    assert_eq!(state.settings.general.theme, "Slate");
}

#[test]
fn test_settings_set_theme_unknown() {
    let mut state = AppState::new(SettingsStore::default());
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::SetTheme("UnknownTheme".into())),
    );
    assert_eq!(state.settings.general.theme, "UnknownTheme");
}

#[test]
fn test_system_theme_changed() {
    let mut state = AppState::new(SettingsStore::default());
    assert_eq!(state.system_theme, iced::theme::Mode::None);
    let _task = update(
        &mut state,
        Message::SystemThemeChanged(iced::theme::Mode::Dark),
    );
    assert_eq!(state.system_theme, iced::theme::Mode::Dark);
    let _task = update(
        &mut state,
        Message::SystemThemeChanged(iced::theme::Mode::Light),
    );
    assert_eq!(state.system_theme, iced::theme::Mode::Light);
}

#[test]
fn test_auto_theme_follows_system_mode() {
    let mut state = AppState::new(SettingsStore::default());
    assert_eq!(state.settings.general.theme, "Auto");

    // Before the OS preference resolves, Auto falls back to Light.
    assert_eq!(crate::app::theme(&state), iced::Theme::Light);

    state.system_theme = iced::theme::Mode::Dark;
    assert_eq!(crate::app::theme(&state), iced::Theme::Dark);

    state.system_theme = iced::theme::Mode::Light;
    assert_eq!(crate::app::theme(&state), iced::Theme::Light);
}

#[test]
fn test_explicit_theme_ignores_system_mode() {
    let mut state = AppState::new(SettingsStore::default());
    state.settings.general.theme = "Dark".to_string();
    state.system_theme = iced::theme::Mode::Light;
    assert_eq!(crate::app::theme(&state), iced::Theme::Dark);
}

#[test]
fn test_settings_open_keybindings() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(matches!(state.settings_ui, SettingsUiState::Hidden));
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::OpenKeybindings),
    );
    assert!(matches!(
        state.settings_ui,
        SettingsUiState::Keybindings { .. }
    ));
}

#[test]
fn test_settings_start_drag_folder_divider() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.folder.dragging_folder_divider);
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::StartDragFolderDivider),
    );
    assert!(state.folder.dragging_folder_divider);
}

#[test]
fn test_settings_start_drag_metadata_divider() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.metadata.dragging_divider);
    let _task = update(
        &mut state,
        Message::Settings(SettingsMessage::StartDragMetadataDivider),
    );
    assert!(state.metadata.dragging_divider);
}

// ============================================================================
// L10n tests
// ============================================================================

#[test]
fn test_l10n_init_with_en() {
    let l10n = media_sort_core::l10n::Localization::init("en");
    let greeting = l10n.tr("keybindings-search-images");
    assert!(!greeting.is_empty());
}

#[test]
fn test_l10n_init_with_de() {
    let l10n = media_sort_core::l10n::Localization::init("de");
    let greeting = l10n.tr("keybindings-search-images");
    assert!(!greeting.is_empty());
}

#[test]
fn test_l10n_init_with_unknown_falls_back() {
    let l10n = media_sort_core::l10n::Localization::init("xx");
    let greeting = l10n.tr("keybindings-search-images");
    assert!(!greeting.is_empty());
}

#[test]
fn test_l10n_missing_key_returns_key() {
    let l10n = media_sort_core::l10n::Localization::init("en");
    let result = l10n.tr("this_key_does_not_exist_anywhere");
    assert_eq!(result, "this_key_does_not_exist_anywhere");
}

#[test]
fn test_l10n_locale_stored() {
    let l10n = media_sort_core::l10n::Localization::init("de");
    assert_eq!(l10n.locale(), "de");
}

#[test]
fn test_l10n_set_locale_switches_language() {
    let mut l10n = media_sort_core::l10n::Localization::init("en");
    l10n.set_locale("de");
    assert_eq!(l10n.locale(), "de");
}

#[test]
fn test_l10n_detect_locale_returns_string() {
    let locale = media_sort_core::l10n::detect_locale();
    assert!(!locale.is_empty());
}

mod filesystem_watcher;
