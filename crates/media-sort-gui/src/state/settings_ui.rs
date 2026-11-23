use media_sort_core::settings::keybindings::KeyAction;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SettingsUiState {
    #[default]
    Hidden,
    Settings,
    Advanced,
    Keybindings {
        editing_keybinding: Option<KeyAction>,
        waiting_for_key: bool,
    },
}
