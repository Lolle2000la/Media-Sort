use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::path_utils;
use crate::settings::advanced::AdvancedSettings;
use crate::settings::general::GeneralSettings;
use crate::settings::keybindings::KeyBindings;
use crate::settings::metadata_panel::MetadataPanelSettings;
use crate::settings::pinned_folders::PinnedFoldersSettings;
use crate::settings::window_position::WindowPosition;

#[derive(Debug)]
pub enum SettingsError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    TomlDe(toml::de::Error),
    TomlSer(toml::ser::Error),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Io(e) => write!(f, "IO error: {e}"),
            SettingsError::Serde(e) => write!(f, "Serialization error: {e}"),
            SettingsError::TomlDe(e) => write!(f, "TOML deserialization error: {e}"),
            SettingsError::TomlSer(e) => write!(f, "TOML serialization error: {e}"),
        }
    }
}

impl From<std::io::Error> for SettingsError {
    fn from(e: std::io::Error) -> Self {
        SettingsError::Io(e)
    }
}

impl From<serde_json::Error> for SettingsError {
    fn from(e: serde_json::Error) -> Self {
        SettingsError::Serde(e)
    }
}

impl From<toml::de::Error> for SettingsError {
    fn from(e: toml::de::Error) -> Self {
        SettingsError::TomlDe(e)
    }
}

