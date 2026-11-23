use super::*;

#[test]
fn test_toggle_type_filter_preserves_selected_path_and_falls_back() {
    let mut state = AppState::new(SettingsStore::default());
    let img = PathBuf::from("/test/a.jpg");
    let doc = PathBuf::from("/test/b.txt");
    state.media_grid.entries = vec![
        MediaEntry {
            path: img.clone(),
            media_type: MediaType::Image,
            file_name: "a.jpg".into(),
            animated: None,
        },
        MediaEntry {
            path: doc.clone(),
            media_type: MediaType::Document,
            file_name: "b.txt".into(),
            animated: None,
        },
    ];
    state.media_grid.rebuild_lower_names();

    let _ = update(&mut state, Message::Media(MediaMessage::SelectEntry(1)));
    assert_eq!(state.media_grid.selected_index, Some(1));
    assert_eq!(state.document.current_path.as_ref(), Some(&doc));

    // Hiding images shifts the selected document from index 1 -> 0 without losing it.
    let _ = update(
        &mut state,
        Message::Media(MediaMessage::ToggleTypeFilter(MediaType::Image)),
    );
    assert!(!state.media_grid.type_filter.images);
    assert_eq!(state.media_grid.selected_index, Some(0));
    assert_eq!(state.document.current_path.as_ref(), Some(&doc));

    // Hiding documents leaves zero entries and clears selection/document preview.
    let _ = update(
        &mut state,
        Message::Media(MediaMessage::ToggleTypeFilter(MediaType::Document)),
    );
    assert!(state.media_grid.filtered_entries().is_empty());
    assert_eq!(state.media_grid.selected_index, None);
    assert!(state.document.current_path.is_none());

    // Re-enabling images selects index 0 (a.jpg) cleanly.
    let _ = update(
        &mut state,
        Message::Media(MediaMessage::ToggleTypeFilter(MediaType::Image)),
    );
    assert_eq!(state.media_grid.selected_index, Some(0));
    assert_eq!(state.media_grid.filtered_entries()[0].path, img);
}

#[test]
fn test_document_loaded_ignores_stale_and_caches_valid() {
    use media_sort_backend::media::document::DocumentPreview;

    let mut state = AppState::new(SettingsStore::default());
    let doc = PathBuf::from("/test/readme.txt");
    let other = PathBuf::from("/test/other.txt");
    state.document.current_path = Some(doc.clone());
    state.document.page_index = 1;
    state.document.loading = true;

    // Stale path
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::Loaded {
            path: other.clone(),
            page_index: 1,
            result: Ok(DocumentPreview::PlainText {
                text: "wrong".into(),
                truncated: false,
            }),
        }),
    );
    assert!(state.document.loading);
    assert!(state.document.data.is_none());
    assert!(!state.cache.document_cache.contains(&(other, 1)));

    // Background page for same document caches without clobbering active loading state
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::Loaded {
            path: doc.clone(),
            page_index: 0,
            result: Ok(DocumentPreview::PlainText {
                text: "prefetched page".into(),
                truncated: false,
            }),
        }),
    );
    assert!(state.document.loading);
    assert!(state.document.data.is_none());
    assert!(state.cache.document_cache.contains(&(doc.clone(), 0)));

    // Matching response updates state and populates cache
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::Loaded {
            path: doc.clone(),
            page_index: 1,
            result: Ok(DocumentPreview::PlainText {
                text: "hello world".into(),
                truncated: false,
            }),
        }),
    );
    assert!(!state.document.loading);
    assert!(matches!(
        state.document.data,
        Some(crate::state::document::DocumentPreviewData::PlainText { .. })
    ));
    assert!(state.cache.document_cache.contains(&(doc, 1)));
}

