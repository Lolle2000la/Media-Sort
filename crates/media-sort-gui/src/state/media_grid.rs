use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc;

use media_sort_core::models::MediaEntry;

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
            .field("scroll", &self.scroll)
            .field("scan_receiver", &self.scan_receiver.is_some())
            .field("pending_select_index", &self.pending_select_index)
            .field("scan_replace", &self.scan_replace)
            .field("pending_refresh", &self.pending_refresh)
            .finish()
    }
}

impl MediaGridState {
    pub fn filtered_entries(&self) -> Vec<&MediaEntry> {
        if self.search.query.is_empty() {
            return self.entries.iter().collect();
        }
        let query_lower = self.search.query.to_lowercase();
        if self.lower_names.len() == self.entries.len() {
            self.entries
                .iter()
                .enumerate()
                .filter(|(i, _)| self.lower_names[*i].contains(&query_lower))
                .map(|(_, e)| e)
                .collect()
        } else {
            self.entries
                .iter()
                .filter(|e| e.file_name.to_lowercase().contains(&query_lower))
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
