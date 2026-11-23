use std::path::{Path, PathBuf};

use iced::Task;
use media_sort_backend::media::document::DocumentPreview;

use crate::message::{DocumentMessage, Message};
use crate::state::AppState;
use crate::state::document::DocumentPreviewData;

/// Handle document preview loading, page/sheet navigation, and link clicks.
pub fn handle_document_message(state: &mut AppState, msg: DocumentMessage) -> Task<Message> {
    match msg {
        DocumentMessage::Loaded {
            path,
            page_index,
            result,
        } => {
            state
                .document
                .in_flight_pages
                .remove(&(path.clone(), page_index));
            if state.document.current_path.as_ref() != Some(&path) {
                return Task::none();
            }
            if page_index != state.document.page_index {
                if let Ok(preview) = result {
                    let actual_page = actual_page_from_preview(&preview, page_index);
                    let data = DocumentPreviewData::from_backend(&preview);
                    state.cache.document_cache.push((path, actual_page), data);
                }
                return Task::none();
            }

            state.document.loading = false;
            match result {
                Ok(preview) => {
                    let actual_page = actual_page_from_preview(&preview, page_index);
                    let data = DocumentPreviewData::from_backend(&preview);
                    let total_pages = document_total_pages(&data);
                    state
                        .cache
                        .document_cache
                        .push((path.clone(), actual_page), data.clone());
                    state.document.page_index = actual_page;
                    state.document.data = Some(data);
                    if let Some(total) = total_pages
                        && total > 1
                    {
                        return prefetch_adjacent_document_pages(state, &path, actual_page, total);
                    }
                }
                Err(err) => {
                    tracing::error!("Failed to load document preview: {err}");
                    state.document.data = Some(DocumentPreviewData::Error(err));
                }
            }
            Task::none()
        }
        DocumentMessage::PrevPage => navigate_document_page(state, false),
        DocumentMessage::NextPage => navigate_document_page(state, true),
        DocumentMessage::SelectSheet(idx) => {
            if let Some(DocumentPreviewData::Spreadsheet {
                sheets,
                active_sheet,
            }) = &mut state.document.data
                && idx < sheets.len()
            {
                *active_sheet = idx;
            }
            Task::none()
        }
        DocumentMessage::LinkClicked(uri) => {
            #[cfg(not(test))]
            if let Err(e) = media_sort_backend::platform::open_url(&uri) {
                tracing::warn!("Ignored document link: {e}");
            }
            #[cfg(test)]
            if !media_sort_backend::platform::is_safe_http_url(&uri) {
                tracing::warn!("Ignored non-http(s) document link");
            }
            Task::none()
        }
    }
}

/// Select `path` for document preview, serving from `document_cache` on hit
/// or scheduling an off-thread preview load on miss.
pub fn select(state: &mut AppState, path: &Path) -> Task<Message> {
    state.cache.selected_image = None;
    let page_index = if state.document.current_path.as_deref() == Some(path) {
        state.document.page_index
    } else {
        0
    };
    let path_buf = path.to_path_buf();
    state.document.current_path = Some(path_buf.clone());
    state.document.page_index = page_index;

    if let Some(cached) = state
        .cache
        .document_cache
        .get(&(path_buf.clone(), page_index))
        .cloned()
    {
        let total_pages = document_total_pages(&cached);
        state.document.loading = false;
        state.document.data = Some(cached);
        if let Some(total) = total_pages
            && total > 1
        {
            prefetch_adjacent_document_pages(state, &path_buf, page_index, total)
        } else {
            Task::none()
        }
    } else {
        state.document.loading = true;
        state.document.data = None;
        if state
            .document
            .in_flight_pages
            .insert((path_buf.clone(), page_index))
        {
            load_document_preview_task(path_buf, page_index)
        } else {
            Task::none()
        }
    }
}

/// Reset the active document preview state.
pub fn reset(state: &mut AppState) {
    state.document.reset();
}

