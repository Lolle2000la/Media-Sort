//! Routing of filesystem watcher events into media grid and folder tree
//! refreshes.
//!
//! Two refresh pipelines are driven from here, both coalescing and both
//! non-blocking:
//!
//! - **Media grid** — events whose parent is the current folder trigger
//!   [`AppState::start_media_refresh`], a replace-mode background rescan
//!   that keeps the current entries visible until the new list lands.
//! - **Folder tree** — events that change the children of any displayed
//!   folder (a subfolder added/removed/renamed inside the current folder or
//!   an expanded node) trigger [`AppState::request_folder_tree_refresh`],
//!   a background tree rebuild that preserves expansion state.
//!
//! Directory-level `Modified` events (the OS only telling us "this
//! directory changed", not which children) trigger the same refreshes.
//! This matters most on macOS, whose kqueue backend (workspace
//! `macos_kqueue` feature) under-reports children: it reports only the
//! first not-yet-known entry per directory-write reap and never reports
//! deletions of pre-existing files, emitting a directory `Modified`
//! instead. Treating every batch as "something changed, re-scan" (full
//! replace-mode rescan, never a partial diff) makes the grid
//! self-correcting on every backend.
//!
//! Special cases: the current folder itself renamed externally is
//! followed (the app re-opens the new path) where the backend can pair
//! the rename (inotify, with the parent watched); where it cannot
//! (Windows/macOS deliver the two sides separately), the view moves up
//! to the closest existing ancestor like a plain deletion. The current
//! folder deleted externally always moves the view up to the closest
//! existing ancestor.

use std::path::{Path, PathBuf};

use iced::Task;

use crate::message::Message;
use crate::state::AppState;
use media_sort_backend::filesystem::watcher::FileSystemEvent;
use media_sort_core::path_utils::paths_equal;

