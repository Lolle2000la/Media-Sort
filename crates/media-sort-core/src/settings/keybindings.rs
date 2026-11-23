use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Enter,
    Tab,
    Space,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Escape,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    MediaPlayPause,
    MediaPlay,
    MediaPause,
    MediaStop,
    MediaTrackNext,
    MediaTrackPrevious,
    AudioVolumeUp,
    AudioVolumeDown,
    AudioVolumeMute,
    Character(char),
}

impl Key {
    pub fn display_name(&self) -> String {
        match self {
            Key::Enter => "Enter".into(),
            Key::Tab => "Tab".into(),
            Key::Space => "Space".into(),
            Key::ArrowUp => "Up".into(),
            Key::ArrowDown => "Down".into(),
            Key::ArrowLeft => "Left".into(),
            Key::ArrowRight => "Right".into(),
            Key::Escape => "Esc".into(),
            Key::Backspace => "Backspace".into(),
            Key::Delete => "Delete".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::PageUp => "PageUp".into(),
            Key::PageDown => "PageDown".into(),
            Key::F1 => "F1".into(),
            Key::F2 => "F2".into(),
            Key::F3 => "F3".into(),
            Key::F4 => "F4".into(),
            Key::F5 => "F5".into(),
            Key::F6 => "F6".into(),
            Key::F7 => "F7".into(),
            Key::F8 => "F8".into(),
            Key::F9 => "F9".into(),
            Key::F10 => "F10".into(),
            Key::F11 => "F11".into(),
            Key::F12 => "F12".into(),
            Key::MediaPlayPause => "MediaPlayPause".into(),
            Key::MediaPlay => "MediaPlay".into(),
            Key::MediaPause => "MediaPause".into(),
            Key::MediaStop => "MediaStop".into(),
            Key::MediaTrackNext => "MediaTrackNext".into(),
            Key::MediaTrackPrevious => "MediaTrackPrevious".into(),
            Key::AudioVolumeUp => "AudioVolumeUp".into(),
            Key::AudioVolumeDown => "AudioVolumeDown".into(),
            Key::AudioVolumeMute => "AudioVolumeMute".into(),
            Key::Character(c) => c.to_string(),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "Enter" => Key::Enter,
            "Tab" => Key::Tab,
            "Space" => Key::Space,
            "Up" => Key::ArrowUp,
            "Down" => Key::ArrowDown,
            "Left" => Key::ArrowLeft,
            "Right" => Key::ArrowRight,
            "Esc" => Key::Escape,
            "Backspace" => Key::Backspace,
            "Delete" => Key::Delete,
            "Home" => Key::Home,
            "End" => Key::End,
            "PageUp" => Key::PageUp,
            "PageDown" => Key::PageDown,
            "F1" => Key::F1,
            "F2" => Key::F2,
            "F3" => Key::F3,
            "F4" => Key::F4,
            "F5" => Key::F5,
            "F6" => Key::F6,
            "F7" => Key::F7,
            "F8" => Key::F8,
            "F9" => Key::F9,
            "F10" => Key::F10,
            "F11" => Key::F11,
            "F12" => Key::F12,
            "MediaPlayPause" => Key::MediaPlayPause,
            "MediaPlay" => Key::MediaPlay,
            "MediaPause" => Key::MediaPause,
            "MediaStop" => Key::MediaStop,
            "MediaTrackNext" => Key::MediaTrackNext,
            "MediaTrackPrevious" => Key::MediaTrackPrevious,
            "AudioVolumeUp" => Key::AudioVolumeUp,
            "AudioVolumeDown" => Key::AudioVolumeDown,
            "AudioVolumeMute" => Key::AudioVolumeMute,
            _ if s.chars().count() == 1 => Key::Character(s.chars().next().unwrap()),
            _ => return None,
        })
    }
}

impl Serialize for Key {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Key::Character(c) => {
                let mut buf = [0u8; 4];
                serializer.serialize_str(c.encode_utf8(&mut buf))
            }
            _ => serializer.serialize_str(&self.display_name()),
        }
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Key::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown key: {s}")))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyBinding {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

