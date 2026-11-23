use super::*;
use std::collections::BTreeMap;

fn seed_dirty_active_preview(state: &mut AppState) {
    let mut meta = BTreeMap::new();
    meta.insert("File".to_string(), BTreeMap::new());
    state.metadata.current = Some(meta);
    state.cache.selected_image = Some((
        PathBuf::from("/dirty.jpg"),
        iced::widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]),
    ));
    state.selected_file_size = Some(42);
    state.video.select(Some(PathBuf::from("/dirty_video.mp4")));
    state.audio.playing = true;
    state.audio.position = 12.5;
    state.audio.duration = 60.0;
    state.audio.volume = 73.0;
    state.audio.muted = true;
    state.audio.selected_cover = Some(iced::widget::image::Handle::from_rgba(
        1,
        1,
        vec![255, 0, 0, 255],
    ));
}

fn assert_active_preview_cleared(state: &AppState) {
    assert!(state.metadata.current.is_none());
    assert!(state.cache.selected_image.is_none());
    assert!(state.selected_file_size.is_none());
    assert!(state.video.selected_path().is_none());
    assert!(!state.audio.playing);
    assert_eq!(state.audio.position, 0.0);
    assert_eq!(state.audio.duration, 0.0);
    assert!(state.audio.selected_cover.is_none());
    // Volume and mute preferences survive preview resets.
    assert_eq!(state.audio.volume, 73.0);
    assert!(state.audio.muted);
}

#[test]
fn test_select_entry_in_bounds() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![make_media_entry("/test/a.jpg", MediaType::Image)];
    state.media_grid.search.query = String::new();
    let _task = update(&mut state, Message::Media(MediaMessage::SelectEntry(0)));
    assert_eq!(state.media_grid.selected_index, Some(0));
    assert!(state.metadata.current.is_none());
}

#[test]
fn test_select_entry_out_of_bounds() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![
        make_media_entry("/test/a.jpg", MediaType::Image),
        make_media_entry("/test/b.jpg", MediaType::Image),
    ];
    state.media_grid.search.query = String::new();
    state.media_grid.selected_index = None;
    let _task = update(&mut state, Message::Media(MediaMessage::SelectEntry(99)));
    assert_eq!(state.media_grid.selected_index, Some(1));
}

#[test]
fn test_select_entry_filtered_empty() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![make_media_entry("/test/a.jpg", MediaType::Image)];
    state.media_grid.search.query = "nomatch".into();
    state.media_grid.selected_index = Some(0);
    let _task = update(&mut state, Message::Media(MediaMessage::SelectEntry(0)));
    assert_eq!(state.media_grid.selected_index, None);
}

#[test]
fn test_selected_file_size_populated_on_select_and_cleared_on_reset() {
    let dir = temp_media_dir("mediasort_file_size");
    let file = dir.join("sample.jpg");
    std::fs::write(&file, b"1234567890").unwrap();

    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![MediaEntry {
        path: file.clone(),
        media_type: MediaType::Image,
        file_name: "sample.jpg".into(),
        animated: None,
    }];
    let _ = update(&mut state, Message::Media(MediaMessage::SelectEntry(0)));
    assert_eq!(state.selected_file_size, Some(10));

    state.clear_active_preview();
    assert_eq!(state.selected_file_size, None);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_clear_active_preview_resets_preview_and_audio_preserving_volume_mute() {
    let mut state = AppState::new(SettingsStore::default());
    seed_dirty_active_preview(&mut state);
    state.clear_active_preview();
    assert_active_preview_cleared(&state);
}

#[test]
fn test_audio_and_preview_reset_across_open_folder_and_async_scan() {
    let dir = temp_media_dir("mediasort_reset_open_scan");
    let mut state = AppState::new(SettingsStore::default());

    seed_dirty_active_preview(&mut state);
    state.open_folder(&dir);
    assert_active_preview_cleared(&state);
    drain_async_scan(&mut state);

    seed_dirty_active_preview(&mut state);
    state.start_async_media_scan(0);
    assert_active_preview_cleared(&state);
    drain_async_scan(&mut state);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_select_and_load_entry_empty_list_clears_active_preview_and_audio() {
    let mut state = AppState::new(SettingsStore::default());
    seed_dirty_active_preview(&mut state);
    state.media_grid.selected_index = Some(0);

    let _ = super::preview::select_and_load_entry(&mut state, 0);
    assert_eq!(state.media_grid.selected_index, None);
    assert_active_preview_cleared(&state);
}

#[test]
fn test_filesystem_ancestor_gone_clears_active_preview_and_audio() {
    let mut state = AppState::new(SettingsStore::default());
    // Relative path with no existing ancestor reaches the fallback reset branch
    // (on Unix `/` always exists as an ancestor for absolute paths).
    state.folder.current_folder = Some(PathBuf::from("nonexistent_rel_a/nonexistent_rel_b"));
    seed_dirty_active_preview(&mut state);

    let _ = update(
        &mut state,
        Message::FileSystemChanged(vec![FileSystemEvent::Modified(PathBuf::from(
            "nonexistent_rel_a",
        ))]),
    );

    assert!(state.folder.current_folder.is_none());
    assert_active_preview_cleared(&state);
}

#[test]
fn test_audio_cover_loaded_ignores_stale_path_and_sets_current() {
    let mut state = AppState::new(SettingsStore::default());
    let current_path = PathBuf::from("/music/current.mp3");
    let stale_path = PathBuf::from("/music/previous.mp3");
    state.media_grid.entries = vec![MediaEntry {
        path: current_path.clone(),
        media_type: MediaType::Audio,
        file_name: "current.mp3".into(),
        animated: None,
    }];
    state.media_grid.selected_index = Some(0);

    let _ = update(
        &mut state,
        Message::Audio(AudioMessage::CoverLoaded(
            stale_path,
            Some((1, 1, vec![1, 2, 3, 255])),
        )),
    );
    assert!(state.audio.selected_cover.is_none());

    let _ = update(
        &mut state,
        Message::Audio(AudioMessage::CoverLoaded(
            current_path,
            Some((1, 1, vec![4, 5, 6, 255])),
        )),
    );
    assert!(state.audio.selected_cover.is_some());
}

#[test]
fn test_thumbnail_ready_empty_data() {
    let mut state = AppState::new(SettingsStore::default());
    let cache_size_before = state.cache.thumbnail_cache.len();

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailReady(
            PathBuf::from("/test/empty.jpg"),
            0,
            0,
            Vec::new(),
        )),
    );
    assert_eq!(state.cache.thumbnail_cache.len(), cache_size_before);
}

