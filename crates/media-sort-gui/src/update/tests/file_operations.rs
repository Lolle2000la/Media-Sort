use super::*;

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
fn test_pin_shortcut_moves_selected_entry_and_updates_history() {
    let (root, file, dest) = setup_temp_dir_with_files("pin_shortcut_move");

    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    drain_async_scan(&mut state);
    state.pin_folder(&dest);
    state.media_grid.selected_index = Some(0);

    let _task = update(&mut state, Message::Folder(FolderMessage::PinShortcut(1)));

    assert!(!file.exists());
    assert!(dest.join("test_image.jpg").exists());
    assert!(state.history.can_undo());
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
