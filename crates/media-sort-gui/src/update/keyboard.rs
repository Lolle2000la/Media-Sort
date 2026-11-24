use iced::Task;
use media_sort_core::media_type::MediaType;
use media_sort_core::models::MediaEntry;
use media_sort_core::settings::keybindings::{Key, KeyAction};

use crate::message::{
    AudioMessage, DocumentMessage, FolderMessage, MediaMessage, Message, SettingsMessage,
};
use crate::state::{AppState, FilterModeState, SettingsUiState};
use crate::subscriptions::keyboard;

/// Whether a top-level modal dialog (settings, credits, or update prompt) is
/// currently open and should block global navigation/file shortcuts.
pub fn is_modal_open(state: &AppState) -> bool {
    state.settings_ui != SettingsUiState::Hidden
        || state.show_credits
        || state.is_update_prompt_visible()
}

fn active_entry(state: &AppState) -> Option<MediaEntry> {
    let index = state.media_grid.selected_index?;
    state
        .media_grid
        .filtered_entries()
        .get(index)
        .copied()
        .cloned()
}

fn active_media_type(state: &AppState) -> Option<MediaType> {
    let index = state.media_grid.selected_index?;
    state
        .media_grid
        .filtered_entries()
        .get(index)
        .map(|entry| entry.media_type)
}

fn handle_playback_key(state: &mut AppState, key: Key) -> Task<Message> {
    match (active_media_type(state), key) {
        (
            Some(MediaType::Audio),
            Key::Space | Key::MediaPlayPause | Key::MediaPlay | Key::MediaPause,
        ) => super::audio::handle_audio_message(state, AudioMessage::PlayPause),
        (Some(MediaType::Audio), Key::MediaStop) => {
            super::audio::handle_audio_message(state, AudioMessage::Stop)
        }
        (Some(MediaType::Audio), Key::AudioVolumeUp) => super::audio::handle_audio_message(
            state,
            AudioMessage::SetVolume((state.audio.volume + 5.0).min(100.0)),
        ),
        (Some(MediaType::Audio), Key::AudioVolumeDown) => super::audio::handle_audio_message(
            state,
            AudioMessage::SetVolume((state.audio.volume - 5.0).max(0.0)),
        ),
        (Some(MediaType::Audio), Key::AudioVolumeMute) => {
            super::audio::handle_audio_message(state, AudioMessage::ToggleMute)
        }
        (
            Some(MediaType::Video),
            Key::Space | Key::MediaPlayPause | Key::MediaPlay | Key::MediaPause,
        ) => {
            state.video.toggle_pause();
            Task::none()
        }
        (Some(MediaType::Video), Key::MediaStop) => {
            state.video.stop();
            Task::none()
        }
        (Some(MediaType::Video), Key::AudioVolumeUp) => {
            state.video.set_volume(state.video.volume() + 5.0);
            Task::none()
        }
        (Some(MediaType::Video), Key::AudioVolumeDown) => {
            state.video.set_volume(state.video.volume() - 5.0);
            Task::none()
        }
        (Some(MediaType::Video), Key::AudioVolumeMute) => {
            state.video.toggle_mute();
            Task::none()
        }
        _ => Task::none(),
    }
}