#[test]
fn test_thumbnail_ready_valid_data() {
    let mut state = AppState::new(SettingsStore::default());
    let path = PathBuf::from("/test/thumb.jpg");

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailReady(
            path.clone(),
            1,
            1,
            vec![255, 0, 0, 255],
        )),
    );
    assert_eq!(state.cache.thumbnail_cache.len(), 1);
    assert!(state.cache.thumbnail_cache.contains(&path));
}

#[test]
fn test_thumbnail_cancelled_leaves_entry_usable() {
    let mut state = AppState::new(SettingsStore::default());
    let path = PathBuf::from("/test/audio_without_cover.wav");

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailCancelled(path.clone())),
    );

    assert!(
        state.cache.media_errors.get_error(&path).is_none(),
        "a file with no thumbnail image must not be marked as failed/unsupported"
    );
}

#[test]
fn test_metadata_loaded_error_clears_metadata() {
    let mut state = AppState::new(SettingsStore::default());
    let mut existing = BTreeMap::new();
    let mut inner = BTreeMap::new();
    inner.insert("Width".to_string(), "1920".to_string());
    existing.insert("EXIF".to_string(), inner);
    state.metadata.current = Some(existing);

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::MetadataLoaded(Err("load failed".to_string()))),
    );
    assert!(state.metadata.current.is_none());
}

#[test]
fn test_metadata_loaded_success() {
    let mut state = AppState::new(SettingsStore::default());
    let mut metadata = BTreeMap::new();
    let mut section = BTreeMap::new();
    section.insert("Width".to_string(), "1920".to_string());
    metadata.insert("EXIF".to_string(), section);

    let _task = update(
        &mut state,
        Message::Media(MediaMessage::MetadataLoaded(Ok(metadata))),
    );
    assert!(state.metadata.current.is_some());
    let m = state.metadata.current.as_ref().unwrap();
    assert_eq!(m.get("EXIF").unwrap().get("Width").unwrap(), "1920");
}

#[test]
fn test_grid_scrolled_updates_viewport_state() {
    let mut state = AppState::new(SettingsStore::default());
    assert_eq!(state.media_grid.scroll.viewport_width, 0.0);

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::GridScrolled(
            iced::widget::scrollable::AbsoluteOffset { x: 120.0, y: 0.0 },
            400.0,
            1200.0,
        )),
    );
    assert_eq!(state.media_grid.scroll.offset_x, 120.0);
    assert_eq!(state.media_grid.scroll.viewport_width, 400.0);
    assert_eq!(state.media_grid.scroll.content_width, 1200.0);
}

