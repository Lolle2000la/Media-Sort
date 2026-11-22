use iced::Task;

use media_sort_core::settings::keybindings::Key;

use crate::message::{FolderMessage, MediaMessage, Message, SettingsMessage};
use crate::state::{AppState, SettingsUiState};
use crate::subscriptions::keyboard;

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

    if let Some(idx) = waiting {
        state.settings_ui = SettingsUiState::Keybindings {
            editing_keybinding: None,
            waiting_for_key: false,
        };
        let bindings = keyboard::keybinding_list(state);
        if idx < bindings.len() {
            let (name, _) = &bindings[idx];
            keyboard::update_keybinding(
                &mut state.settings.keybindings,
                name,
                key,
                ctrl,
                shift,
                alt,
            );
            state.settings.mark_dirty();
        }
        return Task::none();
    }

    if state.rename.path.is_some() {
        match key {
            Key::Enter => {
                return Task::done(Message::Media(MediaMessage::SubmitRename));
            }
            Key::Escape => {
                return Task::done(Message::Media(MediaMessage::CancelRename));
            }
            _ => return Task::none(),
        }
    }

    if state.create_folder.creating_folder_parent.is_some() {
        match key {
            Key::Enter => {
                return Task::done(Message::Folder(FolderMessage::SubmitCreate(
                    std::path::PathBuf::new(),
                )));
            }
            Key::Escape => {
                return Task::done(Message::Folder(FolderMessage::CancelCreate));
            }
            _ => return Task::none(),
        }
    }

    if state.media_grid.search.focused {
        match key {
            Key::Enter | Key::Escape | Key::Tab => {
                return Task::done(Message::Media(MediaMessage::SearchBlurred));
            }
            _ => return Task::none(),
        }
    }

    if key == Key::Space
        && !state.media_grid.search.focused
        && state.rename.path.is_none()
        && state.create_folder.creating_folder_parent.is_none()
    {
        state.video.toggle_pause();
        return Task::none();
    }

    if ctrl && key == Key::Character('Q') {
        return Task::done(Message::Quit);
    }

    match key {
        Key::MediaPlayPause | Key::MediaPlay | Key::MediaPause => {
            state.video.toggle_pause();
            return Task::none();
        }
        Key::MediaStop => {
            state.video.stop();
            return Task::none();
        }
        Key::AudioVolumeUp => {
            state.video.set_volume(state.video.volume() + 5.0);
            return Task::none();
        }
        Key::AudioVolumeDown => {
            state.video.set_volume(state.video.volume() - 5.0);
            return Task::none();
        }
        Key::AudioVolumeMute => {
            state.video.toggle_mute();
            return Task::none();
        }
        _ => {}
    }

    match key {
        Key::MediaTrackNext => {
            return Task::done(Message::Media(MediaMessage::GoRight));
        }
        Key::MediaTrackPrevious => {
            return Task::done(Message::Media(MediaMessage::GoLeft));
        }
        _ => {}
    }

    let bindings = keyboard::keybinding_list(state);
    for (name, binding) in &bindings {
        if binding.key == key
            && binding.ctrl == ctrl
            && binding.shift == shift
            && binding.alt == alt
        {
            match name.as_str() {
                "undo" if state.history.can_undo() => {
                    return Task::done(Message::Media(MediaMessage::Undo));
                }
                "redo" if state.history.can_redo() => {
                    return Task::done(Message::Media(MediaMessage::Redo));
                }
                "open_folder" => {
                    return Task::done(Message::Folder(FolderMessage::Pick));
                }
                "toggle_metadata_panel" => {
                    return Task::done(Message::Settings(SettingsMessage::ToggleMetadataPanel));
                }
                "reveal_in_file_manager" => {
                    if let Some(index) = state.media_grid.selected_index {
                        let filtered = state.media_grid.filtered_entries();
                        if let Some(entry) = filtered.get(index) {
                            return Task::done(Message::Media(MediaMessage::RevealInExplorer(
                                entry.path.clone(),
                            )));
                        }
                    }
                }
                "pin" => {
                    return Task::done(Message::Folder(FolderMessage::PickPin));
                }
                "unpin" => {
                    if let Some(ref c) = state.folder.current_folder {
                        return Task::done(Message::Folder(FolderMessage::UnpinCurrent(c.clone())));
                    }
                }
                "go_left" => {
                    return Task::done(Message::Media(MediaMessage::GoLeft));
                }
                "go_right" => {
                    return Task::done(Message::Media(MediaMessage::GoRight));
                }
                "move_to_folder" => {
                    if let Some(index) = state.media_grid.selected_index {
                        let filtered = state.media_grid.filtered_entries();
                        if let Some(_entry) = filtered.get(index)
                            && let Some(ref dest) = state.folder.selected_folder
                        {
                            return Task::done(Message::Media(MediaMessage::MoveToFolder(
                                dest.clone(),
                            )));
                        }
                    }
                }
                "copy_to_folder" => {
                    if let Some(index) = state.media_grid.selected_index {
                        let filtered = state.media_grid.filtered_entries();
                        if let Some(_entry) = filtered.get(index)
                            && let Some(ref dest) = state.folder.selected_folder
                        {
                            return Task::done(Message::Media(MediaMessage::CopyToFolder(
                                dest.clone(),
                            )));
                        }
                    }
                }
                "delete" => {
                    if let Some(index) = state.media_grid.selected_index {
                        let filtered = state.media_grid.filtered_entries();
                        if let Some(entry) = filtered.get(index) {
                            return Task::done(Message::Media(MediaMessage::DeleteEntry(
                                entry.path.clone(),
                            )));
                        }
                    }
                }
                "rename" => {
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
                        }
                    }
                }
                "create_folder" => {
                    if let Some(p) = state
                        .folder
                        .selected_folder
                        .as_ref()
                        .or(state.folder.current_folder.as_ref())
                    {
                        state.create_folder.creating_folder_parent = Some(p.clone());
                        state.create_folder.create_folder_input = String::new();
                    }
                }
                "open_selected_folder" => {
                    if let Some(ref selected_path) = state.folder.selected_folder {
                        return Task::done(Message::Folder(FolderMessage::Open(
                            selected_path.clone(),
                        )));
                    }
                }
                "pin_selected" => {
                    return Task::done(Message::Folder(FolderMessage::PinSelected));
                }
                "move_pinned_up" => {
                    if let Some(selected_path) = state.folder.selected_folder.clone() {
                        return Task::done(Message::Folder(FolderMessage::MovePinnedUp(
                            selected_path,
                        )));
                    }
                }
                "move_pinned_down" => {
                    if let Some(selected_path) = state.folder.selected_folder.clone() {
                        return Task::done(Message::Folder(FolderMessage::MovePinnedDown(
                            selected_path,
                        )));
                    }
                }
                "folder_up" => {
                    state.folder.select_above();
                    return super::tasks::scroll_to_selected_folder(state);
                }
                "folder_down" => {
                    state.folder.select_below();
                    return super::tasks::scroll_to_selected_folder(state);
                }
                "folder_left" => {
                    state.folder.collapse_selected();
                    return super::tasks::scroll_to_selected_folder(state);
                }
                "folder_right" => {
                    state.folder.expand_selected();
                    return super::tasks::scroll_to_selected_folder(state);
                }
                "search_images" => {
                    return Task::done(Message::Media(MediaMessage::SearchFocused));
                }
                _ => {}
            }
        }
    }

    if let Some(doc_msg) = document_nav_message(state, key, ctrl, shift, alt) {
        return Task::done(Message::Media(doc_msg));
    }

    match key {
        Key::Character(c) if c.is_ascii_digit() && c != '0' && alt && !ctrl && !shift => {
            let digit = c.to_digit(10).expect("c is checked to be an ascii digit") as u8;
            return Task::done(Message::Folder(FolderMessage::PinShortcut(digit)));
        }
        _ => {}
    }

    Task::none()
}

pub(crate) fn document_nav_message(
    state: &AppState,
    key: Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> Option<MediaMessage> {
    if ctrl || shift || alt || !can_navigate_document(state) {
        return None;
    }
    match key {
        Key::PageUp => Some(MediaMessage::DocumentPrevPage),
        Key::PageDown => Some(MediaMessage::DocumentNextPage),
        _ => None,
    }
}

fn can_navigate_document(state: &AppState) -> bool {
    #[cfg(feature = "velopack")]
    if state.show_update_prompt {
        return false;
    }
    if state.rename.path.is_some()
        || state.create_folder.creating_folder_parent.is_some()
        || state.media_grid.search.focused
        || !matches!(state.settings_ui, SettingsUiState::Hidden)
        || state.show_credits
    {
        return false;
    }
    let Some(idx) = state.media_grid.selected_index else {
        return false;
    };
    state
        .media_grid
        .filtered_entries()
        .get(idx)
        .is_some_and(|e| e.media_type == media_sort_core::media_type::MediaType::Document)
}
