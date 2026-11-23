mod audio;
mod cache;
mod create_folder_modal;
pub mod drag_drop;
mod folder;
mod folder_inspection;
pub mod media_errors;
mod media_grid;
mod metadata;
mod rename_modal;
mod settings_ui;

pub use audio::AudioPlaybackState;
pub use cache::CacheState;
pub use create_folder_modal::CreateFolderModalState;
pub use drag_drop::DragDropState;
pub use folder::FolderState;
pub use media_grid::{MediaGridScrollState, MediaGridState, SearchState};
pub use metadata::MetadataPanelState;
pub use rename_modal::RenameModalState;
pub use settings_ui::SettingsUiState;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use media_sort_backend::filesystem::scanner::scan_media_entries;
use media_sort_core::history::History;
use media_sort_core::models::PinnedFolder;
use media_sort_core::path_utils::paths_equal;
use media_sort_core::settings::store::SettingsStore;

use folder::tree;

/// A transient user-facing status banner: text plus a monotonic expiry
/// stamp. Rendered by the main layout, cleared on the next tick whose
/// `Instant` is past `expires_at` — the comparison uses the tick's own
/// monotonic clock, so a suspended process cannot keep a stale banner alive
/// for the leftover duration.
#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub expires_at: Instant,
}

#[cfg_attr(feature = "demo", iced_automation::state(crate::message::Message))]
pub struct AppState {
    pub history: History,
    pub settings: SettingsStore,
    pub should_exit: bool,
    pub l10n: media_sort_core::l10n::Localization,
    pub show_credits: bool,

    pub folder: FolderState,
    pub media_grid: MediaGridState,
    pub rename: RenameModalState,
    pub create_folder: CreateFolderModalState,
    pub video: iced_mpv::VideoState,
    pub audio: AudioPlaybackState,
    pub cache: CacheState,
    pub metadata: MetadataPanelState,
    pub settings_ui: SettingsUiState,
    pub drag_drop: DragDropState,
    pub selected_file_size: Option<u64>,

    /// Transient user-facing status banner (text + monotonic expiry
    /// instant). Rendered by the main layout, cleared on the next tick
    /// after expiry.
    pub status_message: Option<StatusMessage>,

    /// The OS light/dark preference, resolved at startup via
    /// `iced::system::theme()` and kept live by the `SystemThemeChanged`
    /// message (`iced::system::theme_changes()` subscription). Used by
    /// `app::theme()` to resolve the `"Auto"` theme setting; `None` (the
    /// initial value before the first resolution) falls back to Light.
    pub system_theme: iced::theme::Mode,

    /// Monotonic counter folded into the filesystem subscription
    /// identity. Bumped when the current folder is deleted and recreated
    /// at the same path: the path list (the identity's key) is unchanged,
    /// so without the bump iced would keep the stale subscription and the
    /// OS watch on the dead inode would never be re-established.
    pub watch_generation: AtomicU64,

    /// Earliest `Instant` at which the config file may be polled for
    /// external changes (`SettingsStore::reload_from_disk`). The tick
    /// handler re-checks once per second so a file read + TOML parse is
    /// not performed on every 16 ms tick. Initialized one second after
    /// construction so startup ticks (and tests) never poll immediately.
    /// Omitted from demo builds (headless renders must stay deterministic).
    #[cfg(not(feature = "demo"))]
    pub settings_reload_at: Instant,

    #[cfg(feature = "velopack")]
    pub pending_update: Option<velopack::UpdateInfo>,
    #[cfg(feature = "velopack")]
    pub show_update_prompt: bool,
}

impl AppState {
    pub fn new(settings: SettingsStore) -> Self {
        let pinned_folders: Vec<PinnedFolder> = settings
            .pinned_folders
            .paths
            .iter()
            .map(|p| {
                let path = PathBuf::from(p);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.clone());
                PinnedFolder { path, name }
            })
            .collect();

        let panel_expanded = settings.metadata_panel.is_expanded;

        let detected_locale = match &settings.general.locale {
            Some(locale) => locale.as_str(),
            None => media_sort_core::l10n::detect_locale(),
        };
        let l10n = media_sort_core::l10n::Localization::init(detected_locale);
        let search_placeholder = l10n.tr("keybindings-search-images");
        let rename_placeholder = l10n.tr("ui-enter-new-name");
        let create_folder_placeholder = l10n.tr("ui-folder-name-placeholder");

        let folder = FolderState {
            pinned_folders,
            ..Default::default()
        };

        let media_grid = MediaGridState {
            search: SearchState {
                placeholder: search_placeholder,
                ..Default::default()
            },
            ..Default::default()
        };

        let rename = RenameModalState {
            placeholder: rename_placeholder,
            ..Default::default()
        };

        let create_folder = CreateFolderModalState {
            create_folder_placeholder,
            ..Default::default()
        };

        let metadata = MetadataPanelState {
            panel_expanded,
            ..Default::default()
        };

