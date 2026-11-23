pub mod audio;
pub mod drag_drop;
pub mod file_ops;
pub mod filesystem;
pub mod folder;
pub mod image;
pub mod keyboard;
pub mod media;
pub mod metadata;
pub mod preview;
pub mod scroll;
pub mod settings;
pub mod thumbnails;
pub mod video;

#[cfg(test)]
mod tests;

#[cfg(not(feature = "demo"))]
use std::time::Duration;
use std::time::Instant;

use iced::Task;

use crate::message::Message;
use crate::state::AppState;

#[cfg(feature = "demo")]
use iced_automation::AutomationStateTrait;

pub fn update(state: &mut AppState, message: Message) -> Task<Message> {
    #[cfg(feature = "demo")]
    if let Some(task) =
        iced_automation::handle_automation_message(state, &message, Message::AutomationBounds)
    {
        return task;
    }

    match message {
        Message::Tick(instant) => handle_tick(state, instant),
        Message::Video(video_msg) => video::handle_video_message(state, video_msg),
        Message::Folder(folder_msg) => folder::handle_folder_message(state, folder_msg),
        Message::Media(media_msg) => media::handle_media_message(state, media_msg),
        Message::Audio(audio_msg) => audio::handle_audio_message(state, audio_msg),
        Message::Settings(settings_msg) => settings::handle_settings_message(state, settings_msg),
        Message::DragDrop(drag_drop_msg) => {
            drag_drop::handle_drag_drop_message(state, drag_drop_msg)
        }
        Message::FileSystemChanged(events) => filesystem::handle_filesystem_events(state, events),
        Message::SystemThemeChanged(mode) => {
            state.system_theme = mode;
            Task::none()
        }
        Message::KeyCaptured(key, ctrl, shift, alt) => {
            keyboard::handle_key_captured(state, key, ctrl, shift, alt)
        }
        Message::SettingsLoaded(result) => settings::handle_settings_loaded(state, result),
        Message::MediaScanCompleted(result) => media::handle_media_scan_completed(state, result),
        Message::Quit => {
            let _ = state.settings.save();
            state.should_exit = true;
            state.video.deactivate();
            Task::none()
        }
        Message::EventOccurred(event) => handle_event_occurred(state, event),
        Message::OpenCredits => {
            state.show_credits = true;
            Task::none()
        }
        Message::CloseCredits => {
            state.show_credits = false;
            Task::none()
        }
        Message::OpenUrl(url) => {
            #[cfg(not(test))]
            if let Err(e) = media_sort_backend::platform::open_url(&url) {
                tracing::error!("Failed to open URL: {e}");
            }
            #[cfg(test)]
            let _ = url;
            Task::none()
        }
        Message::NoOp => Task::none(),
        #[cfg(feature = "velopack")]
        Message::Update(update_msg) => handle_update_message(state, update_msg),
        #[cfg(feature = "demo")]
        Message::AutomationBounds(_) | Message::AutomationVirtualTick(_) => Task::none(),
    }
}

fn handle_tick(state: &mut AppState, instant: Instant) -> Task<Message> {
    if state.should_exit {
        let _ = state.settings.save();
        state.video.deactivate();
        return iced::window::latest().and_then(iced::window::close);
    }

    // Expire the transient status banner. `expires_at` was stamped by
    // `set_status` with the same monotonic clock the tick stream uses, so a
    // suspended process can't keep a stale banner alive: the comparison is
    // against the tick's own `Instant`, not a wall-clock read at set time.
    if let Some(ref status) = state.status_message
        && instant >= status.expires_at
    {
        state.status_message = None;
    }

    #[cfg(feature = "demo")]
    let automation_task =
        iced_automation::try_tick_state(state, instant, update, Message::AutomationBounds);
    #[cfg(not(feature = "demo"))]
    let automation_task = Task::none();

    let bg_task = poll_background_channels(state);
    let refresh_thumbnails = state.cache.thumbnail_tracker.tick();

    if state.drag_drop.hovering
        && let Some(pos) = state.drag_drop.last_cursor_position
    {
        let win_w = state.settings.window_position.width as f32;
        let win_h = state.settings.window_position.height as f32;
        state.drag_drop.update_cursor(pos, (win_w, win_h));
    }

    if let Some(ref player) = state.audio.player
        && state.audio.playing
    {
        if player.empty() {
            state.audio.playing = false;
            state.audio.position = 0.0;
        } else {
            state.audio.position = player.position();
            state.audio.duration = player.duration();
        }
    }

    // Poll the config file for external changes once per second (a store
    // without a custom path — tests, fallbacks — has no file to watch).
    // Reload happens BEFORE the dirty flush so an unflushed local change
    // is preserved by the reload's merge and persisted by the save below.
    // Excluded from demo builds: parallel headless renders must not adopt
    // the real user config mid-render.
    #[cfg(not(feature = "demo"))]
    if state.settings.custom_path.is_some() && instant >= state.settings_reload_at {
        state.settings_reload_at = instant + Duration::from_secs(1);
        match state.settings.reload_from_disk() {
            Ok(true) => settings::apply_external_settings_changes(state),
            Ok(false) => {}
            Err(e) => tracing::debug!("External settings reload failed: {e}"),
        }
    }

    let _ = state.settings.save_if_dirty();

    if refresh_thumbnails {
        Task::batch(vec![
            thumbnails::load_visible_thumbnails(state),
            bg_task,
            automation_task,
        ])
    } else {
        Task::batch(vec![bg_task, automation_task])
    }
}

