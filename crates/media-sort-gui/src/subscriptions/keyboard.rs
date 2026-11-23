use iced::Subscription;
use iced::keyboard::{self, Key as IcedKey};

use media_sort_core::settings::keybindings::{Key, KeyAction, KeyBinding, KeyBindings};

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

/// Match a captured keypress + modifier tuple against configured [`KeyBindings`]
/// without allocating.
pub fn find_matching_action(
    kb: &KeyBindings,
    key: Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> Option<KeyAction> {
    KeyAction::iter().find(|action| {
        let binding = action.binding(kb);
        binding.key == key && binding.ctrl == ctrl && binding.shift == shift && binding.alt == alt
    })
}

/// Update the keybinding for `action` in `kb`.
pub fn set_keybinding(
    kb: &mut KeyBindings,
    action: KeyAction,
    key: Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
) {
    *action.binding_mut(kb) = KeyBinding {
        key,
        ctrl,
        shift,
        alt,
        meta: false,
    };
}

pub fn format_keybinding(binding: &KeyBinding) -> String {
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
    fn test_set_keybinding_updates_action() {
        let mut kb = KeyBindings::default();
        set_keybinding(
            &mut kb,
            KeyAction::Undo,
            Key::Character('Z'),
            true,
            false,
            false,
        );
        assert_eq!(kb.undo.key, Key::Character('Z'));
        assert!(kb.undo.ctrl);
        assert!(!kb.undo.shift);
        assert!(!kb.undo.alt);
    }

    #[test]
    fn test_find_matching_action_matches_defaults() {
        let kb = KeyBindings::default();
        assert_eq!(
            find_matching_action(&kb, Key::Character('Q'), false, false, false),
            Some(KeyAction::Undo)
        );
        assert_eq!(
            find_matching_action(&kb, Key::Character('L'), false, false, false),
            Some(KeyAction::RevealInFileManager)
        );
        assert_eq!(
            find_matching_action(&kb, Key::ArrowUp, false, true, false),
            Some(KeyAction::CopyToFolder)
        );
        assert_eq!(
            find_matching_action(&kb, Key::F9, false, false, false),
            None
        );
    }

    #[test]
    fn test_key_action_covers_all_expected_actions() {
        let actions: Vec<KeyAction> = KeyAction::iter().collect();
        assert_eq!(actions.len(), 25);
        let ids: Vec<&str> = actions.iter().map(|a| a.id()).collect();
        assert!(ids.contains(&"undo"));
        assert!(ids.contains(&"redo"));
        assert!(ids.contains(&"delete"));
        assert!(ids.contains(&"rename"));
        assert!(ids.contains(&"move_to_folder"));
        assert!(ids.contains(&"reveal_in_file_manager"));
        assert!(ids.contains(&"previous_page"));
        assert!(ids.contains(&"next_page"));
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
