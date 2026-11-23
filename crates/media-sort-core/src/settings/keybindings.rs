use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::{EnumIter, IntoEnumIterator};

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

/// Top-level sections used to group [`KeyAction`] rows in the settings UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter)]
pub enum KeyActionSection {
    Images,
    Folders,
    Documents,
    Other,
}

impl KeyActionSection {
    /// Iterates over all [`KeyActionSection`] variants in display order.
    pub fn iter() -> KeyActionSectionIter {
        <Self as IntoEnumIterator>::iter()
    }

    /// Returns the Fluent localization key for this section heading.
    pub const fn l10n_key(self) -> &'static str {
        match self {
            Self::Images => "keybindings-images",
            Self::Folders => "keybindings-folders",
            Self::Documents => "keybindings-documents-section",
            Self::Other => "keybindings-other",
        }
    }
}

/// Subsections within a [`KeyActionSection`] in the settings UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter)]
pub enum KeyActionSubsection {
    Management,
    Selection,
    Search,
    Metadata,
    Open,
    Pinned,
    History,
}

impl KeyActionSubsection {
    /// Iterates over all [`KeyActionSubsection`] variants in declaration order.
    pub fn iter() -> KeyActionSubsectionIter {
        <Self as IntoEnumIterator>::iter()
    }

    /// Returns the Fluent localization key for this subsection heading.
    pub const fn l10n_key(self) -> &'static str {
        match self {
            Self::Management => "keybindings-management",
            Self::Selection => "keybindings-selection",
            Self::Search => "keybindings-search",
            Self::Metadata => "keybindings-metadata",
            Self::Open => "keybindings-open",
            Self::Pinned => "keybindings-pinned",
            Self::History => "keybindings-history",
        }
    }
}

/// All configurable keybinding actions in canonical UI/list order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter)]
pub enum KeyAction {
    MoveToFolder,
    CopyToFolder,
    Delete,
    Rename,
    GoLeft,
    GoRight,
    CreateFolder,
    FolderUp,
    FolderLeft,
    FolderDown,
    FolderRight,
    Undo,
    Redo,
    OpenFolder,
    OpenSelectedFolder,
    Pin,
    PinSelected,
    Unpin,
    MovePinnedUp,
    MovePinnedDown,
    SearchImages,
    ToggleMetadataPanel,
    RevealInFileManager,
    PreviousPage,
    NextPage,
}

impl KeyAction {
    /// Iterates over all [`KeyAction`] variants in canonical order.
    pub fn iter() -> KeyActionIter {
        <Self as IntoEnumIterator>::iter()
    }

