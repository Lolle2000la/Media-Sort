use super::*;
use media_sort_core::models::FolderNode;

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
        editing_keybinding: Some(KeyAction::MoveToFolder),
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
        editing_keybinding: Some(KeyAction::Rename),
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
fn test_key_action_remapping_via_edit_keybinding_and_dispatch() {
    let mut state = AppState::new(SettingsStore::default());
    let _ = update(
        &mut state,
        Message::Settings(SettingsMessage::OpenKeybindings),
    );
    let _ = update(
        &mut state,
        Message::Settings(SettingsMessage::EditKeyBinding(KeyAction::Rename)),
    );
    assert!(matches!(
        state.settings_ui,
        SettingsUiState::Keybindings {
            editing_keybinding: Some(KeyAction::Rename),
            waiting_for_key: true,
        }
    ));

    let _ = update(
        &mut state,
        Message::KeyCaptured(Key::Character('N'), false, false, false),
    );
    assert_eq!(state.settings.keybindings.rename.key, Key::Character('N'));
    assert!(state.settings.dirty);
    assert_eq!(
        crate::subscriptions::keyboard::find_matching_action(
            &state.settings.keybindings,
            Key::Character('N'),
            false,
            false,
            false,
        ),
        Some(KeyAction::Rename)
    );
    assert_eq!(
        crate::subscriptions::keyboard::find_matching_action(
            &state.settings.keybindings,
            Key::Character('R'),
            false,
            false,
            false,
        ),
        None
    );
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

#[test]
fn test_key_captured_waiting_for_key_clears_state() {
    let mut state = AppState::new(SettingsStore::default());
    state.settings_ui = SettingsUiState::Keybindings {
        editing_keybinding: Some(KeyAction::MoveToFolder),
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

#[test]
fn test_modal_gating_blocks_shortcuts_when_settings_or_credits_open() {
    let mut state = AppState::new(SettingsStore::default());
    let p_a = PathBuf::from("/a");
    let p_b = PathBuf::from("/b");
    state.folder.folder_tree = vec![
        FolderNode {
            path: p_a.clone(),
            name: "a".into(),
            ..FolderNode::default()
        },
        FolderNode {
            path: p_b.clone(),
            name: "b".into(),
            ..FolderNode::default()
        },
    ];
    state.folder.set_selected(p_a.clone(), 0);
    state.media_grid.entries = vec![make_media_entry("/song.mp3", MediaType::Audio)];
    state.media_grid.selected_index = Some(0);
    state.audio.volume = 50.0;

    assert!(!is_modal_open(&state));

    // Settings open: global navigation / playback / file shortcuts are blocked.
    state.settings_ui = SettingsUiState::Settings;
    assert!(is_modal_open(&state));
    let _ = handle_key_captured(&mut state, Key::Character('S'), false, false, false);
    assert_eq!(state.folder.selected_folder, Some(p_a.clone()));
    let _ = handle_key_captured(&mut state, Key::AudioVolumeUp, false, false, false);
    assert_eq!(state.audio.volume, 50.0);

    // Credits open: also blocks global shortcuts.
    state.settings_ui = SettingsUiState::Hidden;
    state.show_credits = true;
    assert!(is_modal_open(&state));
    let _ = handle_key_captured(&mut state, Key::Character('S'), false, false, false);
    assert_eq!(state.folder.selected_folder, Some(p_a));
    let _ = handle_key_captured(&mut state, Key::AudioVolumeUp, false, false, false);
    assert_eq!(state.audio.volume, 50.0);

    // Closing modals restores normal dispatch.
    state.show_credits = false;
    assert!(!is_modal_open(&state));
    let _ = handle_key_captured(&mut state, Key::Character('S'), false, false, false);
    assert_eq!(state.folder.selected_folder, Some(p_b));
}

#[test]
fn test_space_and_media_keys_route_by_active_media_type() {
    let mut state = AppState::new(SettingsStore::default());
    let (ready_msg, mut video_rx) = iced_mpv::testing::ready(16);
    let _ = state.video.update(ready_msg);

    state.media_grid.entries = vec![
        make_media_entry("/track.mp3", MediaType::Audio),
        make_media_entry("/clip.mp4", MediaType::Video),
        make_media_entry("/photo.jpg", MediaType::Image),
    ];

    // Audio selected: volume/mute/stop route to AudioMessage::* and never touch video.
    state.media_grid.selected_index = Some(0);
    state.audio.volume = 50.0;
    state.audio.muted = false;
    state.audio.playing = true;

    let _ = handle_key_captured(&mut state, Key::AudioVolumeUp, false, false, false);
    assert_eq!(state.audio.volume, 55.0);
    assert!(video_rx.try_recv().is_err());

    let _ = handle_key_captured(&mut state, Key::AudioVolumeDown, false, false, false);
    assert_eq!(state.audio.volume, 50.0);

    let _ = handle_key_captured(&mut state, Key::AudioVolumeMute, false, false, false);
    assert!(state.audio.muted);

    let _ = handle_key_captured(&mut state, Key::MediaStop, false, false, false);
    assert!(!state.audio.playing);
    assert!(video_rx.try_recv().is_err());

    // Video selected: Space / media keys emit VideoCommands and do not touch audio.
    state.media_grid.selected_index = Some(1);
    let _ = handle_key_captured(&mut state, Key::AudioVolumeUp, false, false, false);
    assert_eq!(state.audio.volume, 50.0);
    assert!(matches!(
        video_rx.try_recv(),
        Ok(iced_mpv::VideoCommand::SetVolume(v)) if (v - 100.0).abs() < f64::EPSILON
    ));

    let _ = handle_key_captured(&mut state, Key::AudioVolumeDown, false, false, false);
    assert!(matches!(
        video_rx.try_recv(),
        Ok(iced_mpv::VideoCommand::SetVolume(v)) if (v - 95.0).abs() < f64::EPSILON
    ));

    let _ = handle_key_captured(&mut state, Key::Space, false, false, false);
    assert!(matches!(
        video_rx.try_recv(),
        Ok(iced_mpv::VideoCommand::TogglePause)
    ));

    let _ = handle_key_captured(&mut state, Key::MediaStop, false, false, false);
    assert!(matches!(
        video_rx.try_recv(),
        Ok(iced_mpv::VideoCommand::Stop)
    ));

    // Image selected (or no selection): Space & media keys are no-ops.
    state.media_grid.selected_index = Some(2);
    let audio_vol_before = state.audio.volume;
    let _ = handle_key_captured(&mut state, Key::AudioVolumeUp, false, false, false);
    let _ = handle_key_captured(&mut state, Key::Space, false, false, false);
    assert_eq!(state.audio.volume, audio_vol_before);
    assert!(video_rx.try_recv().is_err());
}

#[test]
fn test_filter_mode_leaves_plain_digits_free_when_inactive() {
    let (root, file, dest) = setup_temp_dir_with_files("filter_mode_digits_free");
    let mut state = AppState::new(SettingsStore::default());
    state.open_folder(&root);
    drain_async_scan(&mut state);
    state.pin_folder(&dest);
    state.media_grid.selected_index = Some(0);

    for ch in ['0', '1', '2', '3', '4'] {
        assert_eq!(
            crate::subscriptions::keyboard::find_matching_action(
                &state.settings.keybindings,
                Key::Character(ch),
                false,
                false,
                false,
            ),
            None
        );
        let _ = handle_key_captured(&mut state, Key::Character(ch), false, false, false);
        assert!(!state.media_grid.filter_mode.active);
        assert!(state.media_grid.type_filter.images);
        assert!(state.media_grid.type_filter.video);
        assert!(state.media_grid.type_filter.audio);
        assert!(state.media_grid.type_filter.documents);
    }

    let task = handle_key_captured(&mut state, Key::Character('1'), false, false, true);
    let _ = task;
    let _ = update(&mut state, Message::Folder(FolderMessage::PinShortcut(1)));
    assert!(!file.exists());
    assert!(dest.join("test_image.jpg").exists());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_filter_mode_enter_toggle_digits_arrows_reset_and_exit() {
    let mut state = AppState::new(SettingsStore::default());
    let p_a = PathBuf::from("/a");
    let p_b = PathBuf::from("/b");
    state.folder.folder_tree = vec![
        FolderNode {
            path: p_a.clone(),
            name: "a".into(),
            ..FolderNode::default()
        },
        FolderNode {
            path: p_b,
            name: "b".into(),
            ..FolderNode::default()
        },
    ];
    state.folder.set_selected(p_a.clone(), 0);
    state.media_grid.entries = vec![
        make_media_entry("/test/photo.jpg", MediaType::Image),
        make_media_entry("/test/clip.mp4", MediaType::Video),
        make_media_entry("/test/song.mp3", MediaType::Audio),
        make_media_entry("/test/notes.txt", MediaType::Document),
    ];
    state.media_grid.rebuild_lower_names();
    state.media_grid.selected_index = Some(0);

    // 'T' enters Filter Mode with focus on chip 0 (Images).
    let _ = handle_key_captured(&mut state, Key::Character('T'), false, false, false);
    assert!(state.media_grid.filter_mode.active);
    assert_eq!(state.media_grid.filter_mode.focused_chip, 0);

    // '1' and '3' toggle Images and Audio while staying in Filter Mode.
    let _ = handle_key_captured(&mut state, Key::Character('1'), false, false, false);
    assert!(state.media_grid.filter_mode.active);
    assert_eq!(state.media_grid.filter_mode.focused_chip, 0);
    assert!(!state.media_grid.type_filter.images);

    let _ = handle_key_captured(&mut state, Key::Character('3'), false, false, false);
    assert!(state.media_grid.filter_mode.active);
    assert_eq!(state.media_grid.filter_mode.focused_chip, 2);
    assert!(!state.media_grid.type_filter.audio);
    assert_eq!(state.media_grid.filtered_entries().len(), 2);

    // ArrowRight / ArrowLeft wrap focused_chip (0..=3); Space / Enter toggle focused chip.
    let _ = handle_key_captured(&mut state, Key::ArrowRight, false, false, false);
    assert_eq!(state.media_grid.filter_mode.focused_chip, 3);
    let _ = handle_key_captured(&mut state, Key::ArrowRight, false, false, false);
    assert_eq!(state.media_grid.filter_mode.focused_chip, 0);
    let _ = handle_key_captured(&mut state, Key::ArrowLeft, false, false, false);
    assert_eq!(state.media_grid.filter_mode.focused_chip, 3);

    // Toggle Documents (chip 3) off with Space, then Video (chip 1) off with Enter.
    let _ = handle_key_captured(&mut state, Key::Space, false, false, false);
    assert!(!state.media_grid.type_filter.documents);
    let _ = handle_key_captured(&mut state, Key::ArrowRight, false, false, false);
    let _ = handle_key_captured(&mut state, Key::ArrowRight, false, false, false);
    assert_eq!(state.media_grid.filter_mode.focused_chip, 1);
    let _ = handle_key_captured(&mut state, Key::Enter, false, false, false);
    assert!(!state.media_grid.type_filter.video);
    assert!(state.media_grid.filtered_entries().is_empty());
    assert_eq!(state.media_grid.selected_index, None);

    // '0' resets all 4 filters to enabled.
    let _ = handle_key_captured(&mut state, Key::Character('0'), false, false, false);
    assert!(state.media_grid.type_filter.is_all_enabled());
    assert_eq!(state.media_grid.filtered_entries().len(), 4);
    assert_eq!(state.media_grid.selected_index, Some(0));

    // Unrelated keys do not mutate folder/media while Filter Mode is active.
    let _ = handle_key_captured(&mut state, Key::Character('S'), false, false, false);
    let _ = handle_key_captured(&mut state, Key::Delete, false, false, false);
    assert_eq!(state.folder.selected_folder, Some(p_a));
    assert_eq!(state.media_grid.entries.len(), 4);

    // Esc exits Filter Mode, after which '1' is ignored again.
    let _ = handle_key_captured(&mut state, Key::Escape, false, false, false);
    assert!(!state.media_grid.filter_mode.active);
    let _ = handle_key_captured(&mut state, Key::Character('1'), false, false, false);
    assert!(state.media_grid.type_filter.images);

    // 'T' enters and 'T' again exits Filter Mode.
    let _ = handle_key_captured(&mut state, Key::Character('T'), false, false, false);
    assert!(state.media_grid.filter_mode.active);
    let _ = handle_key_captured(&mut state, Key::Character('T'), false, false, false);
    assert!(!state.media_grid.filter_mode.active);
}

#[test]
fn test_filter_mode_blocked_when_search_focused_or_modal_open_and_search_exits_mode() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![
        make_media_entry("/a.jpg", MediaType::Image),
        make_media_entry("/b.mp4", MediaType::Video),
    ];
    state.media_grid.rebuild_lower_names();

    // Entering Filter Mode, then focusing search exits Filter Mode.
    let _ = handle_key_captured(&mut state, Key::Character('T'), false, false, false);
    assert!(state.media_grid.filter_mode.active);
    let _ = update(&mut state, Message::Media(MediaMessage::SearchFocused));
    assert!(!state.media_grid.filter_mode.active);
    assert!(state.media_grid.search.focused);

    // 'T' is blocked while search is focused.
    let _ = handle_key_captured(&mut state, Key::Character('T'), false, false, false);
    assert!(!state.media_grid.filter_mode.active);

    // 'T' is blocked while a modal is open.
    state.media_grid.search.focused = false;
    state.settings_ui = SettingsUiState::Settings;
    assert!(is_modal_open(&state));
    let _ = handle_key_captured(&mut state, Key::Character('T'), false, false, false);
    assert!(!state.media_grid.filter_mode.active);

    // Closing modal restores 'T' dispatch.
    state.settings_ui = SettingsUiState::Hidden;
    let _ = handle_key_captured(&mut state, Key::Character('T'), false, false, false);
    assert!(state.media_grid.filter_mode.active);
}

#[test]
fn test_remapped_filter_mode_key_enters_and_exits() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![
        make_media_entry("/a.jpg", MediaType::Image),
        make_media_entry("/b.mp4", MediaType::Video),
    ];
    state.media_grid.rebuild_lower_names();

    let _ = update(
        &mut state,
        Message::Settings(SettingsMessage::OpenKeybindings),
    );
    let _ = update(
        &mut state,
        Message::Settings(SettingsMessage::EditKeyBinding(KeyAction::ToggleFilterMode)),
    );
    let _ = update(
        &mut state,
        Message::KeyCaptured(Key::F5, false, false, false),
    );
    assert_eq!(state.settings.keybindings.toggle_filter_mode.key, Key::F5);
    assert!(state.settings.dirty);

    // Close settings modal so global shortcuts are active.
    state.settings_ui = SettingsUiState::Hidden;

    // Default 'T' is now inactive.
    let _ = handle_key_captured(&mut state, Key::Character('T'), false, false, false);
    assert!(!state.media_grid.filter_mode.active);

    // Remapped F5 enters and exits Filter Mode.
    let _ = handle_key_captured(&mut state, Key::F5, false, false, false);
    assert!(state.media_grid.filter_mode.active);
    let _ = handle_key_captured(&mut state, Key::Character('1'), false, false, false);
    assert!(!state.media_grid.type_filter.images);
    let _ = handle_key_captured(&mut state, Key::F5, false, false, false);
    assert!(!state.media_grid.filter_mode.active);
}