#[test]
fn test_document_prev_next_page_and_sheet_navigation() {
    use crate::state::document::DocumentPreviewData;
    use media_sort_backend::media::DecodedImage;
    use media_sort_backend::media::document::{DocumentPreview, SheetData, SlideData};

    let mut state = AppState::new(SettingsStore::default());
    let doc = PathBuf::from("/test/sample.txt");
    let page0 = DocumentPreviewData::from_backend(&DocumentPreview::PagedImage {
        image: DecodedImage::new(2, 2, vec![255; 16]),
        page_index: 0,
        total_pages: 2,
    });
    let page1 = DocumentPreviewData::from_backend(&DocumentPreview::PagedImage {
        image: DecodedImage::new(2, 2, vec![0; 16]),
        page_index: 1,
        total_pages: 2,
    });
    state
        .cache
        .document_cache
        .push((doc.clone(), 0), page0.clone());
    state.cache.document_cache.push((doc.clone(), 1), page1);
    state.document.current_path = Some(doc.clone());
    state.document.page_index = 0;
    state.document.data = Some(page0);

    let _ = update(&mut state, Message::Document(DocumentMessage::NextPage));
    assert_eq!(state.document.page_index, 1);
    assert!(!state.document.loading);
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::PagedImage {
            page_index: 1,
            total_pages: 2,
            ..
        })
    ));

    // Clamps at upper bound
    let _ = update(&mut state, Message::Document(DocumentMessage::NextPage));
    assert_eq!(state.document.page_index, 1);

    let _ = update(&mut state, Message::Document(DocumentMessage::PrevPage));
    assert_eq!(state.document.page_index, 0);

    // Clamps at 0
    let _ = update(&mut state, Message::Document(DocumentMessage::PrevPage));
    assert_eq!(state.document.page_index, 0);

    // Slides adjust slide_index in-memory
    state.document.data = Some(DocumentPreviewData::Slides {
        slides: vec![
            SlideData {
                number: 1,
                title: Some("First".into()),
                body_paragraphs: vec![],
            },
            SlideData {
                number: 2,
                title: Some("Second".into()),
                body_paragraphs: vec![],
            },
        ],
        slide_index: 0,
    });
    let _ = update(&mut state, Message::Document(DocumentMessage::NextPage));
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Slides { slide_index: 1, .. })
    ));
    let _ = update(&mut state, Message::Document(DocumentMessage::PrevPage));
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Slides { slide_index: 0, .. })
    ));

    // Spreadsheet sheet selection
    state.document.data = Some(DocumentPreviewData::Spreadsheet {
        sheets: vec![
            SheetData {
                name: "S1".into(),
                headers: vec![],
                rows: vec![],
                total_rows: 0,
                total_cols: 0,
                truncated: false,
            },
            SheetData {
                name: "S2".into(),
                headers: vec![],
                rows: vec![],
                total_rows: 0,
                total_cols: 0,
                truncated: false,
            },
        ],
        active_sheet: 0,
    });
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::SelectSheet(1)),
    );
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Spreadsheet {
            active_sheet: 1,
            ..
        })
    ));
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::SelectSheet(99)),
    );
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Spreadsheet {
            active_sheet: 1,
            ..
        })
    ));
}

#[test]
fn test_document_link_url_validation() {
    use media_sort_backend::platform::is_safe_http_url;

    assert!(is_safe_http_url("https://mediasort.app"));
    assert!(is_safe_http_url("HTTP://example.com/docs"));
    assert!(!is_safe_http_url(""));
    assert!(!is_safe_http_url("http://"));
    assert!(!is_safe_http_url("https://"));
    assert!(!is_safe_http_url("file:///etc/passwd"));
    assert!(!is_safe_http_url("javascript:alert(1)"));
    assert!(!is_safe_http_url("mailto:user@example.com"));
    assert!(!is_safe_http_url("https://example.com/\nfoo"));

    let mut state = AppState::new(SettingsStore::default());
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::LinkClicked("file:///etc/passwd".into())),
    );
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::LinkClicked("https://mediasort.app".into())),
    );
}

fn setup_document_nav_state() -> AppState {
    use crate::state::document::DocumentPreviewData;
    use media_sort_backend::media::DecodedImage;
    use media_sort_backend::media::document::DocumentPreview;

    let mut state = AppState::new(SettingsStore::default());
    let doc_path = PathBuf::from("/test/a.txt");
    state.media_grid.entries = vec![
        MediaEntry {
            path: doc_path.clone(),
            media_type: MediaType::Document,
            file_name: "a.txt".into(),
            animated: None,
        },
        MediaEntry {
            path: PathBuf::from("/test/b.jpg"),
            media_type: MediaType::Image,
            file_name: "b.jpg".into(),
            animated: None,
        },
    ];
    state.media_grid.rebuild_lower_names();
    state.media_grid.selected_index = Some(0);

    let page0 = DocumentPreviewData::from_backend(&DocumentPreview::PagedImage {
        image: DecodedImage::new(2, 2, vec![10; 16]),
        page_index: 0,
        total_pages: 3,
    });
    let page1 = DocumentPreviewData::from_backend(&DocumentPreview::PagedImage {
        image: DecodedImage::new(2, 2, vec![20; 16]),
        page_index: 1,
        total_pages: 3,
    });
    state
        .cache
        .document_cache
        .push((doc_path.clone(), 0), page0.clone());
    state
        .cache
        .document_cache
        .push((doc_path.clone(), 1), page1);
    state.document.current_path = Some(doc_path);
    state.document.page_index = 0;
    state.document.data = Some(page0);
    state
}

