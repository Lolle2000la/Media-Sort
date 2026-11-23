use std::path::Path;

use media_sort_core::media_type::DocumentKind;

use super::DecodedImage;

pub mod comic;
pub mod epub;
pub mod pdf;
pub mod text;

/// Hard upper bound on reading a full PDF document into memory.
pub(crate) const MAX_PDF_FILE_BYTES: u64 = 128 * 1024 * 1024;
/// Hard upper bound when reading an individual image or content entry out of an archive.
pub(crate) const MAX_ARCHIVE_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
/// Hard upper bound when reading XML / HTML / manifest entries out of an archive.
pub(crate) const MAX_XML_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
/// Hard upper bound on plain-text / Markdown / CSV preview reads.
pub(crate) const MAX_TEXT_PREVIEW_BYTES: u64 = 256 * 1024;
/// Maximum lines rendered for plain text previews before truncation indicator.
pub(crate) const MAX_TEXT_PREVIEW_LINES: usize = 2_000;
/// Maximum rows extracted per spreadsheet or CSV/TSV table preview.
pub(crate) const MAX_SHEET_ROWS: usize = 500;
/// Maximum columns extracted per spreadsheet or CSV/TSV table preview.
pub(crate) const MAX_SHEET_COLS: usize = 64;
/// Target preview bounding box for paged bitmap documents (PDF, comics, soffice pages).
pub const PREVIEW_TARGET_WIDTH: u32 = 1200;
pub const PREVIEW_TARGET_HEIGHT: u32 = 1600;

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
/// Returns `Ok(None)` cleanly when no thumbnail is available (e.g. plain text, Markdown, or
/// encrypted PDFs).
pub fn generate_document_thumbnail_with_options(
    path: &Path,
    max_width: u32,
    max_height: u32,
    _enable_libreoffice: bool,
) -> Result<Option<DecodedImage>, DocumentError> {
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Pdf) => pdf::render_pdf_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Comic) => comic::extract_comic_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Epub) => epub::extract_epub_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Markdown) | Some(DocumentKind::PlainText) | None => Ok(None),
    }
}

/// Generates a bounded thumbnail for `path` using default converter options.
///
/// Returns `Ok(None)` cleanly when no thumbnail is available (e.g. plain text, Markdown, or
/// encrypted PDFs).
pub fn generate_document_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    generate_document_thumbnail_with_options(path, max_width, max_height, true)
}

/// Loads the interactive preview payload for a document at `page_index` (used for PDF pages,
/// comic archive pages, or EPUB spine chapters).
pub fn load_document_preview(
    path: &Path,
    page_index: usize,
    _enable_libreoffice: bool,
) -> Result<DocumentPreview, DocumentError> {
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Pdf) => pdf::load_pdf_preview(path, page_index),
        Some(DocumentKind::Comic) => comic::load_comic_preview(path, page_index),
        Some(DocumentKind::Epub) => epub::load_epub_preview(path, page_index),
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

/// Returns true when an archive entry path looks like a comic/cover image and is not macOS
/// resource-fork clutter.
pub(crate) fn is_image_entry(name: &str) -> bool {
    let normalized = name.replace('\\', "/");
    if normalized.ends_with('/') {
        return false;
    }
    let lower = normalized.to_ascii_lowercase();
    if lower.starts_with("__macosx/") || lower.contains("/__macosx/") {
        return false;
    }
    let file_name = normalized.rsplit('/').next().unwrap_or("");
    if file_name.is_empty() || file_name.starts_with("._") || file_name.starts_with('.') {
        return false;
    }
    let Some((_, ext)) = file_name.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "webp" | "avif" | "bmp" | "gif" | "tif" | "tiff"
    )
}

/// Resolves a relative archive path (`rel`) against `base_dir` (normalized with `/`) while
/// rejecting path traversal outside the archive root.
pub(crate) fn sanitize_archive_rel_path(base_dir: &str, rel: &str) -> Option<String> {
    let clean_rel = rel.split(['#', '?']).next().unwrap_or("").trim();
    if clean_rel.is_empty() || clean_rel.starts_with('/') || clean_rel.contains('\\') {
        return None;
    }
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in clean_rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Decodes XML/XHTML bytes handling UTF-8, UTF-16 BE, and UTF-16 LE BOMs.
pub(crate) fn decode_xml_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c));
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c));
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    String::from_utf8_lossy(bytes).into_owned()
}