pub fn poll_background_channels(state: &mut AppState) -> Task<Message> {
    let mut bg_tasks = Vec::new();

    if let Some(ref rx) = state.folder.folder_tree_receiver
        && let Ok(tree) = rx.try_recv()
    {
        state.folder.folder_tree_receiver = None;
        state.folder.folder_tree = tree;
        state.folder.invalidate_visible_folders_cache();
        state.folder.sync_selected_idx();
    }
    // Watcher events arrived while the tree was rebuilding: the finished
    // rebuild is already stale, start another one (it re-reads the
    // filesystem, so the result is current).
    if state.folder.folder_tree_receiver.is_none() && state.folder.tree_refresh_pending {
        state.folder.tree_refresh_pending = false;
        state.start_async_folder_tree();
    }

    let (new_entries, scan_finished) = if let Some(ref rx) = state.media_grid.scan_receiver {
        let mut new_entries: Vec<media_sort_core::models::MediaEntry> = rx.try_iter().collect();
        // Drain any item that arrived between try_iter() and this loop; only
        // treat Disconnected (not an Ok'd item) as "scan finished".
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(entry) => new_entries.push(entry),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        (new_entries, disconnected)
    } else {
        (Vec::new(), false)
    };
    if state.media_grid.scan_replace {
        // Watcher-driven refresh: buffer the drained entries; the entries
        // list is swapped wholesale when the scan finishes.
        state.media_grid.scan_buffer.extend(new_entries);
    } else if !new_entries.is_empty() {
        state.media_grid.entries.extend(new_entries);
        state.media_grid.rebuild_lower_names();
    }

    if scan_finished {
        state.media_grid.scan_receiver = None;
        if state.media_grid.scan_replace {
            state.media_grid.scan_replace = false;
            let mut entries = std::mem::take(&mut state.media_grid.scan_buffer);
            entries.sort_by(|a, b| a.file_name.cmp(&b.file_name));
            state.media_grid.entries = entries;
            state.media_grid.rebuild_lower_names();
            state.cache.thumbnail_tracker.cancel_debounce();
            bg_tasks.extend(apply_refresh_selection(state));
            bg_tasks.push(thumbnails::load_visible_thumbnails(state));
        } else {
            state
                .media_grid
                .entries
                .sort_by(|a, b| a.file_name.cmp(&b.file_name));
            let mut select_idx = state.media_grid.pending_select_index.take().unwrap_or(0);
            if state.settings.general.reopen_last_opened_folder
                && state.settings.general.reopen_last_selected_media
                && let Some(ref last_media_path_str) = state.settings.general.last_selected_media
            {
                let last_media_path = std::path::PathBuf::from(last_media_path_str);
                if let Some(pos) = state
                    .media_grid
                    .entries
                    .iter()
                    .position(|entry| entry.path == last_media_path)
                {
                    select_idx = pos;
                }
            }
            state.cache.thumbnail_tracker.cancel_debounce();
            bg_tasks.push(thumbnails::load_visible_thumbnails(state));
            bg_tasks.push(preview::select_and_load_entry(state, select_idx));
        }
        // Watcher events arrived while the scan ran: the finished result
        // is already stale, start a replace-mode refresh immediately (it
        // reads the filesystem again, so the result is current).
        if state.media_grid.pending_refresh {
            state.media_grid.pending_refresh = false;
            if let Some(folder) = state.folder.current_folder.clone() {
                state.begin_refresh_scan(&folder);
            }
        }
    }

    Task::batch(bg_tasks)
}

/// Restores the selection after a replace-mode refresh landed. The selected
/// entry is re-located by path (sorted order may have moved it); if it
/// vanished externally, the old position (clamped) is loaded instead. When
/// the same file is still selected, no preview/metadata reload happens.
/// If the user had no selection, none is made — an external change must
/// not silently load entry 0.
fn apply_refresh_selection(state: &mut AppState) -> Vec<Task<Message>> {
    let mut tasks = Vec::new();
    let prev_path = state.media_grid.refresh_select_path.take();
    let filtered = state.media_grid.filtered_entries();
    let new_idx = prev_path.and_then(|p| filtered.iter().position(|e| e.path == p));
    drop(filtered);
    match new_idx {
        Some(idx) => {
            state.media_grid.selected_index = Some(idx);
        }
        None => {
            if let Some(fallback) = state.media_grid.selected_index {
                tasks.push(preview::select_and_load_entry(state, fallback));
            }
        }
    }
    tasks
}

