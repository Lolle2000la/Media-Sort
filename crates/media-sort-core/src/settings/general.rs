use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralSettings {
    #[serde(default = "default_theme")]
    pub theme: String,

    #[serde(default = "default_true")]
    pub check_for_updates_on_startup: bool,

    #[serde(default)]
    pub install_prerelease_builds: bool,

    #[serde(default = "default_true")]
    pub animate_gifs: bool,

    #[serde(default)]
    pub integration_with_windows: bool,

    #[serde(default = "default_true")]
    pub reopen_last_opened_folder: bool,

    #[serde(default)]
    pub last_opened_folder: Option<String>,

    #[serde(default = "default_true")]
    pub reopen_last_selected_media: bool,

    #[serde(default)]
    pub last_selected_media: Option<String>,

    #[serde(default)]
    pub locale: Option<String>,

    #[serde(default = "default_folder_tree_width")]
    pub folder_tree_width: u16,

    /// Treat pinned folders as session-scoped: this instance neither
    /// adopts pinned folders from other instances (on config reload) nor
    /// lets them adopt its own. Off by default (pinned folders are
    /// shared between running instances).
    #[serde(default)]
    pub session_pinned_folders: bool,

    /// Whether the active-session Documents filter chip starts enabled when
    /// opening a folder.
    #[serde(default = "default_true")]
    pub show_documents_by_default: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            check_for_updates_on_startup: true,
            install_prerelease_builds: false,
            animate_gifs: true,
            integration_with_windows: false,
            reopen_last_opened_folder: true,
            last_opened_folder: None,
            reopen_last_selected_media: true,
            last_selected_media: None,
            locale: None,
            folder_tree_width: default_folder_tree_width(),
            session_pinned_folders: false,
            show_documents_by_default: true,
        }
    }
}

/// `"Auto"` follows the OS light/dark preference; see
/// `AppState::system_theme` in the GUI crate.
fn default_theme() -> String {
    "Auto".to_string()
}

fn default_true() -> bool {
    true
}

fn default_folder_tree_width() -> u16 {
    240
}
