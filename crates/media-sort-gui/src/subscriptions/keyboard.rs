use iced::Subscription;
use iced::keyboard::{self, Key as IcedKey};

use media_sort_core::settings::keybindings::Key;

use crate::message::Message;

pub fn keyboard_subscription() -> Subscription<Message> {
    iced::event::listen_with(|event, status, _window| match event {
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) => {
            let is_exit_key = matches!(
                key,
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                    | iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter)
                    | iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab)
            );

            if status == iced::event::Status::Captured && !is_exit_key {
                return None;
            }

            key_to_enum(key).map(|key| {
                Message::KeyCaptured(key, modifiers.control(), modifiers.shift(), modifiers.alt())
            })
        }
        _ => None,
    })
}

pub(crate) fn key_to_enum(key: IcedKey) -> Option<Key> {
    match &key {
        IcedKey::Named(named) => {
            let key = match named {
                keyboard::key::Named::Enter => Key::Enter,
                keyboard::key::Named::Tab => Key::Tab,
                keyboard::key::Named::Space => Key::Space,
                keyboard::key::Named::ArrowUp => Key::ArrowUp,
                keyboard::key::Named::ArrowDown => Key::ArrowDown,
                keyboard::key::Named::ArrowLeft => Key::ArrowLeft,
                keyboard::key::Named::ArrowRight => Key::ArrowRight,
                keyboard::key::Named::Escape => Key::Escape,
                keyboard::key::Named::Backspace => Key::Backspace,
                keyboard::key::Named::Delete => Key::Delete,
                keyboard::key::Named::Home => Key::Home,
                keyboard::key::Named::End => Key::End,
                keyboard::key::Named::PageUp => Key::PageUp,
                keyboard::key::Named::PageDown => Key::PageDown,
                keyboard::key::Named::F1 => Key::F1,
                keyboard::key::Named::F2 => Key::F2,
                keyboard::key::Named::F3 => Key::F3,
                keyboard::key::Named::F4 => Key::F4,
                keyboard::key::Named::F5 => Key::F5,
                keyboard::key::Named::F6 => Key::F6,
                keyboard::key::Named::F7 => Key::F7,
                keyboard::key::Named::F8 => Key::F8,
                keyboard::key::Named::F9 => Key::F9,
                keyboard::key::Named::F10 => Key::F10,
                keyboard::key::Named::F11 => Key::F11,
                keyboard::key::Named::F12 => Key::F12,
                keyboard::key::Named::MediaPlayPause => Key::MediaPlayPause,
                keyboard::key::Named::MediaPlay => Key::MediaPlay,
                keyboard::key::Named::MediaPause => Key::MediaPause,
                keyboard::key::Named::MediaStop => Key::MediaStop,
                keyboard::key::Named::MediaTrackNext => Key::MediaTrackNext,
                keyboard::key::Named::MediaTrackPrevious => Key::MediaTrackPrevious,
                keyboard::key::Named::AudioVolumeUp => Key::AudioVolumeUp,
                keyboard::key::Named::AudioVolumeDown => Key::AudioVolumeDown,
                keyboard::key::Named::AudioVolumeMute => Key::AudioVolumeMute,
                _ => return None,
            };
            Some(key)
        }
        IcedKey::Character(c) if !c.is_empty() => {
            let upper = c.to_uppercase();
            let ch = upper
                .chars()
                .next()
                .expect("character string is not empty because c was checked to be non-empty");
            Some(Key::Character(ch))
        }
        _ => None,
    }
}