#[test]
fn test_keyboard_page_up_down_document_navigation() {
    let mut state = setup_document_nav_state();

    // Default PageDown / PageUp navigate document pages when a document is selected.
    let _ = handle_key_captured(&mut state, Key::PageDown, false, false, false);
    assert_eq!(state.document.page_index, 1);
    let _ = handle_key_captured(&mut state, Key::PageUp, false, false, false);
    assert_eq!(state.document.page_index, 0);

    // Ignored with modifiers, search focus, open modals/dialogs, or non-document selection.
    let _ = handle_key_captured(&mut state, Key::PageDown, true, false, false);
    assert_eq!(state.document.page_index, 0);

    state.media_grid.search.focused = true;
    let _ = handle_key_captured(&mut state, Key::PageDown, false, false, false);
    assert_eq!(state.document.page_index, 0);
    state.media_grid.search.focused = false;

    state.settings_ui = SettingsUiState::Settings;
    let _ = handle_key_captured(&mut state, Key::PageDown, false, false, false);
    assert_eq!(state.document.page_index, 0);
    state.settings_ui = SettingsUiState::Hidden;

    state.show_credits = true;
    let _ = handle_key_captured(&mut state, Key::PageDown, false, false, false);
    assert_eq!(state.document.page_index, 0);
    state.show_credits = false;

    state.media_grid.selected_index = Some(1);
    let _ = handle_key_captured(&mut state, Key::PageDown, false, false, false);
    assert_eq!(state.document.page_index, 0);
}

#[test]
fn test_keyboard_custom_document_navigation_keybindings() {
    use media_sort_core::settings::keybindings::KeyBinding;

    let mut state = setup_document_nav_state();
    state.settings.keybindings.previous_page = KeyBinding::new(Key::Character('['));
    state.settings.keybindings.next_page = KeyBinding::new(Key::Character(']'));

    // Old default keys no longer navigate once remapped.
    let _ = handle_key_captured(&mut state, Key::PageDown, false, false, false);
    assert_eq!(state.document.page_index, 0);

    // Custom keys navigate forward and back.
    let _ = handle_key_captured(&mut state, Key::Character(']'), false, false, false);
    assert_eq!(state.document.page_index, 1);
    let _ = handle_key_captured(&mut state, Key::Character('['), false, false, false);
    assert_eq!(state.document.page_index, 0);

    // Guard still prevents document navigation on non-document entries.
    state.media_grid.selected_index = Some(1);
    let _ = handle_key_captured(&mut state, Key::Character(']'), false, false, false);
    assert_eq!(state.document.page_index, 0);
}