pub fn handle_key_captured(
    state: &mut AppState,
    key: Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> Task<Message> {
    let waiting = match &state.settings_ui {
        SettingsUiState::Keybindings {
            editing_keybinding,
            waiting_for_key,
        } if *waiting_for_key => *editing_keybinding,
        _ => None,
    };

    if let Some(action) = waiting {
        state.settings_ui = SettingsUiState::Keybindings {
            editing_keybinding: None,
            waiting_for_key: false,
        };
        keyboard::set_keybinding(
            &mut state.settings.keybindings,
            action,
            key,
            ctrl,
            shift,
            alt,
        );
        state.settings.mark_dirty();
        return Task::none();
    }

    if state.rename.path.is_some() {
        return match key {
            Key::Enter => Task::done(Message::Media(MediaMessage::SubmitRename)),
            Key::Escape => Task::done(Message::Media(MediaMessage::CancelRename)),
            _ => Task::none(),
        };
    }

    if state.create_folder.creating_folder_parent.is_some() {
        return match key {
            Key::Enter => Task::done(Message::Folder(FolderMessage::SubmitCreate(
                std::path::PathBuf::new(),
            ))),
            Key::Escape => Task::done(Message::Folder(FolderMessage::CancelCreate)),
            _ => Task::none(),
        };
    }

    if is_modal_open(state) {
        return Task::none();
    }

    if state.media_grid.search.focused {
        return match key {
            Key::Enter | Key::Escape | Key::Tab => {
                Task::done(Message::Media(MediaMessage::SearchBlurred))
            }
            _ => Task::none(),
        };
    }

    if ctrl && key == Key::Character('Q') {
        return Task::done(Message::Quit);
    }

    if state.media_grid.filter_mode.active {
        return handle_filter_mode_key(state, key, ctrl, shift, alt);
    }

    if matches!(
        key,
        Key::Space
            | Key::MediaPlayPause
            | Key::MediaPlay
            | Key::MediaPause
            | Key::MediaStop
            | Key::AudioVolumeUp
            | Key::AudioVolumeDown
            | Key::AudioVolumeMute
    ) {
        return handle_playback_key(state, key);
    }

    match key {
        Key::MediaTrackNext => return Task::done(Message::Media(MediaMessage::GoRight)),
        Key::MediaTrackPrevious => return Task::done(Message::Media(MediaMessage::GoLeft)),
        _ => {}
    }

    if let Some(action) =
        keyboard::find_matching_action(&state.settings.keybindings, key, ctrl, shift, alt)
    {
        match action {
            KeyAction::Undo if state.history.can_undo() => {
                return Task::done(Message::Media(MediaMessage::Undo));
            }
            KeyAction::Redo if state.history.can_redo() => {
                return Task::done(Message::Media(MediaMessage::Redo));
            }
            KeyAction::OpenFolder => {
                return Task::done(Message::Folder(FolderMessage::Pick));
            }
            KeyAction::ToggleMetadataPanel => {
                return Task::done(Message::Settings(SettingsMessage::ToggleMetadataPanel));
            }
            KeyAction::RevealInFileManager => {
                if let Some(entry) = active_entry(state) {
                    return Task::done(Message::Media(MediaMessage::RevealInExplorer(entry.path)));
                }
            }
            KeyAction::Pin => {
                return Task::done(Message::Folder(FolderMessage::PickPin));
            }
            KeyAction::Unpin => {
                if let Some(ref c) = state.folder.current_folder {
                    return Task::done(Message::Folder(FolderMessage::UnpinCurrent(c.clone())));
                }
            }
            KeyAction::GoLeft => {
                return Task::done(Message::Media(MediaMessage::GoLeft));
            }
            KeyAction::GoRight => {
                return Task::done(Message::Media(MediaMessage::GoRight));
            }
            KeyAction::MoveToFolder => {
                if active_entry(state).is_some()
                    && let Some(ref dest) = state.folder.selected_folder
                {
                    return Task::done(Message::Media(MediaMessage::MoveToFolder(dest.clone())));
                }
            }
            KeyAction::CopyToFolder => {
                if active_entry(state).is_some()
                    && let Some(ref dest) = state.folder.selected_folder
                {
                    return Task::done(Message::Media(MediaMessage::CopyToFolder(dest.clone())));
                }
            }
            KeyAction::Delete => {
                if let Some(entry) = active_entry(state) {
                    return Task::done(Message::Media(MediaMessage::DeleteEntry(entry.path)));
                }
            }
            KeyAction::Rename => {
                return Task::done(Message::Media(MediaMessage::TriggerRename));
            }
            KeyAction::CreateFolder => {
                return Task::done(Message::Folder(FolderMessage::TriggerCreate));
            }
            KeyAction::OpenSelectedFolder => {
                if let Some(ref selected_path) = state.folder.selected_folder {
                    return Task::done(Message::Folder(FolderMessage::Open(selected_path.clone())));
                }
            }
            KeyAction::PinSelected => {
                return Task::done(Message::Folder(FolderMessage::PinSelected));
            }
            KeyAction::MovePinnedUp => {
                if let Some(selected_path) = state.folder.selected_folder.clone() {
                    return Task::done(Message::Folder(FolderMessage::MovePinnedUp(selected_path)));
                }
            }
            KeyAction::MovePinnedDown => {
                if let Some(selected_path) = state.folder.selected_folder.clone() {
                    return Task::done(Message::Folder(FolderMessage::MovePinnedDown(
                        selected_path,
                    )));
                }
            }
            KeyAction::FolderUp => {
                state.folder.select_above();
                return super::scroll::scroll_to_selected_folder(state);
            }
            KeyAction::FolderDown => {
                state.folder.select_below();
                return super::scroll::scroll_to_selected_folder(state);
            }
            KeyAction::FolderLeft => {
                state.folder.collapse_selected();
                return super::scroll::scroll_to_selected_folder(state);
            }
            KeyAction::FolderRight => {
                state.folder.expand_selected();
                return super::scroll::scroll_to_selected_folder(state);
            }
            KeyAction::SearchImages => {
                return Task::done(Message::Media(MediaMessage::SearchFocused));
            }
            KeyAction::PreviousPage if active_media_type(state) == Some(MediaType::Document) => {
                return super::document::handle_document_message(state, DocumentMessage::PrevPage);
            }
            KeyAction::NextPage if active_media_type(state) == Some(MediaType::Document) => {
                return super::document::handle_document_message(state, DocumentMessage::NextPage);
            }
            KeyAction::ToggleFilterMode => {
                state.media_grid.filter_mode.enter();
                return Task::none();
            }
            KeyAction::Undo | KeyAction::Redo | KeyAction::PreviousPage | KeyAction::NextPage => {}
        }
    }

    if let Key::Character(c) = key
        && c.is_ascii_digit()
        && c != '0'
        && alt
        && !ctrl
        && !shift
    {
        let digit = c.to_digit(10).expect("c is checked to be an ascii digit") as u8;
        return Task::done(Message::Folder(FolderMessage::PinShortcut(digit)));
    }

    Task::none()
}

fn handle_filter_mode_key(
    state: &mut AppState,
    key: Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> Task<Message> {
    let binding = &state.settings.keybindings.toggle_filter_mode;
    let matches_toggle_filter =
        binding.key == key && binding.ctrl == ctrl && binding.shift == shift && binding.alt == alt;

    if (key == Key::Escape && !ctrl && !shift && !alt) || matches_toggle_filter {
        state.media_grid.filter_mode.exit();
        return Task::none();
    }

    if !ctrl && !shift && !alt {
        match key {
            Key::ArrowLeft => {
                state.media_grid.filter_mode.step(-1);
                return Task::none();
            }
            Key::ArrowRight => {
                state.media_grid.filter_mode.step(1);
                return Task::none();
            }
            Key::Space | Key::Enter => {
                let ty = state.media_grid.filter_mode.focused_media_type();
                return super::media::handle_media_message(
                    state,
                    MediaMessage::ToggleTypeFilter(ty),
                );
            }
            Key::Character(c) if ('1'..='4').contains(&c) => {
                let idx = (c as usize) - ('1' as usize);
                let ty = FilterModeState::FILTER_ORDER[idx];
                state.media_grid.filter_mode.focused_chip = idx;
                return super::media::handle_media_message(
                    state,
                    MediaMessage::ToggleTypeFilter(ty),
                );
            }
            Key::Character('0') => {
                return super::media::handle_media_message(state, MediaMessage::ResetTypeFilters);
            }
            _ => {}
        }
    }

    Task::none()
}