impl From<toml::ser::Error> for SettingsError {
    fn from(e: toml::ser::Error) -> Self {
        SettingsError::TomlSer(e)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SettingsStore {
    #[serde(skip)]
    pub custom_path: Option<PathBuf>,
    #[serde(skip)]
    pub dirty: bool,
    /// Serialized snapshot of this store at the last sync point (load or
    /// save). Drives the field-granular diff used by [`save`](Self::save)
    /// and [`reload_from_disk`](Self::reload_from_disk): only fields that
    /// differ from the snapshot are written to disk (merged with the
    /// current on-disk state) or preserved from it, so concurrent
    /// instances merge their changes instead of clobbering each other.
    /// `pub` only because external crates construct `SettingsStore` via
    /// struct update syntax; never set it manually.
    #[serde(skip)]
    pub last_saved: Option<toml::Value>,
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub keybindings: KeyBindings,
    #[serde(default)]
    pub pinned_folders: PinnedFoldersSettings,
    #[serde(default)]
    pub window_position: WindowPosition,
    #[serde(default)]
    pub metadata_panel: MetadataPanelSettings,
    #[serde(default)]
    pub advanced: AdvancedSettings,
}

impl SettingsStore {
    pub fn config_path() -> PathBuf {
        if let Ok(val) = std::env::var("UI_TEST")
            && !val.is_empty()
        {
            return PathBuf::from("ui_test_config.toml");
        }
        let base = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("media-sort");
        std::fs::create_dir_all(&base).ok();
        base.join("config.toml")
    }

    pub fn load() -> Result<Self, SettingsError> {
        let toml_path = Self::config_path();
        if toml_path.exists() {
            let data = std::fs::read_to_string(&toml_path)?;
            let mut store: SettingsStore = toml::from_str(&data)?;
            store.custom_path = Some(toml_path);
            store.last_saved = toml::Value::try_from(&store).ok();
            return Ok(store);
        }

        // Search for legacy WPF C# JSON config to migrate
        if let Some(mut store) = {
            let wpf_base = dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Image Sort");
            let wpf_json_path = if cfg!(debug_assertions) {
                wpf_base.join("debug_config.json")
            } else {
                wpf_base.join("config.json")
            };

            if wpf_json_path.exists()
                && let Ok(data) = std::fs::read_to_string(&wpf_json_path)
                && let Some(store) = parse_wpf_settings(&data)
            {
                Some(store)
            } else {
                None
            }
        } {
            // `last_saved` stays `None` here so the save below writes the
            // full migrated store to disk instead of diffing to nothing.
            store.custom_path = Some(toml_path.clone());
            store.save()?;
            return Ok(store);
        }

        let mut store = Self {
            custom_path: Some(toml_path),
            ..Self::default()
        };
        store.last_saved = toml::Value::try_from(&store).ok();
        Ok(store)
    }

    pub fn save(&mut self) -> Result<(), SettingsError> {
        let curr = toml::Value::try_from(&*self).map_err(SettingsError::TomlSer)?;
        let last = self
            .last_saved
            .clone()
            .unwrap_or_else(|| toml::Value::Table(Default::default()));
        let diff = collect_diff_paths(&last, &curr);
        if diff.is_empty() {
            self.dirty = false;
            return Ok(());
        }

        let path = if let Some(ref custom) = self.custom_path {
            custom.clone()
        } else {
            Self::config_path()
        };
        let target = Self::resolve_target(&path)?;

        // Merge the changed fields onto the current on-disk state instead
        // of overwriting it wholesale: another instance may have written
        // its own changes since we last synced, and a full overwrite would
        // clobber them. A missing or unparsable file falls back to writing
        // the whole store (creation or corruption repair).
        let merged = match read_toml_file(&path) {
            Some(mut disk) => {
                apply_paths(&mut disk, &curr, &diff);
                disk
            }
            None => curr.clone(),
        };
        let data = toml::to_string_pretty(&merged).map_err(SettingsError::TomlSer)?;
        path_utils::atomic_write(&target, data.as_bytes()).map_err(SettingsError::Io)?;
        self.last_saved = Some(curr);
        self.dirty = false;
        Ok(())
    }

    /// Resolves the real write target of a config path: if the config path
    /// is a symlink (common on Linux where dotfiles are symlinked into
    /// ~/.config), the rename lands on the real target so the user's
    /// symlink stays intact.
    fn resolve_target(path: &Path) -> Result<PathBuf, SettingsError> {
        if path_utils::is_symlink(path) {
            // Resolve with read_link, not canonicalize: a dangling symlink
            // (target not created yet, common when setting up dotfiles)
            // makes canonicalize fail, and falling back to the link path
            // would replace the user's symlink with a regular file on the
            // next rename. read_link works even when the target does not
            // exist yet. If read_link itself fails (race, permission),
            // abort the save rather than clobber the symlink.
            let link_target = std::fs::read_link(path).map_err(|e| {
                SettingsError::Io(std::io::Error::new(
                    e.kind(),
                    format!("config symlink {path:?} target could not be resolved: {e}"),
                ))
            })?;
            if link_target.is_absolute() {
                Ok(link_target)
            } else if let Some(parent) = path.parent() {
                Ok(parent.join(link_target))
            } else {
                Ok(link_target)
            }
        } else {
            Ok(path.to_path_buf())
        }
    }

    /// Re-reads the config file and applies external changes to this
    /// store. Returns `Ok(true)` when the store actually changed.
    ///
    /// Merge semantics:
    /// - Unflushed local changes (fields that differ from the last saved
    ///   snapshot) always win over the on-disk state, so an in-progress
    ///   user edit is never reverted by a reload.
    /// - Session-scoped fields are never adopted from disk: this
    ///   instance's `window_position`, `general.last_opened_folder` and
    ///   `general.last_selected_media` stay local, plus
    ///   `pinned_folders` when `general.session_pinned_folders` is
    ///   enabled.
    /// - Everything else adopts the on-disk value.
    /// - Unknown keys (written by a future version) are ignored: they
    ///   never count as a change and never enter the saved snapshot, so a
    ///   later save cannot treat them as removals and delete them.
    ///
    /// A missing or unparsable file is a no-op (`Ok(false)`): startup
    /// already handles those cases, and a corrupt file must not destroy a
    /// running instance's settings.
    pub fn reload_from_disk(&mut self) -> Result<bool, SettingsError> {
        let path = if let Some(ref custom) = self.custom_path {
            custom.clone()
        } else {
            Self::config_path()
        };
        let Ok(data) = std::fs::read_to_string(&path) else {
            return Ok(false);
        };
        let Ok(disk) = toml::from_str::<toml::Value>(&data) else {
            return Ok(false);
        };

        let curr = toml::Value::try_from(&*self).map_err(SettingsError::TomlSer)?;
        let last = self
            .last_saved
            .clone()
            .unwrap_or_else(|| toml::Value::Table(Default::default()));

        let pending = collect_diff_paths(&last, &curr);
        let mut merged = disk.clone();
        apply_paths(&mut merged, &curr, &pending);

        // Session-scoped subtrees never adopt external values...
        for session_path in SESSION_SCOPED_PATHS {
            replace_path(&mut merged, &curr, session_path);
        }

        // ...and, when enabled, pinned folders join the session scope:
        // the choice is read from the merged (to-be-adopted) state so a
        // concurrent toggle of the setting takes effect in this reload.
        let mut incoming: SettingsStore =
            merged.clone().try_into().map_err(SettingsError::TomlDe)?;
        if incoming.general.session_pinned_folders {
            replace_path(&mut merged, &curr, "pinned_folders");
            incoming = merged.clone().try_into().map_err(SettingsError::TomlDe)?;
        }

        // Re-serialize the merged state: the comparison must ignore
        // unknown keys (fields written by a future version), otherwise
        // their mere presence would report a spurious change.
        let adopted = toml::Value::try_from(&incoming).map_err(SettingsError::TomlSer)?;

        // New baseline: the disk state re-serialized through this
        // version's structs. Unknown keys never enter the baseline —
        // otherwise the next save would see them in `last` but not `curr`
        // and delete them from disk. The baseline keeps the DISK values
        // (not the adopted ones) for pending fields so an unflushed local
        // change still differs from the baseline and is flushed by the
        // next save; session subtrees keep their old baseline for the
        // same reason and are never clobbered by this instance.
        let disk_store: SettingsStore = disk.clone().try_into().map_err(SettingsError::TomlDe)?;
        let mut new_last = toml::Value::try_from(&disk_store).map_err(SettingsError::TomlSer)?;
        for session_path in SESSION_SCOPED_PATHS {
            replace_path(&mut new_last, &last, session_path);
        }
        if incoming.general.session_pinned_folders {
            replace_path(&mut new_last, &last, "pinned_folders");
        }

        let changed = adopted != curr;
        if changed {
            let custom_path = self.custom_path.clone();
            let dirty = self.dirty;
            *self = incoming;
            self.custom_path = custom_path;
            self.dirty = dirty;
        }
        self.last_saved = Some(new_last);
        Ok(changed)
    }

    /// Mark the store as having unsaved changes. The next `Message::Tick`
    /// flushes via [`save_if_dirty`](Self::save_if_dirty); crash-resilience
    /// granularity is one tick (~16 ms) of pending mutations.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Flush pending changes to disk iff `dirty` is set. Returns `Ok(())`
    /// (no I/O) when there is nothing to write.
    pub fn save_if_dirty(&mut self) -> Result<(), SettingsError> {
        if self.dirty { self.save() } else { Ok(()) }
    }

    pub fn parse_wpf_settings(data: &str) -> Option<Self> {
        parse_wpf_settings(data)
    }
}

/// Dotted paths of the subtrees that are always scoped to one running
/// instance. `reload_from_disk` never adopts external values for these,
/// and their merge baseline stays local so unflushed local changes stay
/// pending. `pinned_folders` joins this set dynamically when
/// `general.session_pinned_folders` is enabled (see `reload_from_disk`).
const SESSION_SCOPED_PATHS: &[&str] = &[
    "window_position",
    "general.last_opened_folder",
    "general.last_selected_media",
];

/// Dotted paths where `a` and `b` differ. Tables are descended into
/// field-by-field; arrays and scalars are compared as whole leaves. An
/// empty string denotes "the whole document differs" (only reachable when
/// one side is not a table).
fn collect_diff_paths(a: &toml::Value, b: &toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_diff(a, b, "", &mut out);
    out
}

fn collect_diff(a: &toml::Value, b: &toml::Value, prefix: &str, out: &mut Vec<String>) {
    if a == b {
        return;
    }
    match (a.as_table(), b.as_table()) {
        (Some(ta), Some(tb)) => {
            for key in ta.keys() {
                let path = join_path(prefix, key);
                match tb.get(key) {
                    Some(vb) => collect_diff(
                        ta.get(key).expect("key came from ta.keys()"),
                        vb,
                        &path,
                        out,
                    ),
                    None => out.push(path),
                }
            }
            for key in tb.keys() {
                if !ta.contains_key(key) {
                    out.push(join_path(prefix, key));
                }
            }
        }
        _ => out.push(prefix.to_string()),
    }
}

fn join_path(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn value_at<'a>(root: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    let mut cur = root;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Copies every changed path from `src` into `dst`. A path present in
/// `src` sets (or replaces) the value; a path missing from `src` removes
/// the key from `dst`. An empty path replaces the whole document.
fn apply_paths(dst: &mut toml::Value, src: &toml::Value, paths: &[String]) {
    for path in paths {
        replace_path(dst, src, path);
    }
}

fn replace_path(dst: &mut toml::Value, src: &toml::Value, path: &str) {
    if path.is_empty() {
        *dst = src.clone();
        return;
    }
    match value_at(src, path) {
        Some(v) => set_path(dst, path, v.clone()),
        None => remove_path(dst, path),
    }
}

/// Sets `path` in `dst`, creating intermediate tables as needed. Malformed
/// intermediates (a scalar where a table is expected, e.g. a hand-edited
/// config) are replaced by empty tables rather than panicking.
fn set_path(dst: &mut toml::Value, path: &str, val: toml::Value) {
    let segs: Vec<&str> = path.split('.').collect();
    let mut cur = dst;
    for (i, seg) in segs.iter().enumerate() {
        if i == segs.len() - 1 {
            if !cur.is_table() {
                *cur = toml::Value::Table(Default::default());
            }
            cur.as_table_mut()
                .expect("cur was just normalized to a table")
                .insert((*seg).to_string(), val);
            return;
        }
        if !cur.is_table() {
            *cur = toml::Value::Table(Default::default());
        }
        let table = cur
            .as_table_mut()
            .expect("cur was just normalized to a table");
        if !table.contains_key(*seg) {
            table.insert((*seg).to_string(), toml::Value::Table(Default::default()));
        }
        cur = table.get_mut(*seg).expect("key was just inserted");
    }
}

fn remove_path(dst: &mut toml::Value, path: &str) {
    let segs: Vec<&str> = path.split('.').collect();
    if segs.len() == 1 {
        if let Some(table) = dst.as_table_mut() {
            table.remove(segs[0]);
        }
        return;
    }
    let mut cur = dst;
    for seg in &segs[..segs.len() - 1] {
        match cur.get_mut(seg) {
            Some(next) => cur = next,
            None => return,
        }
    }
    if let Some(table) = cur.as_table_mut() {
        table.remove(segs[segs.len() - 1]);
    }
}

fn read_toml_file(path: &Path) -> Option<toml::Value> {
    let data = std::fs::read_to_string(path).ok()?;
    toml::from_str(&data).ok()
}

#[derive(Debug, Deserialize)]
struct WpfHotkey {
    #[serde(rename = "Key")]
    key: i32,
    #[serde(rename = "Modifiers")]
    modifiers: i32,
}

fn map_wpf_key_to_rust(key_val: i32) -> String {
    match key_val {
        2 => "Backspace".to_string(),
        3 => "Tab".to_string(),
        6 => "Enter".to_string(),
        18 => "Space".to_string(),
        19 => "PageUp".to_string(),
        20 => "PageDown".to_string(),
        21 => "End".to_string(),
        22 => "Home".to_string(),
        23 => "Left".to_string(),
        24 => "Up".to_string(),
        25 => "Right".to_string(),
        26 => "Down".to_string(),
        27 => "Esc".to_string(),
        32 => "Delete".to_string(),
        val @ 34..=43 => ((val - 34 + i32::from(b'0')) as u8 as char).to_string(),
        val @ 44..=69 => ((val - 44 + i32::from(b'A')) as u8 as char).to_string(),
        val @ 74..=83 => ((val - 74 + i32::from(b'0')) as u8 as char).to_string(),
        val @ 90..=101 => {
            format!("F{}", val - 89)
        }
        _ => String::new(),
    }
}

fn parse_wpf_settings(data: &str) -> Option<SettingsStore> {
    let json: serde_json::Value = serde_json::from_str(data).ok()?;
    let mut store = SettingsStore::default();

    // 1. General settings
    if let Some(general) = json.get("General") {
        if let Some(val) = general.get("DarkMode").and_then(|v| v.as_bool()) {
            store.general.theme = if val {
                "Dark".to_string()
            } else {
                "Light".to_string()
            };
        }
        if let Some(val) = general
            .get("CheckForUpdatesOnStartup")
            .and_then(|v| v.as_bool())
        {
            store.general.check_for_updates_on_startup = val;
        }
        if let Some(val) = general
            .get("InstallPrereleaseBuilds")
            .and_then(|v| v.as_bool())
        {
            store.general.install_prerelease_builds = val;
        }
        if let Some(val) = general.get("AnimateGifs").and_then(|v| v.as_bool()) {
            store.general.animate_gifs = val;
        }
    }

    // 2. PinnedFolders settings
    if let Some(pinned) = json.get("PinnedFolders")
        && let Some(folders_val) = pinned.get("PinnedFolders")
        && let Some(arr) = folders_val.as_array()
    {
        store.pinned_folders.paths = arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }

    // 3. MetadataPanel settings
    if let Some(meta) = json.get("MetadataPanel") {
        if let Some(val) = meta.get("IsExpanded").and_then(|v| v.as_bool()) {
            store.metadata_panel.is_expanded = val;
        }
        if let Some(val) = meta.get("MetadataPanelWidth").and_then(|v| v.as_u64()) {
            store.metadata_panel.panel_width = val as u16;
        }
    }

    // 4. WindowPosition settings
    if let Some(win) = json.get("MainWindow") {
        if let Some(val) = win.get("Left").and_then(|v| v.as_i64()) {
            store.window_position.left = val as i32;
        }
        if let Some(val) = win.get("Top").and_then(|v| v.as_i64()) {
            store.window_position.top = val as i32;
        }
        if let Some(val) = win.get("Width").and_then(|v| v.as_u64()) {
            store.window_position.width = val as u32;
        }
        if let Some(val) = win.get("Height").and_then(|v| v.as_u64()) {
            store.window_position.height = val as u32;
        }
        if let Some(val) = win.get("IsMaximized").and_then(|v| v.as_bool()) {
            store.window_position.maximized = val;
        }
        if let Some(val) = win.get("ScreenCount").and_then(|v| v.as_u64()) {
            store.window_position.screen_count = val as u32;
        }
    }

    // 5. KeyBindings settings
    if let Some(kb_val) = json.get("KeyBindings") {
        let parse_binding = |key_name: &str| -> Option<crate::settings::keybindings::KeyBinding> {
            let val = kb_val.get(key_name)?;
            let wpf_hk: WpfHotkey = serde_json::from_value(val.clone()).ok()?;
            let rust_key = map_wpf_key_to_rust(wpf_hk.key);
            if rust_key.is_empty() {
                return None;
            }
            Some(crate::settings::keybindings::KeyBinding {
                key: crate::settings::keybindings::Key::parse(&rust_key)?,
                ctrl: (wpf_hk.modifiers & 2) != 0,
                shift: (wpf_hk.modifiers & 4) != 0,
                alt: (wpf_hk.modifiers & 1) != 0,
                meta: false,
            })
        };

        if let Some(b) = parse_binding("Move") {
            store.keybindings.move_to_folder = b;
        }
        if let Some(b) = parse_binding("Delete") {
            store.keybindings.delete = b;
        }
        if let Some(b) = parse_binding("Rename") {
            store.keybindings.rename = b;
        }
        if let Some(b) = parse_binding("GoLeft") {
            store.keybindings.go_left = b;
        }
        if let Some(b) = parse_binding("GoRight") {
            store.keybindings.go_right = b;
        }
        if let Some(b) = parse_binding("CreateFolder") {
            store.keybindings.create_folder = b;
        }
        if let Some(b) = parse_binding("FolderUp") {
            store.keybindings.folder_up = b;
        }
        if let Some(b) = parse_binding("FolderLeft") {
            store.keybindings.folder_left = b;
        }
        if let Some(b) = parse_binding("FolderDown") {
            store.keybindings.folder_down = b;
        }
        if let Some(b) = parse_binding("FolderRight") {
            store.keybindings.folder_right = b;
        }
        if let Some(b) = parse_binding("Undo") {
            store.keybindings.undo = b;
        }
        if let Some(b) = parse_binding("Redo") {
            store.keybindings.redo = b;
        }
        if let Some(b) = parse_binding("OpenFolder") {
            store.keybindings.open_folder = b;
        }
        if let Some(b) = parse_binding("OpenSelectedFolder") {
            store.keybindings.open_selected_folder = b;
        }
        if let Some(b) = parse_binding("Pin") {
            store.keybindings.pin = b;
        }
        if let Some(b) = parse_binding("PinSelected") {
            store.keybindings.pin_selected = b;
        }
        if let Some(b) = parse_binding("Unpin") {
            store.keybindings.unpin = b;
        }
        if let Some(b) = parse_binding("MoveSelectedPinnedFolderUp") {
            store.keybindings.move_pinned_up = b;
        }
        if let Some(b) = parse_binding("MoveSelectedPinnedFolderDown") {
            store.keybindings.move_pinned_down = b;
        }
        if let Some(b) = parse_binding("SearchImages") {
            store.keybindings.search_images = b;
        }
        if let Some(b) = parse_binding("ToggleMetadataPanel") {
            store.keybindings.toggle_metadata_panel = b;
        }
    }

    Some(store)
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use crate::settings::advanced::AdvancedSettings;
    use crate::settings::keybindings::Key;
    use crate::settings::metadata_panel::MetadataPanelSettings;
    use crate::settings::store::{SettingsError, SettingsStore};
    use crate::settings::window_position::WindowPosition;

    fn test_temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("media-sort-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        dir
    }

    fn test_temp_subdir() -> PathBuf {
        let dir = test_temp_dir().join(format!("sub-{}", rand_u32()));
        std::fs::create_dir_all(&dir).ok();
        dir
    }

    fn rand_u32() -> u32 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    }

    #[test]
    fn test_settings_default() {
        let settings = SettingsStore::default();
        assert_eq!(settings.general.theme, "Auto");
        assert!(settings.general.check_for_updates_on_startup);
        assert!(settings.general.animate_gifs);
    }

    #[test]
    fn test_settings_save_load_roundtrip() {
        let mut settings = SettingsStore::default();
        settings.general.theme = "Dark".to_string();
        settings.general.check_for_updates_on_startup = false;
        settings.general.animate_gifs = false;

        let json = serde_json::to_string(&settings).unwrap();
        let loaded: SettingsStore = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.general.theme, "Dark");
        assert!(!loaded.general.check_for_updates_on_startup);
        assert!(!loaded.general.animate_gifs);
    }

    #[test]
    fn test_settings_keybindings_defaults() {
        let kb = &SettingsStore::default().keybindings;

        let keys = [
            &kb.move_to_folder.key,
            &kb.delete.key,
            &kb.rename.key,
            &kb.go_left.key,
            &kb.go_right.key,
            &kb.create_folder.key,
            &kb.folder_up.key,
            &kb.folder_left.key,
            &kb.folder_down.key,
            &kb.folder_right.key,
            &kb.undo.key,
            &kb.redo.key,
            &kb.open_folder.key,
            &kb.open_selected_folder.key,
            &kb.pin.key,
            &kb.pin_selected.key,
            &kb.unpin.key,
            &kb.move_pinned_up.key,
            &kb.move_pinned_down.key,
            &kb.search_images.key,
            &kb.toggle_metadata_panel.key,
        ];

        assert_eq!(keys.len(), 21);
        for (i, key) in keys.iter().enumerate() {
            let name = key.display_name();
            assert!(!name.is_empty(), "keybinding {} has empty key", i);
        }
        assert_eq!(kb.previous_page.key, Key::PageUp);
        assert_eq!(kb.next_page.key, Key::PageDown);
    }

    #[test]
    fn test_settings_empty_json_uses_defaults() {
        let json = "{}";
        let settings: SettingsStore = serde_json::from_str(json).unwrap();
        assert_eq!(settings.general.theme, "Auto");
        assert!(settings.general.check_for_updates_on_startup);
        assert!(
            !settings
                .keybindings
                .move_to_folder
                .key
                .display_name()
                .is_empty()
        );
    }

    #[test]
    fn test_window_position_default() {
        let wp = WindowPosition::default();
        assert_eq!(wp.left, 100);
        assert_eq!(wp.top, 100);
        assert_eq!(wp.width, 1000);
        assert_eq!(wp.height, 600);
        assert!(!wp.maximized);
        assert_eq!(wp.screen_count, 1);
    }

    #[test]
    fn test_metadata_panel_settings_default() {
        let mps = MetadataPanelSettings::default();
        assert!(!mps.is_expanded);
        assert_eq!(mps.panel_width, 300);
    }

    #[test]
    fn test_advanced_settings_default() {
        let advanced = AdvancedSettings::default();
        assert!(!advanced.disable_hardware_decoding);
    }

    #[test]
    fn test_settings_load_corrupted_json() {
        let dir = std::env::temp_dir().join(format!("mediasort_config_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("test_config.json");

        std::fs::write(&config_path, "this is not valid json {{{").unwrap();

        let result = std::fs::read_to_string(&config_path)
            .map_err(SettingsError::from)
            .and_then(|data| {
                serde_json::from_str::<SettingsStore>(&data).map_err(SettingsError::from)
            });
        assert!(result.is_err());
        match result {
            Err(SettingsError::Serde(_)) => {}
            _ => panic!("Expected Serde error, got {:?}", result.err()),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_settings_load_truncated_toml() {
        let dir = std::env::temp_dir().join(format!("mediasort_config2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("test_config.toml");

        std::fs::write(&config_path, "[general]\ntheme = \"Dar").unwrap();

        let result = std::fs::read_to_string(&config_path)
            .map_err(SettingsError::from)
            .and_then(|data| toml::from_str::<SettingsStore>(&data).map_err(SettingsError::from));
        assert!(result.is_err());
        match result {
            Err(SettingsError::TomlDe(_)) => {}
            _ => panic!("Expected TomlDe error, got {:?}", result.err()),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_settings_load_extra_unknown_fields() {
        let dir = std::env::temp_dir().join(format!("mediasort_config3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("test_config.json");

        std::fs::write(
            &config_path,
            r#"{"general": {"theme": "Dark"}, "unknown_field": "should be ignored"}"#,
        )
        .unwrap();

        let result = std::fs::read_to_string(&config_path)
            .map_err(SettingsError::from)
            .and_then(|data| {
                serde_json::from_str::<SettingsStore>(&data).map_err(SettingsError::from)
            });
        assert!(result.is_ok());
        let settings = result.unwrap();
        assert_eq!(settings.general.theme, "Dark");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_wpf_settings_migration() {
        let raw_json = r#"{
            "General": {
                "DarkMode": true,
                "CheckForUpdatesOnStartup": false,
                "InstallPrereleaseBuilds": true,
                "AnimateGifs": false
            },
            "PinnedFolders": {
                "PinnedFolders": [
                    "/some/path/1",
                    "/some/path/2"
                ]
            },
            "MetadataPanel": {
                "IsExpanded": true,
                "MetadataPanelWidth": 250
            },
            "MainWindow": {
                "Left": 50,
                "Top": 60,
                "Width": 1200,
                "Height": 800,
                "IsMaximized": true,
                "ScreenCount": 2
            },
            "KeyBindings": {
                "Move": { "Key": 24, "Modifiers": 0 },
                "Delete": { "Key": 26, "Modifiers": 2 },
                "SearchImages": { "Key": 52, "Modifiers": 4 }
            }
        }"#;

        let store =
            SettingsStore::parse_wpf_settings(raw_json).expect("Failed to parse wpf settings");

        assert_eq!(store.general.theme, "Dark");
        assert!(!store.general.check_for_updates_on_startup);
        assert!(store.general.install_prerelease_builds);
        assert!(!store.general.animate_gifs);

        assert_eq!(store.pinned_folders.paths.len(), 2);
        assert_eq!(store.pinned_folders.paths[0], "/some/path/1");
        assert_eq!(store.pinned_folders.paths[1], "/some/path/2");

        assert!(store.metadata_panel.is_expanded);
        assert_eq!(store.metadata_panel.panel_width, 250);

        assert_eq!(store.window_position.left, 50);
        assert_eq!(store.window_position.top, 60);
        assert_eq!(store.window_position.width, 1200);
        assert_eq!(store.window_position.height, 800);
        assert!(store.window_position.maximized);
        assert_eq!(store.window_position.screen_count, 2);

        assert_eq!(store.keybindings.move_to_folder.key, Key::ArrowUp);
        assert!(!store.keybindings.move_to_folder.ctrl);
        assert_eq!(store.keybindings.delete.key, Key::ArrowDown);
        assert!(store.keybindings.delete.ctrl);
        assert_eq!(store.keybindings.search_images.key, Key::Character('I'));
        assert!(store.keybindings.search_images.shift);
    }

    #[test]
    fn test_parse_wpf_settings_empty_json() {
        let store = SettingsStore::parse_wpf_settings("{}");
        assert!(
            store.is_some(),
            "empty JSON should produce default settings"
        );
    }

    #[test]
    fn test_parse_wpf_settings_partial_json() {
        let json = r#"{"General": {"DarkMode": true}}"#;
        let store = SettingsStore::parse_wpf_settings(json);
        assert!(store.is_some());
        let s = store.unwrap();
        assert_eq!(s.general.theme, "Dark");
        assert!(s.general.animate_gifs);
    }

    #[test]
    fn test_parse_wpf_settings_pinned_folders_not_array() {
        let json = r#"{"PinnedFolders": {"PinnedFolders": "not an array"}}"#;
        let store = SettingsStore::parse_wpf_settings(json);
        assert!(
            store.is_some(),
            "should not crash on non-array pinned folders"
        );
        let s = store.unwrap();
        assert!(s.pinned_folders.paths.is_empty());
    }

    #[test]
    fn test_parse_wpf_settings_null_pinned_folders() {
        let json = r#"{"PinnedFolders": {"PinnedFolders": null}}"#;
        let store = SettingsStore::parse_wpf_settings(json);
        assert!(store.is_some());
        let s = store.unwrap();
        assert!(s.pinned_folders.paths.is_empty());
    }

    #[test]
    fn test_parse_wpf_settings_unknown_theme() {
        let json = r#"{"ThemeSettings": {"Theme": "SomeUnknownTheme"}}"#;
        let store = SettingsStore::parse_wpf_settings(json);
        assert!(store.is_some());
        let s = store.unwrap();
        assert!(!s.general.theme.is_empty());
    }

    #[test]
    fn test_settings_error_display() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "test io");
        let se = SettingsError::Io(io_err);
        assert!(se.to_string().contains("IO error"));

        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let se2 = SettingsError::Serde(json_err);
        assert!(se2.to_string().contains("Serialization error"));
    }

    #[test]
    fn test_settings_error_from_io() {
        let io_err = io::Error::other("test");
        let se: SettingsError = io_err.into();
        assert!(matches!(se, SettingsError::Io(_)));
    }

    #[test]
    fn test_settings_error_from_serde() {
        let json_err = serde_json::from_str::<i32>("not a number").unwrap_err();
        let se: SettingsError = json_err.into();
        assert!(matches!(se, SettingsError::Serde(_)));
    }

    #[test]
    fn test_settings_error_toml_display() {
        let toml_err = toml::from_str::<String>("[[[").unwrap_err();
        let se = SettingsError::TomlDe(toml_err);
        let s = se.to_string();
        assert!(s.contains("TOML deserialization"));

        let bad_val = f64::NAN;
        let toml_ser_err = toml::to_string(&bad_val).unwrap_err();
        let se2 = SettingsError::TomlSer(toml_ser_err);
        let s2 = se2.to_string();
        assert!(s2.contains("TOML serialization"));
    }

    #[test]
    fn test_settings_toml_roundtrip() {
        let mut settings = SettingsStore::default();
        settings.general.theme = "Dark".to_string();
        settings.general.locale = Some("de".to_string());
        settings.general.animate_gifs = false;
        settings.window_position.left = 42;
        settings.window_position.top = 99;
        settings.window_position.maximized = true;
        settings.metadata_panel.is_expanded = true;
        settings.metadata_panel.panel_width = 400;
        settings.pinned_folders.paths = vec!["/home/test".to_string(), "/tmp/foo".to_string()];

        let toml_str = toml::to_string_pretty(&settings).unwrap();
        let loaded: SettingsStore = toml::from_str(&toml_str).unwrap();

        assert_eq!(loaded.general.theme, "Dark");
        assert_eq!(loaded.general.locale, Some("de".to_string()));
        assert!(!loaded.general.animate_gifs);
        assert_eq!(loaded.window_position.left, 42);
        assert_eq!(loaded.window_position.top, 99);
        assert!(loaded.window_position.maximized);
        assert!(loaded.metadata_panel.is_expanded);
        assert_eq!(loaded.metadata_panel.panel_width, 400);
        assert_eq!(loaded.pinned_folders.paths, vec!["/home/test", "/tmp/foo"]);
        assert!(
            !loaded
                .keybindings
                .move_to_folder
                .key
                .display_name()
                .is_empty()
        );
    }

    #[test]
    fn test_settings_save_load_file_roundtrip() {
        let dir = test_temp_subdir();
        let config_file = dir.join("config.toml");

        let mut settings = SettingsStore::default();
        settings.general.theme = "Slate".to_string();
        settings.general.animate_gifs = false;
        settings.general.folder_tree_width = 300;

        let toml_str = toml::to_string_pretty(&settings).unwrap();
        std::fs::write(&config_file, &toml_str).unwrap();

        let loaded_str = std::fs::read_to_string(&config_file).unwrap();
        let loaded: SettingsStore = toml::from_str(&loaded_str).unwrap();
        assert_eq!(loaded.general.theme, "Slate");
        assert!(!loaded.general.animate_gifs);
        assert_eq!(loaded.general.folder_tree_width, 300);
    }

    #[cfg(unix)]
    #[test]
    fn test_save_via_dangling_symlink_preserves_link() {
        use std::os::unix::fs::symlink;

        // A dangling symlink (target not created yet) must survive a save:
        // the write has to land on the target path, not replace the link.
        let dir = test_temp_subdir();
        let config_path = dir.join("config.toml");
        let dotfiles_dir = dir.join("dotfiles");
        std::fs::create_dir_all(&dotfiles_dir).unwrap();
        let real_target = dotfiles_dir.join("config.toml");
        symlink(&real_target, &config_path).unwrap();
        assert!(
            config_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );

        let mut settings = SettingsStore {
            custom_path: Some(config_path.clone()),
            ..Default::default()
        };
        settings.general.theme = "Dark".to_string();
        settings.save().unwrap();

        // The symlink must still be a symlink...
        assert!(
            config_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "save must not replace the user's symlink"
        );
        // ...and the settings must have landed on the (now existing) target.
        assert!(real_target.exists(), "target file must have been created");
        let loaded: SettingsStore =
            toml::from_str(&std::fs::read_to_string(&real_target).unwrap()).unwrap();
        assert_eq!(loaded.general.theme, "Dark");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Loads a store from `path` the way the production `load()` does:
    /// with the custom path set and the saved snapshot baseline filled.
    fn settings_at(path: &std::path::Path) -> SettingsStore {
        let mut s: SettingsStore = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        s.custom_path = Some(path.to_path_buf());
        s.last_saved = toml::Value::try_from(&s).ok();
        s
    }

    #[test]
    fn test_save_merges_changed_fields_with_disk() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");

        // First instance writes the initial file.
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.save().unwrap();

        // Second instance changes a keybinding on disk.
        let mut b = settings_at(&config);
        b.keybindings.undo.key = Key::Character('Z');
        b.keybindings.undo.ctrl = true;
        b.save().unwrap();

        // First instance changes only the theme and saves. Its save must
        // merge onto the file, keeping B's keybinding change.
        a.general.theme = "Dark".to_string();
        a.save().unwrap();

        let merged = settings_at(&config);
        assert_eq!(merged.general.theme, "Dark");
        assert_eq!(merged.keybindings.undo.key, Key::Character('Z'));
        assert!(merged.keybindings.undo.ctrl);

        // A also adopts B's keybinding on the next reload.
        assert!(a.reload_from_disk().unwrap());
        assert_eq!(a.keybindings.undo.key, Key::Character('Z'));
        assert!(a.keybindings.undo.ctrl);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_save_without_changes_skips_write() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut s = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        s.save().unwrap();
        assert!(config.exists());
        std::fs::remove_file(&config).unwrap();
        // Nothing changed since the last save: the file must not be recreated.
        s.save().unwrap();
        assert!(!config.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_save_preserves_unknown_keys_on_disk() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.save().unwrap();
        // Simulate a future app version writing a key this version does not
        // know about.
        let extra = std::fs::read_to_string(&config).unwrap() + "\n[futuresection]\nkey = 1\n";
        std::fs::write(&config, extra).unwrap();
        a.general.theme = "Dark".to_string();
        a.save().unwrap();
        let data = std::fs::read_to_string(&config).unwrap();
        assert!(
            data.contains("[futuresection]"),
            "unknown keys must survive a merged save: {data}"
        );
        assert!(data.contains("Dark"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_reload_then_save_preserves_unknown_keys() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.save().unwrap();
        // A future app version wrote a key this version does not know about.
        let extra = std::fs::read_to_string(&config).unwrap() + "\n[futuresection]\nkey = 1\n";
        std::fs::write(&config, extra).unwrap();

        // Unknown keys alone must not count as an external change...
        assert!(!a.reload_from_disk().unwrap());
        // ...and a subsequent save must not delete them.
        a.general.theme = "Dark".to_string();
        a.save().unwrap();
        let data = std::fs::read_to_string(&config).unwrap();
        assert!(
            data.contains("[futuresection]"),
            "unknown keys must survive reload + save: {data}"
        );
        assert!(data.contains("Dark"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_reload_with_real_change_then_save_preserves_unknown_keys() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.save().unwrap();

        // A future app version wrote a real change AND an unknown key.
        let mut b = settings_at(&config);
        b.general.animate_gifs = false;
        b.save().unwrap();
        let extra = std::fs::read_to_string(&config).unwrap() + "\n[futuresection]\nkey = 1\n";
        std::fs::write(&config, extra).unwrap();

        assert!(a.reload_from_disk().unwrap());
        assert!(!a.general.animate_gifs);
        a.general.theme = "Dark".to_string();
        a.save().unwrap();
        let data = std::fs::read_to_string(&config).unwrap();
        assert!(
            data.contains("[futuresection]"),
            "unknown keys must survive reload + save: {data}"
        );
        assert!(data.contains("Dark"));
        assert!(!settings_at(&config).general.animate_gifs);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_reload_adopts_external_settings() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.save().unwrap();

        let mut b = settings_at(&config);
        b.general.theme = "Dark".to_string();
        b.general.animate_gifs = false;
        b.save().unwrap();

        assert!(a.reload_from_disk().unwrap());
        assert_eq!(a.general.theme, "Dark");
        assert!(!a.general.animate_gifs);

        // A second reload with no new disk content reports no change.
        assert!(!a.reload_from_disk().unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_reload_excludes_session_scoped_fields() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.save().unwrap();

        let mut b = settings_at(&config);
        b.general.theme = "Dark".to_string();
        b.general.last_opened_folder = Some("/elsewhere".to_string());
        b.general.last_selected_media = Some("/elsewhere/file.jpg".to_string());
        b.window_position.left = 555;
        b.save().unwrap();

        assert!(a.reload_from_disk().unwrap());
        assert_eq!(a.general.theme, "Dark");
        // Session-scoped values stay local.
        assert_eq!(a.general.last_opened_folder, None);
        assert_eq!(a.general.last_selected_media, None);
        assert_eq!(a.window_position.left, WindowPosition::default().left);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_reload_shares_pinned_folders_by_default() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.save().unwrap();

        let mut b = settings_at(&config);
        b.pinned_folders.paths = vec!["/b/pin".to_string()];
        b.save().unwrap();

        // Default: pinned folders are shared and adopted on reload.
        assert!(a.reload_from_disk().unwrap());
        assert_eq!(a.pinned_folders.paths, vec!["/b/pin".to_string()]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_reload_session_pinned_folders_are_not_adopted() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.pinned_folders.paths = vec!["/a/pin".to_string()];
        a.save().unwrap();

        let mut b = settings_at(&config);
        b.general.session_pinned_folders = true;
        b.pinned_folders.paths = vec!["/b/pin".to_string()];
        b.save().unwrap();

        // The toggle itself is a shared setting and is adopted...
        assert!(a.reload_from_disk().unwrap());
        assert!(a.general.session_pinned_folders);
        // ...but the pins stay local.
        assert_eq!(a.pinned_folders.paths, vec!["/a/pin".to_string()]);

        // A save by A must not clobber B's pins on disk either: the
        // baseline keeps A's own pins, so nothing is dirty.
        a.save().unwrap();
        assert_eq!(
            settings_at(&config).pinned_folders.paths,
            vec!["/b/pin".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_reload_unflushed_pin_change_survives_session_scope() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.pinned_folders.paths = vec!["/a/pin".to_string()];
        a.save().unwrap();

        let mut b = settings_at(&config);
        b.general.session_pinned_folders = true;
        b.save().unwrap();

        // A changed its pins but has not flushed yet.
        a.pinned_folders.paths = vec!["/a/pin".to_string(), "/a/pin2".to_string()];
        a.mark_dirty();
        assert!(a.reload_from_disk().unwrap());
        // The unflushed local pin change survives the reload...
        assert_eq!(
            a.pinned_folders.paths,
            vec!["/a/pin".to_string(), "/a/pin2".to_string()]
        );
        // ...and is flushed by the next save.
        a.save().unwrap();
        assert_eq!(
            settings_at(&config).pinned_folders.paths,
            vec!["/a/pin".to_string(), "/a/pin2".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_reload_preserves_pending_local_changes() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.save().unwrap();

        let mut b = settings_at(&config);
        b.general.animate_gifs = false;
        b.save().unwrap();

        // A changes the theme but has not saved yet.
        a.general.theme = "Dark".to_string();
        a.mark_dirty();
        assert!(a.reload_from_disk().unwrap());
        // The unflushed local change survives the reload...
        assert_eq!(a.general.theme, "Dark");
        assert!(a.dirty);
        // ...the external change is adopted...
        assert!(!a.general.animate_gifs);
        // ...and a save persists both.
        a.save().unwrap();
        let merged = settings_at(&config);
        assert_eq!(merged.general.theme, "Dark");
        assert!(!merged.general.animate_gifs);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_reload_missing_or_corrupt_file_is_noop() {
        let dir = test_temp_subdir();
        let config = dir.join("config.toml");
        let mut a = SettingsStore {
            custom_path: Some(config.clone()),
            ..SettingsStore::default()
        };
        a.general.theme = "Dark".to_string();
        assert!(!a.reload_from_disk().unwrap());
        assert_eq!(a.general.theme, "Dark");

        std::fs::write(&config, "not valid toml {{{").unwrap();
        assert!(!a.reload_from_disk().unwrap());
        assert_eq!(a.general.theme, "Dark");

        std::fs::remove_dir_all(&dir).ok();
    }
}