fn actual_page_from_preview(preview: &DocumentPreview, fallback_page: usize) -> usize {
    match preview {
        DocumentPreview::PagedImage { page_index, .. } => *page_index,
        DocumentPreview::Markdown {
            chapter_index: Some(chapter),
            ..
        } => *chapter,
        _ => fallback_page,
    }
}

fn step_index(current: usize, len: usize, forward: bool) -> usize {
    if forward {
        (current + 1).min(len - 1)
    } else {
        current.saturating_sub(1)
    }
}

pub fn navigate_document_page(state: &mut AppState, forward: bool) -> Task<Message> {
    match &mut state.document.data {
        Some(DocumentPreviewData::Slides {
            slides,
            slide_index,
        }) => {
            if !slides.is_empty() {
                *slide_index = step_index(*slide_index, slides.len(), forward);
                state.document.page_index = *slide_index;
            }
            return Task::none();
        }
        Some(DocumentPreviewData::Spreadsheet {
            sheets,
            active_sheet,
        }) => {
            if !sheets.is_empty() {
                *active_sheet = step_index(*active_sheet, sheets.len(), forward);
            }
            return Task::none();
        }
        _ => {}
    }

    let Some(path) = state.document.current_path.clone() else {
        return Task::none();
    };
    let Some(total) = state.document.data.as_ref().and_then(document_total_pages) else {
        return Task::none();
    };
    if total == 0 {
        return Task::none();
    }
    let new_page = step_index(state.document.page_index, total, forward);
    if new_page == state.document.page_index {
        return Task::none();
    }

    state.document.page_index = new_page;
    if let Some(cached) = state
        .cache
        .document_cache
        .get(&(path.clone(), new_page))
        .cloned()
    {
        state.document.loading = false;
        state.document.data = Some(cached);
        prefetch_adjacent_document_pages(state, &path, new_page, total)
    } else {
        state.document.loading = true;
        if state
            .document
            .in_flight_pages
            .insert((path.clone(), new_page))
        {
            load_document_preview_task(path, new_page)
        } else {
            Task::none()
        }
    }
}

/// Returns the total page or chapter count for paged document previews (`PagedImage` or
/// `Markdown` with explicit chapter metadata).
pub(crate) fn document_total_pages(data: &DocumentPreviewData) -> Option<usize> {
    match data {
        DocumentPreviewData::PagedImage { total_pages, .. } => Some(*total_pages),
        DocumentPreviewData::Markdown {
            total_chapters: Some(total),
            ..
        } => Some(*total),
        _ => None,
    }
}

/// Prefetches the next and previous pages adjacent to `current_page` when they are neither
/// already cached nor already in flight.
pub fn prefetch_adjacent_document_pages(
    state: &mut AppState,
    path: &Path,
    current_page: usize,
    total_pages: usize,
) -> Task<Message> {
    if total_pages <= 1 {
        return Task::none();
    }
    let mut candidates = Vec::with_capacity(2);
    if let Some(next) = current_page.checked_add(1)
        && next < total_pages
    {
        candidates.push(next);
    }
    if let Some(prev) = current_page.checked_sub(1)
        && prev < total_pages
    {
        candidates.push(prev);
    }

    let mut tasks = Vec::with_capacity(candidates.len());
    for page in candidates {
        let key = (path.to_path_buf(), page);
        if !state.cache.document_cache.contains(&key)
            && state.document.in_flight_pages.insert(key.clone())
        {
            tasks.push(load_document_preview_task(key.0, page));
        }
    }
    Task::batch(tasks)
}

pub fn load_document_preview_task(path: PathBuf, page_index: usize) -> Task<Message> {
    Task::perform(
        async move {
            let path_clone = path.clone();
            let result = tokio::task::spawn_blocking(move || {
                media_sort_backend::media::document::load_document_preview(
                    &path_clone,
                    page_index,
                    false,
                )
                .map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(format!("Join error: {e}")));
            (path, page_index, result)
        },
        |(path, page_index, result)| {
            Message::Document(DocumentMessage::Loaded {
                path,
                page_index,
                result,
            })
        },
    )
}
