use std::path::PathBuf;

use iced::widget::{image, markdown};
use media_sort_backend::media::document::{
    DocumentPreview, DocumentUnavailableReason, SheetData, SlideData,
};

/// UI-ready preview payload derived from a backend [`DocumentPreview`].
#[derive(Debug, Clone)]
pub enum DocumentPreviewData {
    PagedImage {
        handle: image::Handle,
        #[allow(dead_code)]
        width: u32,
        #[allow(dead_code)]
        height: u32,
        page_index: usize,
        total_pages: usize,
    },
    Markdown {
        items: Vec<markdown::Item>,
        chapter_index: Option<usize>,
        total_chapters: Option<usize>,
        cover: Option<image::Handle>,
    },
    Spreadsheet {
        sheets: Vec<SheetData>,
        active_sheet: usize,
    },
    Slides {
        slides: Vec<SlideData>,
        slide_index: usize,
    },
    PlainText {
        text: String,
        truncated: bool,
    },
    Unavailable(DocumentUnavailableReason),
    Error(String),
}

impl DocumentPreviewData {
    /// Converts a cached or freshly decoded backend [`DocumentPreview`] into UI state,
    /// parsing any Markdown body into `iced::widget::markdown::Item`s.
    pub fn from_backend(preview: &DocumentPreview) -> Self {
        match preview {
            DocumentPreview::PagedImage {
                image,
                page_index,
                total_pages,
            } => Self::PagedImage {
                handle: image::Handle::from_rgba(image.width, image.height, image.rgba.clone()),
                width: image.width,
                height: image.height,
                page_index: *page_index,
                total_pages: *total_pages,
            },
            DocumentPreview::Markdown {
                markdown: raw,
                chapter_index,
                total_chapters,
                cover,
            } => Self::Markdown {
                items: markdown::parse(raw).collect(),
                chapter_index: *chapter_index,
                total_chapters: *total_chapters,
                cover: cover
                    .as_ref()
                    .map(|img| image::Handle::from_rgba(img.width, img.height, img.rgba.clone())),
            },
            DocumentPreview::Spreadsheet { sheets } => Self::Spreadsheet {
                sheets: sheets.clone(),
                active_sheet: 0,
            },
            DocumentPreview::Slides { slides } => Self::Slides {
                slides: slides.clone(),
                slide_index: 0,
            },
            DocumentPreview::PlainText { text, truncated } => Self::PlainText {
                text: text.clone(),
                truncated: *truncated,
            },
            DocumentPreview::Unavailable { reason } => Self::Unavailable(reason.clone()),
        }
    }
}

/// Active document preview state for the currently selected document entry.
#[derive(Debug, Clone, Default)]
pub struct DocumentPreviewState {
    pub current_path: Option<PathBuf>,
    pub page_index: usize,
    pub loading: bool,
    pub data: Option<DocumentPreviewData>,
}

impl DocumentPreviewState {
    /// Clears the active document path, page index, loading flag, and preview payload.
    pub fn reset(&mut self) {
        self.current_path = None;
        self.page_index = 0;
        self.loading = false;
        self.data = None;
    }
}