pub fn keybinding_list(
    state: &crate::state::AppState,
) -> Vec<(String, media_sort_core::settings::keybindings::KeyBinding)> {
    let kb = &state.settings.keybindings;
    vec![
        ("move_to_folder".into(), kb.move_to_folder.clone()),
        ("copy_to_folder".into(), kb.copy_to_folder.clone()),
        ("delete".into(), kb.delete.clone()),
        ("rename".into(), kb.rename.clone()),
        ("go_left".into(), kb.go_left.clone()),
        ("go_right".into(), kb.go_right.clone()),
        ("create_folder".into(), kb.create_folder.clone()),
        ("folder_up".into(), kb.folder_up.clone()),
        ("folder_left".into(), kb.folder_left.clone()),
        ("folder_down".into(), kb.folder_down.clone()),
        ("folder_right".into(), kb.folder_right.clone()),
        ("undo".into(), kb.undo.clone()),
        ("redo".into(), kb.redo.clone()),
        ("open_folder".into(), kb.open_folder.clone()),
        (
            "open_selected_folder".into(),
            kb.open_selected_folder.clone(),
        ),
        ("pin".into(), kb.pin.clone()),
        ("pin_selected".into(), kb.pin_selected.clone()),
        ("unpin".into(), kb.unpin.clone()),
        ("move_pinned_up".into(), kb.move_pinned_up.clone()),
        ("move_pinned_down".into(), kb.move_pinned_down.clone()),
        ("search_images".into(), kb.search_images.clone()),
        (
            "toggle_metadata_panel".into(),
            kb.toggle_metadata_panel.clone(),
        ),
        (
            "reveal_in_file_manager".into(),
            kb.reveal_in_file_manager.clone(),
        ),
        ("previous_page".into(), kb.previous_page.clone()),
        ("next_page".into(), kb.next_page.clone()),
    ]
}

pub fn update_keybinding(
    kb: &mut media_sort_core::settings::keybindings::KeyBindings,
    name: &str,
    key: Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
) {
    let binding = media_sort_core::settings::keybindings::KeyBinding {
        key,
        ctrl,
        shift,
        alt,
        meta: false,
    };
    match name {
        "move_to_folder" => kb.move_to_folder = binding,
        "copy_to_folder" => kb.copy_to_folder = binding,
        "delete" => kb.delete = binding,
        "rename" => kb.rename = binding,
        "go_left" => kb.go_left = binding,
        "go_right" => kb.go_right = binding,
        "create_folder" => kb.create_folder = binding,
        "folder_up" => kb.folder_up = binding,
        "folder_left" => kb.folder_left = binding,
        "folder_down" => kb.folder_down = binding,
        "folder_right" => kb.folder_right = binding,
        "undo" => kb.undo = binding,
        "redo" => kb.redo = binding,
        "open_folder" => kb.open_folder = binding,
        "open_selected_folder" => kb.open_selected_folder = binding,
        "pin" => kb.pin = binding,
        "pin_selected" => kb.pin_selected = binding,
        "unpin" => kb.unpin = binding,
        "move_pinned_up" => kb.move_pinned_up = binding,
        "move_pinned_down" => kb.move_pinned_down = binding,
        "search_images" => kb.search_images = binding,
        "toggle_metadata_panel" => kb.toggle_metadata_panel = binding,
        "reveal_in_file_manager" => kb.reveal_in_file_manager = binding,
        "previous_page" => kb.previous_page = binding,
        "next_page" => kb.next_page = binding,
        _ => {}
    }
}