#[test]
fn test_relative_position_for_scrolling() {
    assert_eq!(relative_position_for(0, 7), Some(0.0));
    assert_eq!(relative_position_for(6, 7), Some(1.0));
    assert!((relative_position_for(3, 7).unwrap() - 0.5).abs() < 1e-6);
    assert_eq!(relative_position_for(0, 0), None);
    assert_eq!(relative_position_for(0, 1), None);
    assert_eq!(relative_position_for(99, 7), Some(1.0));
}

#[test]
fn test_calculate_scroll_into_view_1d() {
    assert_eq!(
        calculate_scroll_into_view_1d(0.0, 60.0, 0.0, 0.0, 1000.0, 50.0),
        None
    );
    assert_eq!(
        calculate_scroll_into_view_1d(0.0, 60.0, 0.0, 600.0, 500.0, 50.0),
        None
    );
    assert_eq!(
        calculate_scroll_into_view_1d(200.0, 260.0, 100.0, 600.0, 2000.0, 50.0),
        None
    );
    assert_eq!(
        calculate_scroll_into_view_1d(600.0, 660.0, 100.0, 600.0, 2000.0, 50.0),
        Some(110.0)
    );
    assert_eq!(
        calculate_scroll_into_view_1d(60.0, 120.0, 200.0, 600.0, 2000.0, 50.0),
        Some(10.0)
    );
    assert_eq!(
        calculate_scroll_into_view_1d(50.0, 100.0, 800.0, 400.0, 600.0, 20.0),
        Some(30.0)
    );
}

#[test]
fn test_video_load_failed_records_error_and_frame_ready_clears() {
    let mut state = AppState::new(SettingsStore::default());
    let path = PathBuf::from("/videos/broken.mp4");

    let fail = iced_mpv::PlayerMessage::Event(iced_mpv::WorkerEvent::LoadFailed {
        path: path.clone(),
        error: "demo error".into(),
    });
    let _task = update(&mut state, Message::Video(fail));
    assert!(state.cache.media_errors.get_error(&path).is_some());

    state.video.select(Some(path.clone()));
    let progress = iced_mpv::PlayerMessage::Event(iced_mpv::WorkerEvent::PlaybackProgress {
        position: 0.0,
        duration: 10.0,
    });
    let _task = update(&mut state, Message::Video(progress));
    let ready = iced_mpv::PlayerMessage::Event(iced_mpv::WorkerEvent::FrameReady {
        path: path.clone(),
        width: 640,
        height: 360,
        rotation: iced_mpv::Rotation::R0,
        rgba: std::sync::Arc::new(vec![0u8; 640 * 360 * 4]),
    });
    let _task = update(&mut state, Message::Video(ready));
    assert!(state.cache.media_errors.get_error(&path).is_none());
}

#[test]
fn test_reopen_last_selected_media_behavior() {
    use std::sync::mpsc::channel;

    let mut state = AppState::new(SettingsStore::default());
    state.settings.general.reopen_last_opened_folder = true;
    state.settings.general.reopen_last_selected_media = true;

    let file2 = PathBuf::from("b.jpg");
    state.settings.general.last_selected_media = Some(file2.to_string_lossy().to_string());

    let (tx, rx) = channel();
    tx.send(make_media_entry("a.jpg", MediaType::Image))
        .unwrap();
    tx.send(make_media_entry("b.jpg", MediaType::Image))
        .unwrap();
    drop(tx);
    state.media_grid.scan_receiver = Some(rx);
    state.media_grid.pending_select_index = Some(0);

    let _task = poll_background_channels(&mut state);
    assert_eq!(state.media_grid.selected_index, Some(1));

    state.settings.general.reopen_last_opened_folder = false;
    state.settings.general.reopen_last_selected_media = true;
    state.media_grid.selected_index = None;
    let (tx, rx) = channel();
    tx.send(make_media_entry("a.jpg", MediaType::Image))
        .unwrap();
    tx.send(make_media_entry("b.jpg", MediaType::Image))
        .unwrap();
    drop(tx);
    state.media_grid.scan_receiver = Some(rx);
    state.media_grid.pending_select_index = Some(0);
    let _task = poll_background_channels(&mut state);
    assert_eq!(state.media_grid.selected_index, Some(0));

    state.settings.general.reopen_last_opened_folder = true;
    state.settings.general.reopen_last_selected_media = false;
    state.media_grid.selected_index = None;
    let (tx, rx) = channel();
    tx.send(make_media_entry("a.jpg", MediaType::Image))
        .unwrap();
    tx.send(make_media_entry("b.jpg", MediaType::Image))
        .unwrap();
    drop(tx);
    state.media_grid.scan_receiver = Some(rx);
    state.media_grid.pending_select_index = Some(0);
    let _task = poll_background_channels(&mut state);
    assert_eq!(state.media_grid.selected_index, Some(0));
}