#[cfg(feature = "demo")]
impl iced_automation::AutomationContext<Message> for AppState {
    fn poll_background(&mut self) -> Task<Message> {
        poll_background_channels(self)
    }
}

fn handle_event_occurred(state: &mut AppState, event: iced::Event) -> Task<Message> {
    match event {
        iced::Event::Window(window_event) => handle_window_event(state, window_event),
        iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
            let win_w = state.settings.window_position.width as f32;
            let win_h = state.settings.window_position.height as f32;
            state.drag_drop.update_cursor(position, (win_w, win_h));

            if state.folder.dragging_folder_divider {
                let new_width = position.x.round().clamp(100.0, 800.0) as u16;
                state.settings.general.folder_tree_width = new_width;
            }
            if state.metadata.dragging_divider {
                let window_width = state.settings.window_position.width as f32;
                let raw_width = window_width - position.x;
                let new_width = raw_width.round().clamp(100.0, 800.0) as u16;
                state.settings.metadata_panel.panel_width = new_width;
            }
            Task::none()
        }
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
            if state.folder.dragging_folder_divider || state.metadata.dragging_divider {
                state.folder.dragging_folder_divider = false;
                state.metadata.dragging_divider = false;
                state.settings.mark_dirty();
            }
            if state.folder.dragging_pinned_folder.is_some() {
                state.folder.dragging_pinned_folder = None;
                state.settings.mark_dirty();
            }
            Task::none()
        }
        _ => Task::none(),
    }
}

fn handle_window_event(state: &mut AppState, event: iced::window::Event) -> Task<Message> {
    use crate::message::DragDropMessage;
    match event {
        iced::window::Event::FileHovered(path) => {
            drag_drop::handle_drag_drop_message(state, DragDropMessage::FileHovered(path))
        }
        iced::window::Event::FilesHoveredLeft => {
            drag_drop::handle_drag_drop_message(state, DragDropMessage::FileHoveredCancelled)
        }
        iced::window::Event::FileDropped(path) => {
            drag_drop::handle_drag_drop_message(state, DragDropMessage::FileDropped(path))
        }
        iced::window::Event::CloseRequested => {
            let _ = state.settings.save();
            state.video.deactivate();
            iced::window::latest().and_then(iced::window::close)
        }
        iced::window::Event::Resized(size) => {
            state.settings.window_position.width = size.width.round() as u32;
            state.settings.window_position.height = size.height.round() as u32;
            #[cfg(feature = "demo")]
            if let Some(automation) = state.automation_mut() {
                automation.update_window_size(size.width, size.height);
            }
            state.cache.thumbnail_tracker.handle_scroll();
            Task::none()
        }
        iced::window::Event::Moved(point) => {
            state.settings.window_position.left = point.x.round() as i32;
            state.settings.window_position.top = point.y.round() as i32;
            Task::none()
        }
        _ => Task::none(),
    }
}

#[cfg(feature = "velopack")]
fn handle_update_message(
    state: &mut AppState,
    msg: crate::message::UpdateMessage,
) -> Task<Message> {
    use crate::message::UpdateMessage;
    match msg {
        UpdateMessage::CheckForUpdates => {
            let settings = state.settings.general.clone();
            Task::perform(
                async move { crate::updater::check_for_update_async(&settings).await },
                |result| match result {
                    Ok(Some(info)) => {
                        Message::Update(UpdateMessage::UpdateAvailable(Box::new(info)))
                    }
                    Ok(None) => Message::Update(UpdateMessage::NoUpdateFound),
                    Err(e) => Message::Update(UpdateMessage::UpdateFailed(e)),
                },
            )
        }
        UpdateMessage::UpdateAvailable(info) => {
            state.show_update_prompt = true;
            state.pending_update = Some(*info);
            Task::none()
        }
        UpdateMessage::NoUpdateFound => {
            tracing::info!("Update check completed, no update available");
            Task::none()
        }
        UpdateMessage::UserConfirmedUpdate(info) => {
            state.show_update_prompt = false;
            state.pending_update = None;
            let allow_prerelease = state.settings.general.install_prerelease_builds
                || env!("CARGO_PKG_VERSION").contains('-');
            Task::perform(
                crate::updater::download_and_apply_async(*info, allow_prerelease),
                |result| match result {
                    Ok(()) => Message::Update(UpdateMessage::UpdateFailed(
                        "Update applied, restarting...".into(),
                    )),
                    Err(e) => Message::Update(UpdateMessage::UpdateFailed(e)),
                },
            )
        }
        UpdateMessage::UpdateFailed(e) => {
            tracing::error!("Update failed: {e}");
            // Surface the failure through the status banner. The banner
            // text stays short and localized; the raw error is appended at
            // debug level only, never shown in the UI.
            tracing::debug!(details = %e, "update failure details");
            state.set_status(state.l10n.get("status-update-failed", &[]));
            Task::none()
        }
        UpdateMessage::DismissUpdatePrompt => {
            state.show_update_prompt = false;
            state.pending_update = None;
            Task::none()
        }
    }
}
