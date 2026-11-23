use super::*;

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

    let mut other: SettingsStore = toml::from_str(&std::fs::read_to_string(&tmp).unwrap()).unwrap();
    other.custom_path = Some(tmp.clone());
    other.general.theme = "Dark".to_string();
    other.general.locale = Some("de".to_string());
    other.pinned_folders.paths = vec!["/tmp/pinned".to_string()];
    other.save().unwrap();

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

    let mut other: SettingsStore = toml::from_str(&std::fs::read_to_string(&tmp).unwrap()).unwrap();
    other.custom_path = Some(tmp.clone());
    other.general.animate_gifs = false;
    other.save().unwrap();

    state.settings.general.theme = "Dark".to_string();
    state.settings.mark_dirty();

    state.settings_reload_at = Instant::now();
    let _task = update(&mut state, Message::Tick(Instant::now()));

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

    let mut other: SettingsStore = toml::from_str(&std::fs::read_to_string(&tmp).unwrap()).unwrap();
    other.custom_path = Some(tmp.clone());
    other.general.session_pinned_folders = true;
    other.pinned_folders.paths = vec!["/other/pin".to_string()];
    other.save().unwrap();

    state.settings_reload_at = Instant::now();
    let _task = update(&mut state, Message::Tick(Instant::now()));

    assert!(state.settings.general.session_pinned_folders);
    assert!(state.settings.pinned_folders.paths.is_empty());
    assert!(state.folder.pinned_folders.is_empty());

    let _ = std::fs::remove_file(&tmp);
}

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