    /// Returns the stable snake_case identifier matching the [`KeyBindings`] field name.
    pub const fn id(self) -> &'static str {
        match self {
            Self::MoveToFolder => "move_to_folder",
            Self::CopyToFolder => "copy_to_folder",
            Self::Delete => "delete",
            Self::Rename => "rename",
            Self::GoLeft => "go_left",
            Self::GoRight => "go_right",
            Self::CreateFolder => "create_folder",
            Self::FolderUp => "folder_up",
            Self::FolderLeft => "folder_left",
            Self::FolderDown => "folder_down",
            Self::FolderRight => "folder_right",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::OpenFolder => "open_folder",
            Self::OpenSelectedFolder => "open_selected_folder",
            Self::Pin => "pin",
            Self::PinSelected => "pin_selected",
            Self::Unpin => "unpin",
            Self::MovePinnedUp => "move_pinned_up",
            Self::MovePinnedDown => "move_pinned_down",
            Self::SearchImages => "search_images",
            Self::ToggleMetadataPanel => "toggle_metadata_panel",
            Self::RevealInFileManager => "reveal_in_file_manager",
            Self::PreviousPage => "previous_page",
            Self::NextPage => "next_page",
        }
    }

    /// Returns the Fluent localization key for this action's settings label.
    pub const fn l10n_key(self) -> &'static str {
        match self {
            Self::MoveToFolder => "keybindings-move",
            Self::CopyToFolder => "keybindings-copy",
            Self::Delete => "keybindings-delete",
            Self::Rename => "keybindings-rename",
            Self::GoLeft => "keybindings-select-left",
            Self::GoRight => "keybindings-select-right",
            Self::CreateFolder => "keybindings-create-folder",
            Self::FolderUp => "keybindings-select-above",
            Self::FolderLeft => "keybindings-collapse",
            Self::FolderDown => "keybindings-select-below",
            Self::FolderRight => "keybindings-expand",
            Self::Undo => "keybindings-undo",
            Self::Redo => "keybindings-redo",
            Self::OpenFolder => "keybindings-open-folder",
            Self::OpenSelectedFolder => "keybindings-open-selected",
            Self::Pin => "keybindings-pin",
            Self::PinSelected => "keybindings-pin-selected",
            Self::Unpin => "keybindings-unpin",
            Self::MovePinnedUp => "keybindings-move-pinned-up",
            Self::MovePinnedDown => "keybindings-move-pinned-down",
            Self::SearchImages => "keybindings-search-images",
            Self::ToggleMetadataPanel => "keybindings-toggle-metadata",
            Self::RevealInFileManager => "keybindings-reveal-in-file-manager",
            Self::PreviousPage => "keybindings-previous-page",
            Self::NextPage => "keybindings-next-page",
        }
    }

    /// Returns the top-level settings section containing this action.
    pub const fn section(self) -> KeyActionSection {
        match self {
            Self::MoveToFolder
            | Self::CopyToFolder
            | Self::Delete
            | Self::Rename
            | Self::RevealInFileManager
            | Self::GoLeft
            | Self::GoRight
            | Self::SearchImages
            | Self::ToggleMetadataPanel => KeyActionSection::Images,
            Self::CreateFolder
            | Self::FolderUp
            | Self::FolderLeft
            | Self::FolderDown
            | Self::FolderRight
            | Self::OpenFolder
            | Self::OpenSelectedFolder
            | Self::Pin
            | Self::PinSelected
            | Self::Unpin
            | Self::MovePinnedUp
            | Self::MovePinnedDown => KeyActionSection::Folders,
            Self::PreviousPage | Self::NextPage => KeyActionSection::Documents,
            Self::Undo | Self::Redo => KeyActionSection::Other,
        }
    }

    /// Returns the optional subsection within [`KeyAction::section`] containing this action.
    pub const fn subsection(self) -> Option<KeyActionSubsection> {
        match self {
            Self::MoveToFolder
            | Self::CopyToFolder
            | Self::Delete
            | Self::Rename
            | Self::RevealInFileManager
            | Self::CreateFolder => Some(KeyActionSubsection::Management),
            Self::GoLeft
            | Self::GoRight
            | Self::FolderUp
            | Self::FolderLeft
            | Self::FolderDown
            | Self::FolderRight => Some(KeyActionSubsection::Selection),
            Self::SearchImages => Some(KeyActionSubsection::Search),
            Self::ToggleMetadataPanel => Some(KeyActionSubsection::Metadata),
            Self::OpenFolder | Self::OpenSelectedFolder => Some(KeyActionSubsection::Open),
            Self::Pin
            | Self::PinSelected
            | Self::Unpin
            | Self::MovePinnedUp
            | Self::MovePinnedDown => Some(KeyActionSubsection::Pinned),
            Self::PreviousPage | Self::NextPage => None,
            Self::Undo | Self::Redo => Some(KeyActionSubsection::History),
        }
    }

    /// Returns an immutable reference to this action's binding in `kb`.
    pub const fn binding(self, kb: &KeyBindings) -> &KeyBinding {
        match self {
            Self::MoveToFolder => &kb.move_to_folder,
            Self::CopyToFolder => &kb.copy_to_folder,
            Self::Delete => &kb.delete,
            Self::Rename => &kb.rename,
            Self::GoLeft => &kb.go_left,
            Self::GoRight => &kb.go_right,
            Self::CreateFolder => &kb.create_folder,
            Self::FolderUp => &kb.folder_up,
            Self::FolderLeft => &kb.folder_left,
            Self::FolderDown => &kb.folder_down,
            Self::FolderRight => &kb.folder_right,
            Self::Undo => &kb.undo,
            Self::Redo => &kb.redo,
            Self::OpenFolder => &kb.open_folder,
            Self::OpenSelectedFolder => &kb.open_selected_folder,
            Self::Pin => &kb.pin,
            Self::PinSelected => &kb.pin_selected,
            Self::Unpin => &kb.unpin,
            Self::MovePinnedUp => &kb.move_pinned_up,
            Self::MovePinnedDown => &kb.move_pinned_down,
            Self::SearchImages => &kb.search_images,
            Self::ToggleMetadataPanel => &kb.toggle_metadata_panel,
            Self::RevealInFileManager => &kb.reveal_in_file_manager,
            Self::PreviousPage => &kb.previous_page,
            Self::NextPage => &kb.next_page,
        }
    }

    /// Returns a mutable reference to this action's binding in `kb`.
    pub const fn binding_mut(self, kb: &mut KeyBindings) -> &mut KeyBinding {
        match self {
            Self::MoveToFolder => &mut kb.move_to_folder,
            Self::CopyToFolder => &mut kb.copy_to_folder,
            Self::Delete => &mut kb.delete,
            Self::Rename => &mut kb.rename,
            Self::GoLeft => &mut kb.go_left,
            Self::GoRight => &mut kb.go_right,
            Self::CreateFolder => &mut kb.create_folder,
            Self::FolderUp => &mut kb.folder_up,
            Self::FolderLeft => &mut kb.folder_left,
            Self::FolderDown => &mut kb.folder_down,
            Self::FolderRight => &mut kb.folder_right,
            Self::Undo => &mut kb.undo,
            Self::Redo => &mut kb.redo,
            Self::OpenFolder => &mut kb.open_folder,
            Self::OpenSelectedFolder => &mut kb.open_selected_folder,
            Self::Pin => &mut kb.pin,
            Self::PinSelected => &mut kb.pin_selected,
            Self::Unpin => &mut kb.unpin,
            Self::MovePinnedUp => &mut kb.move_pinned_up,
            Self::MovePinnedDown => &mut kb.move_pinned_down,
            Self::SearchImages => &mut kb.search_images,
            Self::ToggleMetadataPanel => &mut kb.toggle_metadata_panel,
            Self::RevealInFileManager => &mut kb.reveal_in_file_manager,
            Self::PreviousPage => &mut kb.previous_page,
            Self::NextPage => &mut kb.next_page,
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
    fn test_key_action_count_and_order() {
        use crate::settings::keybindings::KeyAction;

        let actions: Vec<KeyAction> = KeyAction::iter().collect();
        assert_eq!(actions.len(), 25);
        assert_eq!(actions[0], KeyAction::MoveToFolder);
        assert_eq!(actions[3], KeyAction::Rename);
        assert_eq!(actions[22], KeyAction::RevealInFileManager);
        assert_eq!(actions[23], KeyAction::PreviousPage);
        assert_eq!(actions[24], KeyAction::NextPage);
    }

    #[test]
    fn test_key_action_ids_unique_and_match_toml_fields() {
        use std::collections::HashSet;

        use crate::settings::keybindings::{KeyAction, KeyBindings};

        let toml_val = toml::Value::try_from(KeyBindings::default()).unwrap();
        let table = toml_val.as_table().unwrap();
        let mut seen = HashSet::new();

        for action in KeyAction::iter() {
            let id = action.id();
            assert!(!id.is_empty());
            assert!(seen.insert(id), "duplicate KeyAction id: {id}");
            assert!(
                table.contains_key(id),
                "KeyAction id {id} missing from KeyBindings TOML table"
            );
        }
        assert_eq!(seen.len(), table.len());
    }

    #[test]
    fn test_key_action_binding_roundtrip() {
        use crate::settings::keybindings::{KeyAction, KeyBindings};

        for (idx, action) in KeyAction::iter().enumerate() {
            let mut kb = KeyBindings::default();
            let marker = Key::F1;
            {
                let target = action.binding_mut(&mut kb);
                target.key = marker;
                target.ctrl = idx % 2 == 0;
                target.shift = idx % 3 == 0;
                target.alt = idx % 5 == 0;
                target.meta = idx % 7 == 0;
            }
            let read_back = action.binding(&kb);
            assert_eq!(read_back.key, marker);
            assert_eq!(read_back.ctrl, idx % 2 == 0);
            assert_eq!(read_back.shift, idx % 3 == 0);
            assert_eq!(read_back.alt, idx % 5 == 0);
            assert_eq!(read_back.meta, idx % 7 == 0);
        }
    }

    #[test]
    fn test_key_action_l10n_keys_non_empty() {
        use crate::settings::keybindings::{KeyAction, KeyActionSection, KeyActionSubsection};

        for section in KeyActionSection::iter() {
            assert!(!section.l10n_key().is_empty());
        }
        for subsection in KeyActionSubsection::iter() {
            assert!(!subsection.l10n_key().is_empty());
        }
        for action in KeyAction::iter() {
            assert!(!action.l10n_key().is_empty());
            assert!(!action.section().l10n_key().is_empty());
            if let Some(subsection) = action.subsection() {
                assert!(!subsection.l10n_key().is_empty());
            }
        }
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