pub fn handle_filesystem_events(
    state: &mut AppState,
    events: Vec<FileSystemEvent>,
) -> Task<Message> {
    let Some(current) = state.folder.current_folder.clone() else {
        return Task::none();
    };

    let watched = state.watched_directories();

    let mut tree_dirty = false;
    let mut media_dirty = false;
    let mut folder_renamed_to: Option<PathBuf> = None;

    for event in &events {
        match event {
            FileSystemEvent::Added(path) => {
                let is_dir = path.is_dir();
                // A new subfolder changes the children of its parent; if
                // that parent is displayed (watched), the tree must pick
                // it up. New files change nothing in the tree.
                if is_dir && parent_in_watched(path, &watched) {
                    tree_dirty = true;
                }
                // New files in the current folder appear in the grid; new
                // subfolders do not (the scanner lists files only).
                if !is_dir && parent_is_current(path, &current) {
                    media_dirty = true;
                }
            }
            FileSystemEvent::Removed(path) => {
                // The current folder itself was removed. If it still does
                // not exist after the loop, the view moves up to the
                // closest existing ancestor. If an external tool deleted
                // AND recreated it at the same path before the flush, the
                // folder exists again: the grid/tree must rescan the new
                // content, and the watch-generation bump forces the
                // subscription to restart so the OS watch is re-established
                // on the new directory (the old inode watch is dead).
                if paths_equal(path, &current) {
                    media_dirty = true;
                    tree_dirty = true;
                    state.bump_watch_generation();
                    continue;
                }
                // A removed path that used to be a tree node (displayed
                // folder) needs a tree rebuild; the path can no longer be
                // statted, so ask the tree itself.
                if state.tree_contains_path(path) {
                    tree_dirty = true;
                }
                if parent_is_current(path, &current) {
                    media_dirty = true;
                }
            }
            FileSystemEvent::Renamed(from, to) => {
                if paths_equal(from, &current) {
                    folder_renamed_to = Some(to.clone());
                    continue;
                }
                // A pinned folder renamed externally: keep the pin (and the
                // persisted settings) pointing at the new path. Only the
                // paired-rename backend (inotify) reaches this arm — see
                // the module doc. `p.path == *from` matches pins stored in
                // canonical form (the event's `from` is canonical); pins
                // stored in a non-canonical spelling of the OLD path can
                // never match here because the old path no longer exists
                // and `paths_equal` cannot canonicalize it. The
                // `paths_equal(&p.path, to)` arm only handles re-delivery
                // of an already-updated pin (idempotency), not the primary
                // match.
                if let Some(pos) = state
                    .folder
                    .pinned_folders
                    .iter()
                    .position(|p| p.path == *from || paths_equal(&p.path, to))
                {
                    state.folder.pinned_folders[pos].path = to.clone();
                    if let Some(name) = to.file_name() {
                        state.folder.pinned_folders[pos].name = name.to_string_lossy().to_string();
                    }
                    state.sync_pinned_folders_to_settings();
                    state.settings.mark_dirty();
                }
                // A folder rename changes the displayed children of the
                // source and/or destination parent; a FILE rename changes
                // nothing in the tree (it shows folders only), so gate the
                // tree rebuild on directory-ness. `tree_contains_path`
                // covers a displayed folder node renamed away from the
                // watched set (its destination may not be statable here).
                if state.tree_contains_path(from)
                    || (to.is_dir()
                        && (parent_in_watched(from, &watched) || parent_in_watched(to, &watched)))
                {
                    tree_dirty = true;
                }
                // Renames into or out of the current folder change the
                // grid either way.
                if parent_is_current(from, &current) || parent_is_current(to, &current) {
                    media_dirty = true;
                }
            }
            FileSystemEvent::Modified(path) => {
                // A directory-level Modified means its child set may have
                // changed without per-child events (kqueue's
                // under-reporting, watch errors, NFS). Content-only
                // modifications of FILES stay deliberately ignored.
                if !path.is_dir() {
                    continue;
                }
                if paths_equal(path, &current) {
                    // The current folder itself: its displayed children
                    // (grid + tree) may both have changed.
                    media_dirty = true;
                    tree_dirty = true;
                } else if watched.iter().any(|w| paths_equal(w, path)) {
                    // An expanded tree node: its displayed children may
                    // have changed.
                    tree_dirty = true;
                }
            }
        }
    }

    if let Some(new_path) = folder_renamed_to {
        // The current folder was renamed externally: follow it. A clean
        // re-open of the new path resets selection/history/video like any
        // other folder switch, and the watcher subscription re-keys on the
        // new current folder automatically.
        //
        // Early return: the remaining events in this batch (e.g. sibling
        // changes) are deliberately dropped — `open_folder` rebuilds the
        // tree and rescans the media grid from scratch, which subsumes
        // their effects.
        tracing::info!(
            "Current folder was renamed externally: {} -> {}",
            current.display(),
            new_path.display()
        );
        state.open_folder(&new_path);
        return Task::none();
    }

    if !current.exists() {
        // The current folder vanished without a usable rename event (e.g.
        // deleted, or moved on a backend that can't pair renames). Move up
        // to the closest existing ancestor so the user isn't left staring
        // at a dead grid.
        let mut ancestor = current.parent().map(|p| p.to_path_buf());
        while let Some(ref dir) = ancestor {
            if dir.is_dir() {
                state.open_folder(dir);
                return Task::none();
            }
            ancestor = dir.parent().map(|p| p.to_path_buf());
        }
        state.folder.current_folder = None;
        state.media_grid.entries.clear();
        state.media_grid.rebuild_lower_names();
        state.media_grid.selected_index = None;
        state.folder.folder_tree.clear();
        state.folder.invalidate_visible_folders_cache();
        state.clear_active_preview();
        return Task::none();
    }

    if tree_dirty {
        state.request_folder_tree_refresh();
    }
    if media_dirty {
        state.start_media_refresh();
    }
    Task::none()
}

/// Whether `path`'s parent is one of the watched (displayed) directories.
fn parent_in_watched(path: &Path, watched: &[PathBuf]) -> bool {
    path.parent()
        .is_some_and(|p| watched.iter().any(|w| paths_equal(w, p)))
}

/// Whether `path` sits directly inside the current folder.
fn parent_is_current(path: &Path, current: &Path) -> bool {
    path.parent().is_some_and(|p| paths_equal(p, current))
}