#[test]
fn test_document_spreadsheet_sheet_navigation() {
    use crate::state::document::DocumentPreviewData;
    use media_sort_backend::media::document::SheetData;

    let make_sheet = |name: &str| SheetData {
        name: name.into(),
        headers: vec![],
        rows: vec![],
        total_rows: 0,
        total_cols: 0,
        truncated: false,
    };

    let mut state = AppState::new(SettingsStore::default());
    state.document.data = Some(DocumentPreviewData::Spreadsheet {
        sheets: vec![make_sheet("S1"), make_sheet("S2"), make_sheet("S3")],
        active_sheet: 0,
    });

    // Clamps at 0 when stepping backward from the first sheet.
    let _ = update(&mut state, Message::Document(DocumentMessage::PrevPage));
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Spreadsheet {
            active_sheet: 0,
            ..
        })
    ));

    // Steps forward through sheets and clamps at sheets.len() - 1.
    let _ = update(&mut state, Message::Document(DocumentMessage::NextPage));
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Spreadsheet {
            active_sheet: 1,
            ..
        })
    ));
    let _ = update(&mut state, Message::Document(DocumentMessage::NextPage));
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Spreadsheet {
            active_sheet: 2,
            ..
        })
    ));
    let _ = update(&mut state, Message::Document(DocumentMessage::NextPage));
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Spreadsheet {
            active_sheet: 2,
            ..
        })
    ));

    // Steps backward again.
    let _ = update(&mut state, Message::Document(DocumentMessage::PrevPage));
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Spreadsheet {
            active_sheet: 1,
            ..
        })
    ));

    // Empty sheet list is a safe no-op.
    state.document.data = Some(DocumentPreviewData::Spreadsheet {
        sheets: vec![],
        active_sheet: 0,
    });
    let _ = update(&mut state, Message::Document(DocumentMessage::NextPage));
    let _ = update(&mut state, Message::Document(DocumentMessage::PrevPage));
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::Spreadsheet {
            active_sheet: 0,
            ..
        })
    ));
}

#[test]
fn test_settings_toggle_show_documents_by_default() {
    let mut state = AppState::new(SettingsStore::default());
    assert!(state.settings.general.show_documents_by_default);

    let _ = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleShowDocumentsByDefault(false)),
    );
    assert!(!state.settings.general.show_documents_by_default);
    assert!(state.settings.dirty);

    let _ = update(
        &mut state,
        Message::Settings(SettingsMessage::ToggleShowDocumentsByDefault(true)),
    );
    assert!(state.settings.general.show_documents_by_default);
}

#[test]
fn test_document_prefetch_loaded_caches_without_clobbering_and_next_page_is_instant() {
    use crate::state::document::DocumentPreviewData;
    use media_sort_backend::media::DecodedImage;
    use media_sort_backend::media::document::DocumentPreview;

    let mut state = AppState::new(SettingsStore::default());
    let doc = PathBuf::from("/test/multi.txt");
    state.document.current_path = Some(doc.clone());
    state.document.page_index = 0;
    state.document.loading = true;
    state.document.in_flight_pages.insert((doc.clone(), 0));

    let page0 = DocumentPreview::PagedImage {
        image: DecodedImage::new(2, 2, vec![10; 16]),
        page_index: 0,
        total_pages: 3,
    };
    let page1 = DocumentPreview::PagedImage {
        image: DecodedImage::new(2, 2, vec![20; 16]),
        page_index: 1,
        total_pages: 3,
    };

    // Active page 0 finishes: populates cache, updates active view, and prefetches page 1.
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::Loaded {
            path: doc.clone(),
            page_index: 0,
            result: Ok(page0),
        }),
    );
    assert!(!state.document.loading);
    assert_eq!(state.document.page_index, 0);
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::PagedImage {
            page_index: 0,
            total_pages: 3,
            ..
        })
    ));
    assert!(!state.document.in_flight_pages.contains(&(doc.clone(), 0)));
    assert!(state.document.in_flight_pages.contains(&(doc.clone(), 1)));
    assert!(!state.document.in_flight_pages.contains(&(doc.clone(), 2)));

    // Simulate a separate active load flag to confirm background prefetch leaves loading/data untouched.
    state.document.loading = true;
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::Loaded {
            path: doc.clone(),
            page_index: 1,
            result: Ok(page1),
        }),
    );
    assert!(state.document.loading);
    assert_eq!(state.document.page_index, 0);
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::PagedImage { page_index: 0, .. })
    ));
    assert!(!state.document.in_flight_pages.contains(&(doc.clone(), 1)));
    assert!(state.cache.document_cache.contains(&(doc.clone(), 1)));

    // Background prefetch error for another page must not overwrite the active view.
    state.document.in_flight_pages.insert((doc.clone(), 2));
    let _ = update(
        &mut state,
        Message::Document(DocumentMessage::Loaded {
            path: doc.clone(),
            page_index: 2,
            result: Err("background error".into()),
        }),
    );
    assert!(!state.document.in_flight_pages.contains(&(doc.clone(), 2)));
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::PagedImage { page_index: 0, .. })
    ));

    // Pressing Next hits the prefetched cache entry instantaneously and prefetches page 2.
    let _ = update(&mut state, Message::Document(DocumentMessage::NextPage));
    assert_eq!(state.document.page_index, 1);
    assert!(!state.document.loading);
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::PagedImage {
            page_index: 1,
            total_pages: 3,
            ..
        })
    ));
    assert!(state.document.in_flight_pages.contains(&(doc, 2)));
}

