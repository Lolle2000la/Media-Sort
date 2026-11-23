use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvancedSettings {
    /// Disables hardware video decoding. Hardware decoding is enabled by
    /// default; when enabled, video frames are rendered at their native
    /// (mpv default) resolution.
    #[serde(default)]
    pub disable_hardware_decoding: bool,

    /// When true (the default) and a host `soffice`/`libreoffice` binary is
    /// detected outside Flatpak/Snap sandboxes, render Word/PowerPoint/ODF
    /// previews via headless PDF conversion instead of pure-Rust XML text
    /// extraction.
    #[serde(default = "default_true")]
    pub enable_libreoffice_preview: bool,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            disable_hardware_decoding: false,
            enable_libreoffice_preview: true,
        }
    }
}
