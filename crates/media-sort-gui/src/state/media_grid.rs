use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc;

use media_sort_core::media_type::MediaType;
use media_sort_core::models::MediaEntry;

/// Active-session media type filter toggles for the media grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaTypeFilter {
    pub images: bool,
    pub video: bool,
    pub audio: bool,
    pub documents: bool,
}

impl Default for MediaTypeFilter {
    fn default() -> Self {
        Self {
            images: true,
            video: true,
            audio: true,
            documents: true,
        }
    }
}

impl MediaTypeFilter {
    /// Returns `true` when entries of `media_type` should be shown.
    pub fn is_enabled(&self, media_type: MediaType) -> bool {
        match media_type {
            MediaType::Image => self.images,
            MediaType::Video => self.video,
            MediaType::Audio => self.audio,
            MediaType::Document => self.documents,
        }
    }

    /// Toggles visibility of `media_type` in the active session.
    pub fn toggle(&mut self, media_type: MediaType) {
        match media_type {
            MediaType::Image => self.images = !self.images,
            MediaType::Video => self.video = !self.video,
            MediaType::Audio => self.audio = !self.audio,
            MediaType::Document => self.documents = !self.documents,
        }
    }

    /// Returns `true` when every media type filter is enabled.
    pub fn is_all_enabled(&self) -> bool {
        self.images && self.video && self.audio && self.documents
    }

    /// Enables all four media type filters.
    pub fn reset_all(&mut self) {
        self.images = true;
        self.video = true;
        self.audio = true;
        self.documents = true;
    }
}

/// Keyboard-driven Filter Mode state for toggling media type filter chips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilterModeState {
    pub active: bool,
    pub focused_chip: usize,
}

impl FilterModeState {
    pub const CHIP_COUNT: usize = 4;
    pub const FILTER_ORDER: [MediaType; Self::CHIP_COUNT] = [
        MediaType::Image,
        MediaType::Video,
        MediaType::Audio,
        MediaType::Document,
    ];

    /// Enters Filter Mode and focuses the first filter chip.
    pub fn enter(&mut self) {
        self.active = true;
        self.focused_chip = 0;
    }

    /// Exits Filter Mode and resets chip focus.
    pub fn exit(&mut self) {
        self.active = false;
        self.focused_chip = 0;
    }

    /// Moves chip focus by `delta`, wrapping around `CHIP_COUNT`.
    pub fn step(&mut self, delta: isize) {
        let len = Self::CHIP_COUNT as isize;
        let current = (self.focused_chip % Self::CHIP_COUNT) as isize;
        self.focused_chip = (current + delta).rem_euclid(len) as usize;
    }

    /// Returns the [`MediaType`] corresponding to the currently focused chip.
    pub fn focused_media_type(self) -> MediaType {
        Self::FILTER_ORDER[self.focused_chip % Self::CHIP_COUNT]
    }
}

/// Snapshot of the media grid's scrollable viewport. Updated whenever the
/// scrollable reports a new viewport via its `on_scroll` callback.
///
/// Kept in sync for diagnostic / debug purposes only; auto-scroll now uses
/// relative positions so it doesn't depend on this snapshot being current.
#[derive(Debug, Clone, Copy, Default)]
pub struct MediaGridScrollState {
    /// Current horizontal scroll offset in pixels.
    pub offset_x: f32,
    /// Width of the visible viewport in pixels.
    pub viewport_width: f32,
    /// Width of the scrollable content in pixels.
    pub content_width: f32,
}

#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    pub focused: bool,
    pub placeholder: String,
}

#[derive(Default)]
pub struct MediaGridState {
    pub entries: Vec<MediaEntry>,
    /// Precomputed lowercase `file_name`s mirrored from [`entries`]. MUST be
    /// kept in sync via [`rebuild_lower_names`](Self::rebuild_lower_names)
    /// after ANY direct mutation of `entries` (push / clear / drain / swap /
    /// extend / retain / insert / truncate / etc.). When the cache length is
    /// stale, `filtered_entries` falls back to per-call lowercasing — still
    /// correct (it iterates `entries` directly), just slower.
    pub lower_names: Vec<String>,
    pub selected_index: Option<usize>,
    pub search: SearchState,
    pub type_filter: MediaTypeFilter,
    pub filter_mode: FilterModeState,
    pub scroll: MediaGridScrollState,
    pub scan_receiver: Option<mpsc::Receiver<MediaEntry>>,
    /// Index to select after the background scan completes.
    pub pending_select_index: Option<usize>,
    /// When true, the in-flight scan REPLACES [`entries`] on completion
    /// (watcher-driven refresh) instead of extending them. Buffered entries
    /// land in [`scan_buffer`] while the scan runs.
    pub(crate) scan_replace: bool,
    /// Entries drained from the receiver while [`scan_replace`] is active.
    pub(crate) scan_buffer: Vec<MediaEntry>,
    /// A watcher event arrived while a scan was in flight; start another
    /// replace-scan once the current one finishes.
    pub(crate) pending_refresh: bool,
    /// Path of the selected entry captured when a refresh starts, so the
    /// selection can be restored by path after the replacement lands.
    pub(crate) refresh_select_path: Option<PathBuf>,
}

impl fmt::Debug for MediaGridState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaGridState")
            .field("entries_len", &self.entries.len())
            .field("selected_index", &self.selected_index)
            .field("search", &self.search)
            .field("type_filter", &self.type_filter)
            .field("filter_mode", &self.filter_mode)
            .field("scroll", &self.scroll)
            .field("scan_receiver", &self.scan_receiver.is_some())
            .field("pending_select_index", &self.pending_select_index)
            .field("scan_replace", &self.scan_replace)
            .field("pending_refresh", &self.pending_refresh)
            .finish()
    }
}

impl MediaGridState {
    /// Returns entry counts across all scanned items in `[images, video, audio, documents]` order.
    pub fn type_counts(&self) -> [usize; 4] {
        let mut counts = [0usize; 4];
        for entry in &self.entries {
            match entry.media_type {
                MediaType::Image => counts[0] += 1,
                MediaType::Video => counts[1] += 1,
                MediaType::Audio => counts[2] += 1,
                MediaType::Document => counts[3] += 1,
            }
        }
        counts
    }

    pub fn filtered_entries(&self) -> Vec<&MediaEntry> {
        let has_query = !self.search.query.is_empty();
        if !has_query && self.type_filter.is_all_enabled() {
            return self.entries.iter().collect();
        }
        if !has_query {
            return self
                .entries
                .iter()
                .filter(|e| self.type_filter.is_enabled(e.media_type))
                .collect();
        }
        let query_lower = self.search.query.to_lowercase();
        if self.lower_names.len() == self.entries.len() {
            self.entries
                .iter()
                .enumerate()
                .filter(|(i, e)| {
                    self.type_filter.is_enabled(e.media_type)
                        && self.lower_names[*i].contains(&query_lower)
                })
                .map(|(_, e)| e)
                .collect()
        } else {
            self.entries
                .iter()
                .filter(|e| {
                    self.type_filter.is_enabled(e.media_type)
                        && e.file_name.to_lowercase().contains(&query_lower)
                })
                .collect()
        }
    }

    /// Recompute [`lower_names`](Self::lower_names) from [`entries`]. Call
    /// this after every direct mutation of `entries` so `filtered_entries`
    /// can use the pre-lowercased cache.
    pub fn rebuild_lower_names(&mut self) {
        self.lower_names = self
            .entries
            .iter()
            .map(|e| e.file_name.to_lowercase())
            .collect();
    }
}