        Self {
            history: History::new(),
            settings,
            should_exit: false,
            l10n,
            show_credits: false,
            folder,
            media_grid,
            rename,
            create_folder,
            video: iced_mpv::VideoState::default(),
            audio: AudioPlaybackState::new(),
            cache: CacheState::new(),
            metadata,
            settings_ui: SettingsUiState::default(),
            drag_drop: DragDropState::new(),
            selected_file_size: None,
            status_message: None,
            system_theme: iced::theme::Mode::None,
            watch_generation: AtomicU64::new(0),
            #[cfg(not(feature = "demo"))]
            settings_reload_at: Instant::now() + Duration::from_secs(1),
            #[cfg(feature = "velopack")]
            pending_update: None,
            #[cfg(feature = "velopack")]
            show_update_prompt: false,
            #[cfg(feature = "demo")]
            automation: Default::default(),
        }
    }

    /// Show a transient status banner that auto-expires after 5 seconds
    /// (cleared on the next tick whose `Instant` has passed). Used to
    /// surface refused actions such as symbolic-link drops, which
    /// previously failed silently.
    pub fn set_status(&mut self, text: String) {
        self.status_message = Some(StatusMessage {
            text,
            expires_at: Instant::now() + Duration::from_secs(5),
        });
    }

    pub fn is_update_prompt_visible(&self) -> bool {
        #[cfg(feature = "velopack")]
        {
            self.show_update_prompt
        }
        #[cfg(not(feature = "velopack"))]
        {
            false
        }
    }

    /// Clear all active preview state (metadata, full image, file size,
    /// video selection, and audio playback/cover) without altering audio
    /// volume, mute, or the underlying player instance.
    pub fn clear_active_preview(&mut self) {
        self.metadata.current = None;
        self.cache.selected_image = None;
        self.selected_file_size = None;
        self.video.select(None);
        crate::update::audio::reset(self);
    }

    /// Persist the current UI pinned-folder list into `settings.pinned_folders.paths`.
    pub fn sync_pinned_folders_to_settings(&mut self) {
        self.settings.pinned_folders.paths = self
            .folder
            .pinned_folders
            .iter()
            .map(|p| p.path.display().to_string())
            .collect();
    }

    pub fn open_folder(&mut self, path: &Path) {
        // Symlinked folders are followed transparently: the tree, the media
        // scanner and the watcher all operate on the real target path, so
        // entering a symlink shows and works on its final destination.
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path = canonical.as_path();
        self.folder.current_folder = Some(path.to_path_buf());
        self.settings.general.last_opened_folder = Some(path.to_string_lossy().to_string());
        self.settings.mark_dirty();
        self.history.clear();
        // A banner set by a drop (or a refused action) must not linger
        // across the folder switch.
        self.status_message = None;
        self.media_grid.entries.clear();
        self.media_grid.rebuild_lower_names();
        self.media_grid.selected_index = None;
        // Cancel any watcher-driven refresh state: the folder switch starts
        // a fresh extend-mode scan below.
        self.media_grid.scan_replace = false;
        self.media_grid.scan_buffer.clear();
        self.media_grid.pending_refresh = false;
        self.media_grid.refresh_select_path = None;
        // The scroll snapshot belongs to the previous folder's content
        // (possibly thousands of cards); without a reset the first render
        // of the new, smaller folder would slice the entry list past its
        // end. `GridScrolled` events keep it in sync afterwards, and
        // `viewport_window` clamps defensively either way.
        self.media_grid.scroll.offset_x = 0.0;
        self.folder.selected_folder = None;
        self.folder.selected_folder_idx = None;
        self.cache.image_cache.clear();
        self.cache.media_errors.clear();
        self.clear_active_preview();

        self.folder.tree_refresh_pending = false;
        self.start_async_folder_tree();

        self.media_grid.scan_receiver =
            Some(scan_media_entries(path, self.settings.general.animate_gifs));
        self.media_grid.pending_select_index = Some(0);
    }

    /// Kick off an asynchronous media scan of `current_folder`. The GUI
    /// thread drops to an empty grid immediately; `poll_background_channels`
    /// drains the receiver on subsequent `Tick`s and finally calls
    /// `select_and_load_entry(state, select_idx)` once the scan finishes.
    /// Used by `open_folder` and by Undo/Redo — routes what used to be a
    /// blocking on-UI-thread rescan into the existing async pipeline.
    pub fn start_async_media_scan(&mut self, select_idx: usize) {
        let Some(folder) = self.folder.current_folder.clone() else {
            return;
        };
        self.media_grid.entries.clear();
        self.media_grid.rebuild_lower_names();
        self.media_grid.selected_index = None;
        // Cancel any watcher-driven refresh state: this is a full
        // extend-mode rescan (initial load / undo / redo).
        self.media_grid.scan_replace = false;
        self.media_grid.scan_buffer.clear();
        self.media_grid.pending_refresh = false;
        self.media_grid.refresh_select_path = None;
        // Clear active preview (including video selection) so a late
        // FrameReady or CoverLoaded from the previously-selected media can't
        // repopulate preview state during the rescan.
        self.clear_active_preview();
        self.media_grid.scan_receiver = Some(scan_media_entries(
            &folder,
            self.settings.general.animate_gifs,
        ));
        self.media_grid.pending_select_index = Some(select_idx);
    }

    /// Watcher-driven rescan of the current folder that REPLACES the grid
    /// entries when it finishes (unlike `open_folder`/Undo/Redo, the
    /// existing entries stay visible while the scan runs, so external
    /// changes never blank the grid). Coalesces: if a scan is already in
    /// flight, the refresh is deferred until it completes — restarting on
    /// every event batch of a large copy would starve the scanner.
    pub fn start_media_refresh(&mut self) {
        let Some(folder) = self.folder.current_folder.clone() else {
            return;
        };
        if self.media_grid.scan_receiver.is_some() {
            self.media_grid.pending_refresh = true;
            return;
        }
        self.media_grid.pending_refresh = false;
        self.begin_refresh_scan(&folder);
    }

    pub(crate) fn begin_refresh_scan(&mut self, folder: &Path) {
        // Capture the selected entry's path so the selection can be restored
        // by path after the replacement lands (indices shift on re-sort).
        self.media_grid.refresh_select_path = self.media_grid.selected_index.and_then(|idx| {
            self.media_grid
                .filtered_entries()
                .get(idx)
                .map(|e| e.path.clone())
        });
        self.media_grid.scan_replace = true;
        self.media_grid.scan_buffer.clear();
        self.media_grid.scan_receiver = Some(scan_media_entries(
            folder,
            self.settings.general.animate_gifs,
        ));
    }

    /// Watcher-driven folder tree rebuild. Coalesces like
    /// [`start_media_refresh`](Self::start_media_refresh): while a rebuild
    /// is in flight the request is deferred, so an event storm cannot spawn
    /// a thread per batch.
    pub fn request_folder_tree_refresh(&mut self) {
        if self.folder.current_folder.is_none() {
            return;
        }
        if self.folder.folder_tree_receiver.is_some() {
            self.folder.tree_refresh_pending = true;
            return;
        }
        self.folder.tree_refresh_pending = false;
        self.start_async_folder_tree();
    }

    /// The directories whose direct children are currently displayed:
    /// every expanded tree node, the current folder (its children are the
    /// media grid), and the current folder's PARENT. The parent is
    /// watched so a rename/delete of the current folder itself stays
    /// visible on backends that can't report self-events (Windows
    /// delivers nothing for a deleted current folder unless the parent is
    /// watched). Watched non-recursively by the filesystem subscription;
    /// canonicalized so a symlinked node is watched at its real target
    /// (matching how the tree displays it).
    pub fn watched_directories(&self) -> Vec<PathBuf> {
        let mut set: HashSet<PathBuf> = tree::collect_expanded_paths(&self.folder.folder_tree);
        if let Some(ref cur) = self.folder.current_folder {
            set.insert(cur.clone());
            if let Some(parent) = cur.parent().filter(|p| !p.as_os_str().is_empty()) {
                set.insert(parent.to_path_buf());
            }
        }
        let mut dirs: Vec<PathBuf> = set
            .into_iter()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.canonicalize().unwrap_or(p))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs
    }

    /// Whether `path` (path-equality, canonicalization-aware) appears as a
    /// node anywhere in the current folder tree. Used to decide whether a
    /// removed path was a displayed folder (and therefore a tree rebuild is
    /// needed) — after removal the path can no longer be statted.
    pub fn tree_contains_path(&self, path: &Path) -> bool {
        fn walk(nodes: &[media_sort_core::models::FolderNode], path: &Path) -> bool {
            nodes
                .iter()
                .any(|n| paths_equal(&n.path, path) || walk(&n.children, path))
        }
        walk(&self.folder.folder_tree, path)
    }

    /// Forces the filesystem subscription to restart on the next update
    /// cycle by changing its identity. Used when a watched directory was
    /// deleted and recreated at the same path — the path list key is
    /// unchanged, but the OS watch on the old inode is dead.
    pub fn bump_watch_generation(&mut self) {
        self.watch_generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn build_folder_tree(&mut self) {
        if self.folder.current_folder.is_none() {
            return;
        }
        self.folder.folder_tree_receiver = None;
        self.folder.tree_refresh_pending = false;
        let expanded_paths = tree::collect_expanded_paths(&self.folder.folder_tree);
        let root = self
            .folder
            .current_folder
            .clone()
            .expect("current_folder must be Some since we checked it is not None above");
        self.folder.folder_tree =
            tree::build_tree_nodes_data(&root, &self.folder.pinned_folders, &expanded_paths);
        self.folder.invalidate_visible_folders_cache();
        self.folder.sync_selected_idx();
    }

    pub fn start_async_folder_tree(&mut self) {
        let Some(ref current) = self.folder.current_folder else {
            return;
        };
        let root = current.clone();
        let pinned = self.folder.pinned_folders.clone();
        let expanded_paths = tree::collect_expanded_paths(&self.folder.folder_tree);

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let tree = tree::build_tree_nodes_data(&root, &pinned, &expanded_paths);
            let _ = tx.send(tree);
        });
        self.folder.folder_tree_receiver = Some(rx);
    }

    pub fn toggle_folder_expand(&mut self, path: &Path, idx: usize) {
        let current = self.folder.current_folder.clone();
        let mut running_idx = 0;
        let toggled = tree::toggle_expand_recursive(
            &mut self.folder.folder_tree,
            path,
            idx,
            &mut running_idx,
            current.as_deref(),
        );
        // The click index can be stale (a tree rebuild between render and
        // message delivery); fall back to resolving the path's current flat
        // index so the correct node is still toggled.
        if !toggled && let Some(current_idx) = tree::flat_index_of(&self.folder.folder_tree, path) {
            let mut running_idx = 0;
            let _ = tree::toggle_expand_recursive(
                &mut self.folder.folder_tree,
                path,
                current_idx,
                &mut running_idx,
                current.as_deref(),
            );
        }
        self.folder.invalidate_visible_folders_cache();
        self.folder.sync_selected_idx();
    }

    #[allow(dead_code)]
    pub fn pin_current_folder(&mut self) {
        if let Some(ref folder) = self.folder.current_folder {
            let name = folder
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let pinned = PinnedFolder {
                path: folder.clone(),
                name,
            };
            if !self
                .folder
                .pinned_folders
                .iter()
                .any(|p| p.path == pinned.path)
            {
                self.folder.pinned_folders.push(pinned);
                self.sync_pinned_folders_to_settings();
                self.build_folder_tree();
            }
        }
    }

    pub fn unpin_folder(&mut self, path: &Path) {
        self.folder.pinned_folders.retain(|p| p.path != path);
        self.sync_pinned_folders_to_settings();
        self.build_folder_tree();
    }

    pub fn pin_folder(&mut self, path: &Path) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let pinned = PinnedFolder {
            path: path.to_path_buf(),
            name,
        };
        if !self
            .folder
            .pinned_folders
            .iter()
            .any(|p| p.path == pinned.path)
        {
            self.folder.pinned_folders.push(pinned);
            self.sync_pinned_folders_to_settings();
            self.build_folder_tree();
        }
    }

    pub fn move_pinned_folder_up(&mut self, path: &Path) {
        if let Some(pos) = self
            .folder
            .pinned_folders
            .iter()
            .position(|p| p.path == path)
            && pos > 0
        {
            self.folder.pinned_folders.swap(pos, pos - 1);
            // Index 0 is the current-folder root; pinned folders start at 1.
            if pos + 1 < self.folder.folder_tree.len() {
                self.folder.folder_tree.swap(pos + 1, pos);
            }
            self.folder.invalidate_visible_folders_cache();
            self.sync_pinned_folders_to_settings();
            self.settings.mark_dirty();
        }
    }

    pub fn move_pinned_folder_down(&mut self, path: &Path) {
        if let Some(pos) = self
            .folder
            .pinned_folders
            .iter()
            .position(|p| p.path == path)
            && pos < self.folder.pinned_folders.len() - 1
        {
            self.folder.pinned_folders.swap(pos, pos + 1);
            // Index 0 is the current-folder root; pinned folders start at 1.
            if pos + 2 < self.folder.folder_tree.len() {
                self.folder.folder_tree.swap(pos + 1, pos + 2);
            }
            self.folder.invalidate_visible_folders_cache();
            self.sync_pinned_folders_to_settings();
            self.settings.mark_dirty();
        }
    }

    pub fn swap_pinned_folders(&mut self, pos_a: usize, pos_b: usize) {
        self.folder.pinned_folders.swap(pos_a, pos_b);
        if pos_a + 1 < self.folder.folder_tree.len() && pos_b + 1 < self.folder.folder_tree.len() {
            self.folder.folder_tree.swap(pos_a + 1, pos_b + 1);
        }
        self.folder.invalidate_visible_folders_cache();
        self.sync_pinned_folders_to_settings();
    }

    /// Re-derives the pinned-folder UI list from the settings store after
    /// an external settings reload adopted changed pinned folders. In-app
    /// pin operations write both sides, so a mismatch can only come from
    /// another instance.
    #[cfg(not(feature = "demo"))]
    pub fn sync_pinned_folders_from_settings(&mut self) {
        let current: Vec<String> = self
            .folder
            .pinned_folders
            .iter()
            .map(|p| p.path.display().to_string())
            .collect();
        if current == self.settings.pinned_folders.paths {
            return;
        }
        self.folder.pinned_folders = self
            .settings
            .pinned_folders
            .paths
            .iter()
            .map(|p| {
                let path = PathBuf::from(p);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.clone());
                PinnedFolder { path, name }
            })
            .collect();
        self.build_folder_tree();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_sort_backend::filesystem::scanner::classify_media;
    use media_sort_core::media_type::MediaType;
    use media_sort_core::models::{FolderNode, MediaEntry, PinnedFolder};
    use media_sort_core::settings::store::SettingsStore;
    use std::path::{Path, PathBuf};

    #[test]
    fn test_classify_media_image() {
        assert_eq!(
            classify_media(Path::new("test.jpg"), true).0,
            MediaType::Image
        );
        assert_eq!(
            classify_media(Path::new("test.png"), true).0,
            MediaType::Image
        );
        assert_eq!(
            classify_media(Path::new("test.jpeg"), true).0,
            MediaType::Image
        );
        assert_eq!(
            classify_media(Path::new("test.bmp"), true).0,
            MediaType::Image
        );
        assert_eq!(
            classify_media(Path::new("vector.svg"), true).0,
            MediaType::Image
        );
        assert_eq!(
            classify_media(Path::new("icon.SVGZ"), true).0,
            MediaType::Image
        );
    }

    #[test]
    fn test_classify_media_video() {
        assert_eq!(
            classify_media(Path::new("test.mp4"), true).0,
            MediaType::Video
        );
        assert_eq!(
            classify_media(Path::new("test.mkv"), true).0,
            MediaType::Video
        );
        assert_eq!(
            classify_media(Path::new("test.webm"), true).0,
            MediaType::Video
        );
        assert_eq!(
            classify_media(Path::new("test.mov"), true).0,
            MediaType::Video
        );
        assert_eq!(
            classify_media(Path::new("test.gif"), true).0,
            MediaType::Video
        );
    }

    #[test]
    fn test_classify_media_audio() {
        assert_eq!(
            classify_media(Path::new("test.mp3"), true).0,
            MediaType::Audio
        );
        assert_eq!(
            classify_media(Path::new("test.flac"), true).0,
            MediaType::Audio
        );
        assert_eq!(
            classify_media(Path::new("test.wav"), true).0,
            MediaType::Audio
        );
        assert_eq!(
            classify_media(Path::new("test.ogg"), true).0,
            MediaType::Audio
        );
    }

    #[test]
    fn test_classify_media_unknown_fallback() {
        assert_eq!(
            classify_media(Path::new("test.xyz"), true).0,
            MediaType::Image
        );
        assert_eq!(classify_media(Path::new("test"), true).0, MediaType::Image);
        assert_eq!(
            classify_media(Path::new("test.doc"), true).0,
            MediaType::Image
        );
    }

    #[test]
    fn test_classify_media_case_insensitive() {
        assert_eq!(
            classify_media(Path::new("test.JPG"), true).0,
            MediaType::Image
        );
        assert_eq!(
            classify_media(Path::new("test.MP3"), true).0,
            MediaType::Audio
        );
        assert_eq!(
            classify_media(Path::new("test.Mp4"), true).0,
            MediaType::Video
        );
    }

    #[test]
    fn test_classify_media_static_gif() {
        let dir = std::env::temp_dir().join("mediasort_test_static_gif");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("static.gif");

        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        img.save(&path).unwrap();

        assert_eq!(classify_media(&path, true).0, MediaType::Image);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_filtered_media_entries_empty_query() {
        let mut state = AppState::new(SettingsStore::default());
        state.media_grid.entries = vec![
            MediaEntry {
                path: "/a.jpg".into(),
                media_type: MediaType::Image,
                file_name: "a.jpg".into(),
                animated: None,
            },
            MediaEntry {
                path: "/b.png".into(),
                media_type: MediaType::Image,
                file_name: "b.png".into(),
                animated: None,
            },
        ];
        state.media_grid.search.query = String::new();
        let results = state.media_grid.filtered_entries();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_filtered_media_entries_with_query() {
        let mut state = AppState::new(SettingsStore::default());
        state.media_grid.entries = vec![
            MediaEntry {
                path: "/sunset.jpg".into(),
                media_type: MediaType::Image,
                file_name: "sunset.jpg".into(),
                animated: None,
            },
            MediaEntry {
                path: "/mountain.png".into(),
                media_type: MediaType::Image,
                file_name: "mountain.png".into(),
                animated: None,
            },
        ];
        state.media_grid.search.query = "sun".into();
        let results = state.media_grid.filtered_entries();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_name, "sunset.jpg");
    }

    #[test]
    fn test_filtered_media_entries_case_insensitive() {
        let mut state = AppState::new(SettingsStore::default());
        state.media_grid.entries = vec![MediaEntry {
            path: "/SUNSET.jpg".into(),
            media_type: MediaType::Image,
            file_name: "SUNSET.jpg".into(),
            animated: None,
        }];
        state.media_grid.search.query = "sun".into();
        let results = state.media_grid.filtered_entries();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_filtered_media_entries_no_match() {
        let mut state = AppState::new(SettingsStore::default());
        state.media_grid.entries = vec![MediaEntry {
            path: "/test.jpg".into(),
            media_type: MediaType::Image,
            file_name: "test.jpg".into(),
            animated: None,
        }];
        state.media_grid.search.query = "nonexistent".into();
        let results = state.media_grid.filtered_entries();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_app_state_new() {
        let state = AppState::new(SettingsStore::default());
        assert!(state.media_grid.entries.is_empty());
        assert!(state.media_grid.search.query.is_empty());
        assert!(state.media_grid.selected_index.is_none());
        assert!(!state.should_exit);
        assert!(matches!(state.settings_ui, SettingsUiState::Hidden));
        assert!(!state.metadata.panel_expanded);
        assert!(matches!(state.settings_ui, SettingsUiState::Hidden));
        assert_eq!(state.history.done_len(), 0);
    }

    #[test]
    fn test_pin_current_folder() {
        let mut state = AppState::new(SettingsStore::default());
        let folder = PathBuf::from("/test/folder");
        state.folder.current_folder = Some(folder.clone());
        state.pin_current_folder();
        assert_eq!(state.folder.pinned_folders.len(), 1);
        assert_eq!(state.folder.pinned_folders[0].path, folder);
    }

    #[test]
    fn test_unpin_folder() {
        let mut state = AppState::new(SettingsStore::default());
        let folder = PathBuf::from("/test/folder");
        state.folder.current_folder = Some(folder.clone());
        state.pin_current_folder();
        state.unpin_folder(&folder);
        assert!(state.folder.pinned_folders.is_empty());
    }

    #[test]
    fn test_pin_current_folder_no_duplicate() {
        let mut state = AppState::new(SettingsStore::default());
        let folder = std::path::PathBuf::from("/test/folder");
        state.folder.current_folder = Some(folder.clone());
        state.pin_current_folder();
        assert_eq!(state.folder.pinned_folders.len(), 1);
        state.pin_current_folder();
        assert_eq!(state.folder.pinned_folders.len(), 1);
    }

    #[test]
    fn test_pin_current_folder_syncs_settings() {
        let mut state = AppState::new(SettingsStore::default());
        let folder = std::path::PathBuf::from("/test/folder");
        state.folder.current_folder = Some(folder.clone());
        state.pin_current_folder();
        assert_eq!(state.settings.pinned_folders.paths.len(), 1);
        assert_eq!(state.settings.pinned_folders.paths[0], "/test/folder");
    }

    #[test]
    fn test_pin_current_folder_no_current() {
        let mut state = AppState::new(SettingsStore::default());
        state.folder.current_folder = None;
        state.pin_current_folder();
        assert!(state.folder.pinned_folders.is_empty());
    }

    #[test]
    fn test_folder_tree_navigation() {
        let mut state = AppState::new(SettingsStore::default());

        let p_root1 = PathBuf::from("/root1");
        let p_sub1 = PathBuf::from("/root1/sub1");
        let p_sub2 = PathBuf::from("/root1/sub2");
        let p_root2 = PathBuf::from("/root2");

        let node_root1 = FolderNode {
            path: p_root1.clone(),
            name: "root1".to_string(),
            children: vec![
                FolderNode {
                    path: p_sub1.clone(),
                    name: "sub1".to_string(),
                    children: Vec::new(),
                    is_current: false,
                    is_expanded: false,
                    is_parent_nav: false,
                    ..FolderNode::default()
                },
                FolderNode {
                    path: p_sub2.clone(),
                    name: "sub2".to_string(),
                    children: Vec::new(),
                    is_current: false,
                    is_expanded: false,
                    is_parent_nav: false,
                    ..FolderNode::default()
                },
            ],
            is_current: false,
            is_expanded: false,
            is_parent_nav: false,
            ..FolderNode::default()
        };

        let node_root2 = FolderNode {
            path: p_root2.clone(),
            name: "root2".to_string(),
            children: Vec::new(),
            is_current: false,
            is_expanded: false,
            is_parent_nav: false,
            ..FolderNode::default()
        };

        state.folder.folder_tree = vec![node_root1, node_root2];

        state.folder.select_below();
        assert_eq!(state.folder.selected_folder, Some(p_root1.clone()));

        state.folder.select_below();
        assert_eq!(state.folder.selected_folder, Some(p_root2.clone()));

        state.folder.select_above();
        assert_eq!(state.folder.selected_folder, Some(p_root1.clone()));

        state.folder.expand_selected();
        assert!(state.folder.folder_tree[0].is_expanded);

        state.folder.select_below();
        assert_eq!(state.folder.selected_folder, Some(p_sub1.clone()));

        state.folder.select_below();
        assert_eq!(state.folder.selected_folder, Some(p_sub2.clone()));

        state.folder.collapse_selected();
        assert_eq!(state.folder.selected_folder, Some(p_root1.clone()));

        state.folder.collapse_selected();
        assert!(!state.folder.folder_tree[0].is_expanded);
        assert_eq!(state.folder.selected_folder, Some(p_root1.clone()));
    }

    #[test]
    fn test_select_folder_navigation_after_expansion() {
        let mut state = AppState::new(SettingsStore::default());
        let p_root = PathBuf::from("/root");
        let p_sub = PathBuf::from("/root/sub");

        let node = FolderNode {
            path: p_root.clone(),
            name: "root".into(),
            children: vec![FolderNode {
                path: p_sub.clone(),
                name: "sub".into(),
                children: vec![],
                is_current: false,
                is_expanded: false,
                is_parent_nav: false,
                ..FolderNode::default()
            }],
            is_current: false,
            is_expanded: false,
            is_parent_nav: false,
            ..FolderNode::default()
        };
        state.folder.folder_tree = vec![node];

        state.folder.set_selected(p_root.clone(), 0);
        assert_eq!(state.folder.selected_folder, Some(p_root.clone()));

        state.folder.folder_tree[0].is_expanded = true;
        state.folder.invalidate_visible_folders_cache();

        state.folder.select_below();
        assert_eq!(state.folder.selected_folder, Some(p_sub));
    }

    #[test]
    fn test_collapse_already_collapsed_navigates_to_visible_parent() {
        let mut state = AppState::new(SettingsStore::default());
        let p_root = PathBuf::from("/root");
        let p_sub = PathBuf::from("/root/sub");

        let node_sub = FolderNode {
            path: p_sub.clone(),
            name: "sub".into(),
            children: vec![],
            is_current: false,
            is_expanded: false,
            is_parent_nav: false,
            ..FolderNode::default()
        };
        let node_root = FolderNode {
            path: p_root.clone(),
            name: "root".into(),
            children: vec![node_sub],
            is_current: false,
            is_expanded: true,
            is_parent_nav: false,
            ..FolderNode::default()
        };
        state.folder.folder_tree = vec![node_root];

        state.folder.set_selected(p_sub.clone(), 1);
        state.folder.collapse_selected();
        assert_eq!(
            state.folder.selected_folder,
            Some(p_root.clone()),
            "collapsing an already-collapsed child should navigate to its visible parent"
        );
        assert_eq!(state.folder.selected_folder_idx, Some(0));
    }

    #[test]
    fn test_collapse_already_collapsed_does_not_select_invisible_parent() {
        let mut state = AppState::new(SettingsStore::default());
        let p_root = PathBuf::from("/root");
        let p_mid = PathBuf::from("/root/mid");
        let p_sub = PathBuf::from("/root/mid/sub");

        let node_sub = FolderNode {
            path: p_sub.clone(),
            name: "sub".into(),
            children: vec![],
            is_current: false,
            is_expanded: false,
            is_parent_nav: false,
            ..FolderNode::default()
        };
        let node_mid = FolderNode {
            path: p_mid.clone(),
            name: "mid".into(),
            children: vec![node_sub],
            is_current: false,
            is_expanded: false,
            is_parent_nav: false,
            ..FolderNode::default()
        };
        let node_root = FolderNode {
            path: p_root.clone(),
            name: "root".into(),
            children: vec![node_mid],
            is_current: false,
            is_expanded: false,
            is_parent_nav: false,
            ..FolderNode::default()
        };
        state.folder.folder_tree = vec![node_root];

        state.folder.set_selected(p_sub.clone(), 2);
        state.folder.collapse_selected();
        assert_eq!(
            state.folder.selected_folder,
            Some(p_root.clone()),
            "collapsing an already-collapsed child whose parent is hidden \
             should fall back to the first visible item (the root)"
        );
        assert_eq!(state.folder.selected_folder_idx, Some(0));
        assert_ne!(state.folder.selected_folder, Some(p_mid.clone()));
        assert_ne!(state.folder.selected_folder, Some(p_sub.clone()));
    }

    #[test]
    fn test_expand_already_expanded_navigates_to_first_child() {
        let mut state = AppState::new(SettingsStore::default());
        let p_root = PathBuf::from("/root");
        let p_sub1 = PathBuf::from("/root/sub1");
        let p_sub2 = PathBuf::from("/root/sub2");

        let node_sub2 = FolderNode {
            path: p_sub2.clone(),
            name: "sub2".into(),
            children: vec![],
            is_current: false,
            is_expanded: false,
            is_parent_nav: false,
            ..FolderNode::default()
        };
        let node_sub1 = FolderNode {
            path: p_sub1.clone(),
            name: "sub1".into(),
            children: vec![node_sub2],
            is_current: false,
            is_expanded: false,
            is_parent_nav: false,
            ..FolderNode::default()
        };
        let node_root = FolderNode {
            path: p_root.clone(),
            name: "root".into(),
            children: vec![node_sub1],
            is_current: false,
            is_expanded: true,
            is_parent_nav: false,
            ..FolderNode::default()
        };
        state.folder.folder_tree = vec![node_root];

        state.folder.set_selected(p_root.clone(), 0);
        state.folder.expand_selected();
        assert_eq!(
            state.folder.selected_folder,
            Some(p_sub1.clone()),
            "expanding an already-expanded folder should navigate to its first child"
        );
        assert_eq!(state.folder.selected_folder_idx, Some(1));
    }

    #[test]
    fn test_keyboard_navigation_with_duplicate_paths() {
        let mut state = AppState::new(SettingsStore::default());
        let path_a = PathBuf::from("/duplicate_path");
        let path_b = PathBuf::from("/other_path");

        state.folder.folder_tree = vec![
            FolderNode {
                path: path_a.clone(),
                name: "Instance 1".into(),
                children: vec![],
                is_current: false,
                is_expanded: false,
                is_parent_nav: false,
                ..FolderNode::default()
            },
            FolderNode {
                path: path_b.clone(),
                name: "Other".into(),
                children: vec![],
                is_current: false,
                is_expanded: false,
                is_parent_nav: false,
                ..FolderNode::default()
            },
            FolderNode {
                path: path_a.clone(),
                name: "Instance 2".into(),
                children: vec![],
                is_current: false,
                is_expanded: false,
                is_parent_nav: false,
                ..FolderNode::default()
            },
        ];

        state.folder.selected_folder = Some(path_a.clone());
        state.folder.selected_folder_idx = Some(2);

        state.folder.select_above();

        assert_eq!(state.folder.selected_folder_idx, Some(1));
        assert_eq!(state.folder.selected_folder, Some(path_b));
    }

    #[test]
    fn test_pinned_folder_reordering_tree_integrity() {
        let mut state = AppState::new(SettingsStore::default());
        state.folder.current_folder = Some(PathBuf::from("/current"));

        state.folder.pinned_folders = vec![
            PinnedFolder {
                path: PathBuf::from("/pinned1"),
                name: "p1".into(),
            },
            PinnedFolder {
                path: PathBuf::from("/pinned2"),
                name: "p2".into(),
            },
        ];

        state.build_folder_tree();
        assert_eq!(state.folder.folder_tree.len(), 3);

        state.folder.pinned_folders.swap(0, 1);
        state.build_folder_tree();

        assert_eq!(state.folder.folder_tree.len(), 3);
        assert_eq!(state.folder.folder_tree[1].path, PathBuf::from("/pinned2"));
        assert_eq!(state.folder.folder_tree[2].path, PathBuf::from("/pinned1"));
    }

    #[test]
    fn test_swap_pinned_folders() {
        let mut state = AppState::new(SettingsStore::default());
        state.folder.current_folder = Some(PathBuf::from("/current"));
        state.folder.pinned_folders = vec![
            PinnedFolder {
                path: PathBuf::from("/pinned1"),
                name: "p1".into(),
            },
            PinnedFolder {
                path: PathBuf::from("/pinned2"),
                name: "p2".into(),
            },
        ];
        state.build_folder_tree();
        assert_eq!(state.folder.folder_tree.len(), 3);
        assert_eq!(state.folder.folder_tree[1].path, PathBuf::from("/pinned1"));
        assert_eq!(state.folder.folder_tree[2].path, PathBuf::from("/pinned2"));

        state.swap_pinned_folders(0, 1);

        assert_eq!(
            state.folder.pinned_folders[0].path,
            PathBuf::from("/pinned2")
        );
        assert_eq!(
            state.folder.pinned_folders[1].path,
            PathBuf::from("/pinned1")
        );
        assert_eq!(state.folder.folder_tree[1].path, PathBuf::from("/pinned2"));
        assert_eq!(state.folder.folder_tree[2].path, PathBuf::from("/pinned1"));
    }

    #[test]
    fn test_build_folder_tree_preserves_parent_nav_expansion() {
        let mut state = AppState::new(SettingsStore::default());
        let root = PathBuf::from("/a/b/c");
        state.folder.current_folder = Some(root);

        let parent_nav_path = PathBuf::from("/a/b");
        state.folder.folder_tree = vec![FolderNode {
            path: PathBuf::from("/a/b/c"),
            name: "c".into(),
            children: vec![FolderNode {
                path: parent_nav_path.clone(),
                name: "b".into(),
                children: vec![],
                is_current: false,
                is_expanded: true,
                is_parent_nav: true,
                ..FolderNode::default()
            }],
            is_current: true,
            is_expanded: true,
            is_parent_nav: false,
            ..FolderNode::default()
        }];

        state.build_folder_tree();

        let children = &state.folder.folder_tree[0].children;
        let b_node = children.iter().find(|c| c.path == parent_nav_path).unwrap();
        assert!(
            b_node.is_expanded,
            "Rebuilding the folder tree collapsed an expanded parent navigation node!"
        );
    }

    #[test]
    fn test_pin_selected_folder_updates_index_alignment() {
        let mut state = AppState::new(SettingsStore::default());
        let root = PathBuf::from("/workspace");
        state.folder.current_folder = Some(root.clone());

        let target_pin = PathBuf::from("/target_pin");
        state.folder.folder_tree = vec![
            FolderNode {
                path: root,
                name: "workspace".into(),
                children: vec![],
                is_current: true,
                is_expanded: true,
                is_parent_nav: false,
                ..FolderNode::default()
            },
            FolderNode {
                path: target_pin.clone(),
                name: "target_pin".into(),
                children: vec![],
                is_current: false,
                is_expanded: false,
                is_parent_nav: false,
                ..FolderNode::default()
            },
        ];

        state.folder.set_selected(target_pin.clone(), 1);
        state.pin_folder(&target_pin);

        assert_eq!(state.folder.pinned_folders.len(), 1);
        assert_eq!(state.folder.selected_folder, Some(target_pin));
        assert!(
            state.folder.selected_folder_idx.is_some(),
            "Pin selection action decoupled layout tracking index references!"
        );
    }

    #[test]
    fn test_classify_media_animate_gifs_false() {
        assert_eq!(
            classify_media(Path::new("test.gif"), false).0,
            MediaType::Image
        );
    }

    #[test]
    fn test_classify_media_empty_extension() {
        assert_eq!(classify_media(Path::new("test."), true).0, MediaType::Image);
    }

    #[test]
    fn test_select_folder_above_at_zero() {
        let mut state = AppState::new(SettingsStore::default());
        let p_a = PathBuf::from("/a");
        let p_b = PathBuf::from("/b");
        state.folder.folder_tree = vec![
            FolderNode {
                path: p_a.clone(),
                name: "a".into(),
                children: vec![],
                is_current: false,
                is_expanded: false,
                is_parent_nav: false,
                ..FolderNode::default()
            },
            FolderNode {
                path: p_b.clone(),
                name: "b".into(),
                children: vec![],
                is_current: false,
                is_expanded: false,
                is_parent_nav: false,
                ..FolderNode::default()
            },
        ];
        state.folder.set_selected(p_a.clone(), 0);
        assert_eq!(state.folder.selected_folder_idx, Some(0));
        state.folder.select_above();
        assert_eq!(state.folder.selected_folder_idx, Some(0));
    }

    #[test]
    fn test_select_folder_below_at_end() {
        let mut state = AppState::new(SettingsStore::default());
        let p_a = PathBuf::from("/a");
        let p_b = PathBuf::from("/b");
        state.folder.folder_tree = vec![
            FolderNode {
                path: p_a.clone(),
                name: "a".into(),
                children: vec![],
                is_current: false,
                is_expanded: false,
                is_parent_nav: false,
                ..FolderNode::default()
            },
            FolderNode {
                path: p_b.clone(),
                name: "b".into(),
                children: vec![],
                is_current: false,
                is_expanded: false,
                is_parent_nav: false,
                ..FolderNode::default()
            },
        ];
        state.folder.set_selected(p_b.clone(), 1);
        assert_eq!(state.folder.selected_folder_idx, Some(1));
        state.folder.select_below();
        assert_eq!(state.folder.selected_folder_idx, Some(1));
    }
}
