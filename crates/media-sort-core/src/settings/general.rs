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

    #[serde(default = "default_video_zero_copy")]
    pub video_zero_copy: bool,

    #[serde(default = "default_true")]
    pub video_hardware_decoding: bool,
}

/// Zero-copy GPU interop is only implemented on Windows and Linux; on macOS
/// the setting is always forced to `false` (see also `app::video_subscription`
/// and `update::settings::handle_settings_loaded`).
fn default_video_zero_copy() -> bool {
    !cfg!(target_os = "macos")
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
            video_zero_copy: default_video_zero_copy(),
            video_hardware_decoding: true,
        }
    }
}

fn default_theme() -> String {
    "Light".to_string()
}

fn default_true() -> bool {
    true
}

fn default_folder_tree_width() -> u16 {
    240
}
