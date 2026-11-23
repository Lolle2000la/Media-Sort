use std::path::Path;

use media_sort_core::media_type::DocumentKind;

use super::DecodedImage;

pub mod text;

/// Hard upper bound on plain-text / Markdown / CSV preview reads.
pub(crate) const MAX_TEXT_PREVIEW_BYTES: u64 = 256 * 1024;
/// Maximum lines rendered for plain text previews before truncation indicator.
pub(crate) const MAX_TEXT_PREVIEW_LINES: usize = 2_000;
/// Maximum rows extracted per spreadsheet or CSV/TSV table preview.
pub(crate) const MAX_SHEET_ROWS: usize = 500;
/// Maximum columns extracted per spreadsheet or CSV/TSV table preview.
pub(crate) const MAX_SHEET_COLS: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image decode error: {0}")]
    Image(#[from] image::ImageError),
    #[error("Document error: {0}")]
    Message(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentUnavailableReason {
    Encrypted,
    BinaryOrUnreadable,
    NoPreviewBackend,
    TooLarge,
    UnsupportedOrCorrupt(String),
}

#[derive(Debug, Clone)]
pub struct SheetData {
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: usize,
    pub total_cols: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct SlideData {
    pub number: usize,
    pub title: Option<String>,
    pub body_paragraphs: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum DocumentPreview {
    PagedImage {
        image: DecodedImage,
        page_index: usize,
        total_pages: usize,
    },
    Markdown {
        markdown: String,
        chapter_index: Option<usize>,
        total_chapters: Option<usize>,
        cover: Option<DecodedImage>,
    },
    Spreadsheet {
        sheets: Vec<SheetData>,
    },
    Slides {
        slides: Vec<SlideData>,
    },
    PlainText {
        text: String,
        truncated: bool,
    },
    Unavailable {
        reason: DocumentUnavailableReason,
    },
}

/// Generates a bounded thumbnail for `path` with explicit converter options.
///
/// Returns `Ok(None)` cleanly when no thumbnail is available (e.g. plain text or Markdown).
pub fn generate_document_thumbnail_with_options(
    path: &Path,
    _max_width: u32,
    _max_height: u32,
    _enable_libreoffice: bool,
) -> Result<Option<DecodedImage>, DocumentError> {
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Markdown) | Some(DocumentKind::PlainText) | None => Ok(None),
    }
}

/// Generates a bounded thumbnail for `path` using default converter options.
///
/// Returns `Ok(None)` cleanly when no thumbnail is available (e.g. plain text or Markdown).
pub fn generate_document_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    generate_document_thumbnail_with_options(path, max_width, max_height, true)
}

/// Loads the interactive preview payload for a document at `page_index`.
pub fn load_document_preview(
    path: &Path,
    _page_index: usize,
    _enable_libreoffice: bool,
) -> Result<DocumentPreview, DocumentError> {
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Markdown) | Some(DocumentKind::PlainText) => {
            text::load_text_preview(path)
        }
        None => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::UnsupportedOrCorrupt(
                "Unsupported document extension".to_string(),
            ),
        }),
    }
}
