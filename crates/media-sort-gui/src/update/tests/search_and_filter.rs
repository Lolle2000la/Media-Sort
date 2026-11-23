use super::*;

#[test]
fn test_search_query_changed_empty_result_clears_active_preview_and_audio() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![make_media_entry("/a.mp3", MediaType::Audio)];
    state.media_grid.rebuild_lower_names();
    state.media_grid.selected_index = Some(0);
    state.selected_file_size = Some(2048);
    state.video.select(Some(PathBuf::from("/v.mp4")));
    state.audio.playing = true;
    state.audio.position = 5.0;
    state.audio.duration = 20.0;
    state.audio.selected_cover = Some(iced::widget::image::Handle::from_rgba(
        1,
        1,
        vec![255, 0, 0, 255],
    ));
    state.cache.selected_image = Some((
        PathBuf::from("/a.jpg"),
        iced::widget::image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]),
    ));

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::SearchQueryChanged("nomatch".into())),
    );

    assert_eq!(state.media_grid.selected_index, None);
    assert!(state.metadata.current.is_none());
    assert!(state.cache.selected_image.is_none());
    assert!(state.selected_file_size.is_none());
    assert!(state.video.selected_path().is_none());
    assert!(!state.audio.playing);
    assert_eq!(state.audio.position, 0.0);
    assert!(state.audio.selected_cover.is_none());
}

#[test]
fn test_search_query_changed_preserves_previously_selected_entry() {
    let mut state = AppState::new(SettingsStore::default());
    state.media_grid.entries = vec![
        make_media_entry("/apple.jpg", MediaType::Image),
        make_media_entry("/banana.jpg", MediaType::Image),
    ];
    state.media_grid.rebuild_lower_names();
    state.media_grid.selected_index = Some(1);

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::SearchQueryChanged("ban".into())),
    );

    assert_eq!(state.media_grid.selected_index, Some(0));
    let filtered = state.media_grid.filtered_entries();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].file_name, "banana.jpg");
}

#[test]
fn test_search_focus_and_blur_messages() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(!state.media_grid.search.focused);

    let _ = update(&mut state, Message::Media(MediaMessage::SearchFocused));
    assert!(state.media_grid.search.focused);

    let _ = update(&mut state, Message::Media(MediaMessage::SearchBlurred));
    assert!(!state.media_grid.search.focused);
}