pub fn format_keybinding(binding: &media_sort_core::settings::keybindings::KeyBinding) -> String {
    let mut parts: Vec<String> = Vec::new();
    if binding.ctrl {
        parts.push("Ctrl".into());
    }
    if binding.shift {
        parts.push("Shift".into());
    }
    if binding.alt {
        parts.push("Alt".into());
    }
    parts.push(binding.key.display_name());
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_sort_core::settings::keybindings::{Key, KeyBinding, KeyBindings};

    #[test]
    fn test_key_to_name_named_keys() {
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Named(
                iced::keyboard::key::Named::Enter
            )),
            Some(Key::Enter)
        );
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Named(
                iced::keyboard::key::Named::Space
            )),
            Some(Key::Space)
        );
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Named(
                iced::keyboard::key::Named::ArrowUp
            )),
            Some(Key::ArrowUp)
        );
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Named(
                iced::keyboard::key::Named::Escape
            )),
            Some(Key::Escape)
        );
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Named(iced::keyboard::key::Named::F1)),
            Some(Key::F1)
        );
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Named(iced::keyboard::key::Named::F12)),
            Some(Key::F12)
        );
    }

    #[test]
    fn test_key_to_name_character() {
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Character("a".into())),
            Some(Key::Character('A'))
        );
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Character("z".into())),
            Some(Key::Character('Z'))
        );
    }

    #[test]
    fn test_key_to_name_unknown() {
        assert_eq!(key_to_enum(iced::keyboard::Key::Unidentified), None);
        assert_eq!(key_to_enum(iced::keyboard::Key::Character("".into())), None);
    }

    #[test]
    fn test_format_keybinding_plain() {
        let kb = KeyBinding::new(Key::Character('A'));
        assert_eq!(format_keybinding(&kb), "A");
    }

    #[test]
    fn test_format_keybinding_ctrl() {
        let kb = KeyBinding::new(Key::Character('X')).with_ctrl();
        assert_eq!(format_keybinding(&kb), "Ctrl+X");
    }

    #[test]
    fn test_format_keybinding_shift() {
        let mut kb = KeyBinding::new(Key::Character('A'));
        kb.shift = true;
        assert_eq!(format_keybinding(&kb), "Shift+A");
    }

    #[test]
    fn test_format_keybinding_ctrl_shift() {
        let kb = KeyBinding::new(Key::Character('Z'))
            .with_ctrl()
            .with_shift();
        assert_eq!(format_keybinding(&kb), "Ctrl+Shift+Z");
    }

    #[test]
    fn test_format_keybinding_all_modifiers() {
        let kb = KeyBinding::new(Key::Delete)
            .with_ctrl()
            .with_shift()
            .with_alt();
        assert_eq!(format_keybinding(&kb), "Ctrl+Shift+Alt+Delete");
    }

    #[test]
    fn test_update_keybinding_known_name() {
        let mut kb = KeyBindings::default();
        update_keybinding(&mut kb, "undo", Key::Character('Z'), true, false, false);
        assert_eq!(kb.undo.key, Key::Character('Z'));
        assert!(kb.undo.ctrl);
        assert!(!kb.undo.shift);
        assert!(!kb.undo.alt);
    }

    #[test]
    fn test_update_keybinding_unknown_name() {
        let mut kb = KeyBindings::default();
        let saved = kb.redo.key;
        update_keybinding(
            &mut kb,
            "nonexistent_action",
            Key::Character('X'),
            false,
            false,
            false,
        );
        assert_eq!(kb.redo.key, saved);
    }

    #[test]
    fn test_keybinding_list_length() {
        use crate::state::AppState;
        use media_sort_core::settings::store::SettingsStore;

        let state = AppState::new(SettingsStore::default());
        let list = keybinding_list(&state);
        assert_eq!(list.len(), 25);
        let names: Vec<&str> = list.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&"undo"));
        assert!(names.contains(&"redo"));
        assert!(names.contains(&"delete"));
    }

    #[test]
    fn test_keybinding_list_has_known_actions() {
        use crate::state::AppState;
        use media_sort_core::settings::store::SettingsStore;

        let state = AppState::new(SettingsStore::default());
        let list = keybinding_list(&state);
        let names: Vec<&str> = list.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&"undo"));
        assert!(names.contains(&"redo"));
        assert!(names.contains(&"delete"));
        assert!(names.contains(&"rename"));
        assert!(names.contains(&"move_to_folder"));
        assert!(names.contains(&"reveal_in_file_manager"));
        assert!(names.contains(&"previous_page"));
        assert!(names.contains(&"next_page"));
    }

    #[test]
    fn test_format_keybinding_special_char() {
        let kb = KeyBinding::new(Key::Character(';'));
        assert_eq!(format_keybinding(&kb), ";");

        let kb = KeyBinding::new(Key::Character(';')).with_ctrl();
        assert_eq!(format_keybinding(&kb), "Ctrl+;");

        let kb = KeyBinding::new(Key::Character('ö')).with_shift();
        assert_eq!(format_keybinding(&kb), "Shift+ö");
    }

    #[test]
    fn test_key_to_enum_special_char() {
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Character(";".into())),
            Some(Key::Character(';'))
        );
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Character("+".into())),
            Some(Key::Character('+'))
        );
        assert_eq!(
            key_to_enum(iced::keyboard::Key::Character("ö".into())),
            Some(Key::Character('Ö'))
        );
    }
}
