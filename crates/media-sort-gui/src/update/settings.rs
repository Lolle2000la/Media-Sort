use iced::Task;

use crate::message::{Message, SettingsMessage};
use crate::state::{AppState, SettingsUiState};

pub fn handle_settings_message(state: &mut AppState, msg: SettingsMessage) -> Task<Message> {
    match msg {
        SettingsMessage::ToggleMetadataPanel => {
            state.metadata.panel_expanded = !state.metadata.panel_expanded;
            state.settings.metadata_panel.is_expanded = state.metadata.panel_expanded;
            state.settings.mark_dirty();
            Task::none()
        }
        SettingsMessage::EditKeyBinding(action) => {
            if let SettingsUiState::Keybindings {
                editing_keybinding,
                waiting_for_key,
            } = &mut state.settings_ui
            {
                *editing_keybinding = Some(action);
                *waiting_for_key = true;
            }
            Task::none()
        }
        SettingsMessage::Open => {
            state.settings_ui = SettingsUiState::Settings;
            Task::none()
        }
        SettingsMessage::Close => {
            state.settings_ui = SettingsUiState::Hidden;
            Task::done(Message::Settings(SettingsMessage::Save))
        }
        SettingsMessage::ChangeLanguage(locale) => {
            state.settings.general.locale = Some(locale);
            state.settings.mark_dirty();
            apply_locale_if_changed(state);
            Task::none()
        }
        SettingsMessage::SetTheme(theme) => {
            state.settings.general.theme = theme;
            state.settings.mark_dirty();
            Task::none()
        }
        SettingsMessage::ToggleReopenFolder => {
            state.settings.general.reopen_last_opened_folder =
                !state.settings.general.reopen_last_opened_folder;
            state.settings.mark_dirty();
            Task::none()
        }
        SettingsMessage::ToggleReopenMedia => {
            state.settings.general.reopen_last_selected_media =
                !state.settings.general.reopen_last_selected_media;
            state.settings.mark_dirty();
            Task::none()
        }
        SettingsMessage::StartDragFolderDivider => {
            state.folder.dragging_folder_divider = true;
            Task::none()
        }
        SettingsMessage::StartDragMetadataDivider => {
            state.metadata.dragging_divider = true;
            Task::none()
        }
        SettingsMessage::ToggleAnimateGifs => {
            state.settings.general.animate_gifs = !state.settings.general.animate_gifs;
            state.settings.mark_dirty();
            Task::none()
        }
        SettingsMessage::ToggleSessionPinnedFolders => {
            state.settings.general.session_pinned_folders =
                !state.settings.general.session_pinned_folders;
            state.settings.mark_dirty();
            Task::none()
        }
        SettingsMessage::Save => {
            state.settings.mark_dirty();
            Task::none()
        }
        SettingsMessage::OpenKeybindings => {
            state.settings_ui = SettingsUiState::Keybindings {
                editing_keybinding: None,
                waiting_for_key: false,
            };
            Task::none()
        }
        SettingsMessage::OpenAdvanced => {
            state.settings_ui = SettingsUiState::Advanced;
            Task::none()
        }
        SettingsMessage::ToggleHardwareDecoding => {
            state.settings.advanced.disable_hardware_decoding =
                !state.settings.advanced.disable_hardware_decoding;
            state.settings.mark_dirty();
            Task::none()
        }
        SettingsMessage::RestoreDefaultKeyBindings => {
            state.settings.keybindings =
                media_sort_core::settings::keybindings::KeyBindings::default();
            state.settings.mark_dirty();
            Task::none()
        }
        #[cfg(feature = "velopack")]
        SettingsMessage::ToggleCheckForUpdates => {
            state.settings.general.check_for_updates_on_startup =
                !state.settings.general.check_for_updates_on_startup;
            state.settings.mark_dirty();
            Task::none()
        }
        #[cfg(feature = "velopack")]
        SettingsMessage::ToggleInstallPrerelease => {
            state.settings.general.install_prerelease_builds =
                !state.settings.general.install_prerelease_builds;
            state.settings.mark_dirty();
            Task::none()
        }
        #[cfg(target_os = "windows")]
        SettingsMessage::ToggleIntegrationWithWindows => {
            state.settings.general.integration_with_windows =
                !state.settings.general.integration_with_windows;
            let enabled = state.settings.general.integration_with_windows;
            state.settings.mark_dirty();

            if enabled {
                if let Ok(exe) = std::env::current_exe()
                    && let Some(exe_str) = exe.to_str()
                {
                    let _ = media_sort_backend::platform::windows_shell::register(exe_str);
                }
            } else {
                let _ = media_sort_backend::platform::windows_shell::unregister();
            }
            Task::none()
        }
    }
}

pub fn handle_settings_loaded(
    state: &mut AppState,
    result: Box<Result<media_sort_core::settings::store::SettingsStore, String>>,
) -> Task<Message> {
    match *result {
        Ok(settings) => {
            state.settings = settings;
            #[cfg(target_os = "windows")]
            {
                if state.settings.general.integration_with_windows
                    && let Ok(exe) = std::env::current_exe()
                    && let Some(exe_str) = exe.to_str()
                {
                    let _ = media_sort_backend::platform::windows_shell::register(exe_str);
                }
            }
            Task::none()
        }
        Err(err) => {
            tracing::error!("Failed to load settings: {err}");
            Task::none()
        }
    }
}

/// Mirrors a settings store that was replaced by an external reload
/// (`SettingsStore::reload_from_disk`) into the derived UI state: the
/// locale (and localized placeholders), the metadata-panel expansion
/// mirror and the pinned-folder list. Plain fields (theme, keybindings,
/// tree width, …) are read live from `state.settings` by the view and
/// subscription layers and need no mirroring.
#[cfg(not(feature = "demo"))]
pub fn apply_external_settings_changes(state: &mut AppState) {
    apply_locale_if_changed(state);
    state.metadata.panel_expanded = state.settings.metadata_panel.is_expanded;
    state.sync_pinned_folders_from_settings();
}

/// Applies the locale selected in the settings store to the fluent bundle
/// and re-derives the localized placeholder strings.
fn apply_locale_if_changed(state: &mut AppState) {
    if let Some(ref locale) = state.settings.general.locale
        && state.l10n.locale() != *locale
    {
        state.l10n.set_locale(locale);
        state.media_grid.search.placeholder = state.l10n.tr("keybindings-search-images");
        state.rename.placeholder = state.l10n.tr("ui-enter-new-name");
        state.create_folder.create_folder_placeholder = state.l10n.tr("ui-folder-name-placeholder");
    }
}