impl KeyBinding {
    pub fn new(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: false,
            alt: false,
            meta: false,
        }
    }

    pub fn with_ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }

    pub fn with_shift(mut self) -> Self {
        self.shift = true;
        self
    }

    pub fn with_alt(mut self) -> Self {
        self.alt = true;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyBindings {
    pub move_to_folder: KeyBinding,
    pub copy_to_folder: KeyBinding,
    pub delete: KeyBinding,
    pub rename: KeyBinding,
    pub go_left: KeyBinding,
    pub go_right: KeyBinding,
    pub create_folder: KeyBinding,
    pub folder_up: KeyBinding,
    pub folder_left: KeyBinding,
    pub folder_down: KeyBinding,
    pub folder_right: KeyBinding,
    pub undo: KeyBinding,
    pub redo: KeyBinding,
    pub open_folder: KeyBinding,
    pub open_selected_folder: KeyBinding,
    pub pin: KeyBinding,
    pub pin_selected: KeyBinding,
    pub unpin: KeyBinding,
    pub move_pinned_up: KeyBinding,
    pub move_pinned_down: KeyBinding,
    pub search_images: KeyBinding,
    pub toggle_metadata_panel: KeyBinding,
    #[serde(default = "default_reveal_in_file_manager_binding")]
    pub reveal_in_file_manager: KeyBinding,
    #[serde(default = "default_previous_page_binding")]
    pub previous_page: KeyBinding,
    #[serde(default = "default_next_page_binding")]
    pub next_page: KeyBinding,
}

fn default_reveal_in_file_manager_binding() -> KeyBinding {
    KeyBinding::new(Key::Character('L'))
}

fn default_previous_page_binding() -> KeyBinding {
    KeyBinding::new(Key::PageUp)
}

fn default_next_page_binding() -> KeyBinding {
    KeyBinding::new(Key::PageDown)
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            move_to_folder: KeyBinding::new(Key::ArrowUp),
            copy_to_folder: KeyBinding::new(Key::ArrowUp).with_shift(),
            delete: KeyBinding::new(Key::ArrowDown),
            rename: KeyBinding::new(Key::Character('R')),
            go_left: KeyBinding::new(Key::ArrowLeft),
            go_right: KeyBinding::new(Key::ArrowRight),
            create_folder: KeyBinding::new(Key::Character('C')),
            folder_up: KeyBinding::new(Key::Character('W')),
            folder_left: KeyBinding::new(Key::Character('A')),
            folder_down: KeyBinding::new(Key::Character('S')),
            folder_right: KeyBinding::new(Key::Character('D')),
            undo: KeyBinding::new(Key::Character('Q')),
            redo: KeyBinding::new(Key::Character('E')),
            open_folder: KeyBinding::new(Key::Character('O')),
            open_selected_folder: KeyBinding::new(Key::Enter),
            pin: KeyBinding::new(Key::Character('P')),
            pin_selected: KeyBinding::new(Key::Character('F')),
            unpin: KeyBinding::new(Key::Character('U')),
            move_pinned_up: KeyBinding::new(Key::Character('W')).with_ctrl(),
            move_pinned_down: KeyBinding::new(Key::Character('S')).with_ctrl(),
            search_images: KeyBinding::new(Key::Character('I')),
            toggle_metadata_panel: KeyBinding::new(Key::Character('M')),
            reveal_in_file_manager: default_reveal_in_file_manager_binding(),
            previous_page: default_previous_page_binding(),
            next_page: default_next_page_binding(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::settings::keybindings::{Key, KeyBinding};

    #[test]
    fn test_keybinding_new() {
        let kb = KeyBinding::new(Key::Enter);
        assert_eq!(kb.key, Key::Enter);
        assert!(!kb.ctrl);
        assert!(!kb.shift);
        assert!(!kb.alt);
        assert!(!kb.meta);
    }

    #[test]
    fn test_keybinding_builders() {
        let kb = KeyBinding::new(Key::Character('A'))
            .with_ctrl()
            .with_shift()
            .with_alt();
        assert_eq!(kb.key, Key::Character('A'));
        assert!(kb.ctrl);
        assert!(kb.shift);
        assert!(kb.alt);
        assert!(!kb.meta);
    }

    #[test]
    fn test_keybinding_serde_roundtrip() {
        let kb = KeyBinding::new(Key::Character('X')).with_ctrl();
        let json = serde_json::to_string(&kb).unwrap();
        let kb2: KeyBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(kb2.key, Key::Character('X'));
        assert!(kb2.ctrl);
        assert!(!kb2.shift);
        assert!(!kb2.alt);

        let kb = KeyBinding::new(Key::Character(';')).with_ctrl();
        let json = serde_json::to_string(&kb).unwrap();
        let kb2: KeyBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(kb2.key, Key::Character(';'));
        assert!(kb2.ctrl);
    }

    #[test]
    fn test_key_parse_special_chars() {
        assert_eq!(Key::parse(";"), Some(Key::Character(';')));
        assert_eq!(Key::parse("."), Some(Key::Character('.')));
        assert_eq!(Key::parse(","), Some(Key::Character(',')));
        assert_eq!(Key::parse("+"), Some(Key::Character('+')));
        assert_eq!(Key::parse("-"), Some(Key::Character('-')));
        assert_eq!(Key::parse("="), Some(Key::Character('=')));
        assert_eq!(Key::parse("/"), Some(Key::Character('/')));
        assert_eq!(Key::parse("\\"), Some(Key::Character('\\')));
        assert_eq!(Key::parse("?"), Some(Key::Character('?')));
        assert_eq!(Key::parse("!"), Some(Key::Character('!')));
        assert_eq!(Key::parse("<"), Some(Key::Character('<')));
    }

    #[test]
    fn test_key_display_special_chars() {
        assert_eq!(Key::Character(';').display_name(), ";");
        assert_eq!(Key::Character('.').display_name(), ".");
        assert_eq!(Key::Character('+').display_name(), "+");
        assert_eq!(Key::Character('-').display_name(), "-");
        assert_eq!(Key::Character('ä').display_name(), "ä");
        assert_eq!(Key::Character('ö').display_name(), "ö");
        assert_eq!(Key::Character('ß').display_name(), "ß");
    }

    #[test]
    fn test_key_parse_utf8_chars() {
        assert_eq!(Key::parse("ö"), Some(Key::Character('ö')));
        assert_eq!(Key::parse("€"), Some(Key::Character('€')));

        assert_eq!(Key::Character('ö').display_name(), "ö");
        assert_eq!(Key::Character('€').display_name(), "€");
    }

    #[test]
    fn test_key_parse_multi_char_unknown_returns_none() {
        assert_eq!(Key::parse("Shift+Up"), None);
        assert_eq!(Key::parse("ab"), None);
        assert_eq!(Key::parse(";;"), None);
        assert_eq!(Key::parse(""), None);
    }

    #[test]
    fn test_key_parse_whitespace() {
        assert!(Key::parse("  ").is_none());
        assert!(Key::parse(" Enter ").is_none());
    }

    #[test]
    fn test_key_parse_case_sensitivity() {
        assert!(Key::parse("Esc").is_some());
        assert!(
            Key::parse("esc").is_none(),
            "\"esc\" (lowercase) is not a known key name"
        );
    }

    #[test]
    fn test_key_display_name_non_empty() {
        let keys = vec![
            Key::Enter,
            Key::Escape,
            Key::Space,
            Key::Tab,
            Key::F1,
            Key::F12,
            Key::MediaPlayPause,
            Key::Character('A'),
        ];
        for key in &keys {
            let name = key.display_name();
            assert!(
                !name.is_empty(),
                "display_name should not be empty for {:?}",
                key
            );
        }
    }

    #[test]
    fn test_keybindings_copy_to_folder_default() {
        let kb = &crate::settings::keybindings::KeyBindings::default().copy_to_folder;
        assert_eq!(kb.key, Key::ArrowUp);
        assert!(kb.shift);
        assert!(!kb.ctrl);
    }

    #[test]
    fn test_keybindings_reveal_in_file_manager_default() {
        let kb = &crate::settings::keybindings::KeyBindings::default().reveal_in_file_manager;
        assert_eq!(kb.key, Key::Character('L'));
        assert!(!kb.ctrl);
        assert!(!kb.shift);
        assert!(!kb.alt);
    }

    #[test]
    fn test_keybindings_page_navigation_defaults() {
        let kbs = crate::settings::keybindings::KeyBindings::default();
        assert_eq!(kbs.previous_page.key, Key::PageUp);
        assert!(!kbs.previous_page.ctrl);
        assert!(!kbs.previous_page.shift);
        assert!(!kbs.previous_page.alt);
        assert!(!kbs.previous_page.meta);

        assert_eq!(kbs.next_page.key, Key::PageDown);
        assert!(!kbs.next_page.ctrl);
        assert!(!kbs.next_page.shift);
        assert!(!kbs.next_page.alt);
        assert!(!kbs.next_page.meta);
    }

    #[test]
    fn test_keybindings_omitted_page_navigation_uses_defaults() {
        let mut value =
            serde_json::to_value(crate::settings::keybindings::KeyBindings::default()).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.remove("previous_page");
        obj.remove("next_page");

        let deserialized: crate::settings::keybindings::KeyBindings =
            serde_json::from_value(value).unwrap();
        assert_eq!(deserialized.previous_page.key, Key::PageUp);
        assert!(!deserialized.previous_page.ctrl);
        assert!(!deserialized.previous_page.shift);
        assert!(!deserialized.previous_page.alt);
        assert!(!deserialized.previous_page.meta);

        assert_eq!(deserialized.next_page.key, Key::PageDown);
        assert!(!deserialized.next_page.ctrl);
        assert!(!deserialized.next_page.shift);
        assert!(!deserialized.next_page.alt);
        assert!(!deserialized.next_page.meta);
    }
}