#[test]
fn test_document_select_cache_hit_and_non_document_resets_in_flight() {
    use crate::state::document::DocumentPreviewData;
    use media_sort_backend::media::DecodedImage;
    use media_sort_backend::media::document::DocumentPreview;

    let mut state = AppState::new(SettingsStore::default());
    let doc = PathBuf::from("/test/book.txt");
    let img = PathBuf::from("/test/photo.jpg");
    state.media_grid.entries = vec![
        MediaEntry {
            path: doc.clone(),
            media_type: MediaType::Document,
            file_name: "book.txt".into(),
            animated: None,
        },
        MediaEntry {
            path: img,
            media_type: MediaType::Image,
            file_name: "photo.jpg".into(),
            animated: None,
        },
    ];
    state.media_grid.rebuild_lower_names();

    let page0 = DocumentPreviewData::from_backend(&DocumentPreview::PagedImage {
        image: DecodedImage::new(2, 2, vec![42; 16]),
        page_index: 0,
        total_pages: 3,
    });
    state.cache.document_cache.push((doc.clone(), 0), page0);

    // Selecting cached document resolves immediately and triggers adjacent prefetch for page 1.
    let _ = update(&mut state, Message::Media(MediaMessage::SelectEntry(0)));
    assert_eq!(state.document.current_path.as_ref(), Some(&doc));
    assert!(!state.document.loading);
    assert!(matches!(
        state.document.data,
        Some(DocumentPreviewData::PagedImage { page_index: 0, .. })
    ));
    assert!(state.document.in_flight_pages.contains(&(doc.clone(), 1)));

    // Navigating to in-flight page 1 marks loading while reusing existing in-flight entry.
    let _ = update(&mut state, Message::Document(DocumentMessage::NextPage));
    assert_eq!(state.document.page_index, 1);
    assert!(state.document.loading);
    assert!(state.document.in_flight_pages.contains(&(doc, 1)));

    // Selecting an image resets document state including in_flight_pages.
    let _ = update(&mut state, Message::Media(MediaMessage::SelectEntry(1)));
    assert!(state.document.current_path.is_none());
    assert!(state.document.in_flight_pages.is_empty());
}

#[test]
fn test_thumbnails_in_flight_dedup_on_load_visible_and_select() {
    let mut state = AppState::new(SettingsStore::default());
    let a = PathBuf::from("/test/a.jpg");
    let b = PathBuf::from("/test/b.jpg");
    let cached = PathBuf::from("/test/cached.jpg");
    let errored = PathBuf::from("/test/errored.jpg");

    state.media_grid.entries = vec![
        MediaEntry {
            path: a.clone(),
            media_type: MediaType::Image,
            file_name: "a.jpg".into(),
            animated: None,
        },
        MediaEntry {
            path: b.clone(),
            media_type: MediaType::Image,
            file_name: "b.jpg".into(),
            animated: None,
        },
        MediaEntry {
            path: cached.clone(),
            media_type: MediaType::Image,
            file_name: "cached.jpg".into(),
            animated: None,
        },
        MediaEntry {
            path: errored.clone(),
            media_type: MediaType::Image,
            file_name: "errored.jpg".into(),
            animated: None,
        },
    ];
    state.media_grid.rebuild_lower_names();

    state.cache.thumbnail_cache.push(
        cached.clone(),
        iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]),
    );
    state.cache.media_errors.record(errored.clone(), "bad file");

    let _ = super::thumbnails::load_visible_thumbnails(&mut state);
    assert_eq!(state.cache.thumbnails_in_flight.len(), 2);
    assert!(state.cache.thumbnails_in_flight.contains(&a));
    assert!(state.cache.thumbnails_in_flight.contains(&b));
    assert!(!state.cache.thumbnails_in_flight.contains(&cached));
    assert!(!state.cache.thumbnails_in_flight.contains(&errored));

    // Back-to-back calls do not duplicate already in-flight paths.
    let _ = super::thumbnails::load_visible_thumbnails(&mut state);
    assert_eq!(state.cache.thumbnails_in_flight.len(), 2);

    // `select_and_load_entry` respects `thumbnails_in_flight`, `thumbnail_cache`, and `media_errors`.
    let _ = super::preview::select_and_load_entry(&mut state, 0);
    assert_eq!(state.cache.thumbnails_in_flight.len(), 2);
    assert!(!state.cache.thumbnails_in_flight.contains(&cached));
    assert!(!state.cache.thumbnails_in_flight.contains(&errored));

    // Fresh state verifies `select_and_load_entry` inserts and deduplicates on back-to-back calls.
    let mut fresh = AppState::new(SettingsStore::default());
    fresh.media_grid.entries = vec![
        MediaEntry {
            path: a.clone(),
            media_type: MediaType::Image,
            file_name: "a.jpg".into(),
            animated: None,
        },
        MediaEntry {
            path: b.clone(),
            media_type: MediaType::Image,
            file_name: "b.jpg".into(),
            animated: None,
        },
    ];
    fresh.media_grid.rebuild_lower_names();

    let _ = super::preview::select_and_load_entry(&mut fresh, 0);
    assert_eq!(fresh.cache.thumbnails_in_flight.len(), 2);
    assert!(fresh.cache.thumbnails_in_flight.contains(&a));
    assert!(fresh.cache.thumbnails_in_flight.contains(&b));

    let _ = super::preview::select_and_load_entry(&mut fresh, 0);
    assert_eq!(fresh.cache.thumbnails_in_flight.len(), 2);
}

#[test]
fn test_thumbnails_in_flight_cleared_by_completion_messages() {
    use crate::thumbnails::ThumbnailError;

    let mut state = AppState::new(SettingsStore::default());
    let ready = PathBuf::from("/test/ready.jpg");
    let ready_empty = PathBuf::from("/test/ready_empty.jpg");
    let failed_decode = PathBuf::from("/test/failed_decode.jpg");
    let failed_transient = PathBuf::from("/test/failed_transient.jpg");
    let cancelled = PathBuf::from("/test/cancelled.wav");

    for path in [
        &ready,
        &ready_empty,
        &failed_decode,
        &failed_transient,
        &cancelled,
    ] {
        state.cache.thumbnails_in_flight.insert(path.clone());
    }

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailReady(
            ready.clone(),
            1,
            1,
            vec![255, 0, 0, 255],
        )),
    );
    assert!(!state.cache.thumbnails_in_flight.contains(&ready));
    assert!(state.cache.thumbnail_cache.contains(&ready));

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailReady(
            ready_empty.clone(),
            0,
            0,
            Vec::new(),
        )),
    );
    assert!(!state.cache.thumbnails_in_flight.contains(&ready_empty));

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailFailed(
            failed_decode.clone(),
            ThumbnailError::Decode("corrupt".into()),
        )),
    );
    assert!(!state.cache.thumbnails_in_flight.contains(&failed_decode));
    assert!(state.cache.media_errors.has_error(&failed_decode));

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailFailed(
            failed_transient.clone(),
            ThumbnailError::Transient("timeout".into()),
        )),
    );
    assert!(!state.cache.thumbnails_in_flight.contains(&failed_transient));
    assert!(!state.cache.media_errors.has_error(&failed_transient));

    let _ = update(
        &mut state,
        Message::Media(MediaMessage::ThumbnailCancelled(cancelled.clone())),
    );
    assert!(!state.cache.thumbnails_in_flight.contains(&cancelled));
    assert!(state.cache.thumbnails_in_flight.is_empty());
}

#[test]
fn test_open_folder_clears_thumbnails_in_flight() {
    let dir = temp_media_dir("mediasort_open_folder_inflight");
    let mut state = AppState::new(SettingsStore::default());
    state
        .cache
        .thumbnails_in_flight
        .insert(PathBuf::from("/old/a.jpg"));
    state
        .cache
        .thumbnails_in_flight
        .insert(PathBuf::from("/old/b.txt"));

    state.open_folder(&dir);
    assert!(state.cache.thumbnails_in_flight.is_empty());

    drain_async_scan(&mut state);
    drain_folder_tree(&mut state);
    std::fs::remove_dir_all(&dir).ok();
}
