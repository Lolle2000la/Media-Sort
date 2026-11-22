use std::cmp::Ordering;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use media_sort_core::media_type::DocumentKind;

use super::DecodedImage;

pub mod comic;
pub mod epub;
pub mod office;
pub mod os_preview;
pub mod pdf;
pub mod soffice_pipe;
pub mod text;

/// Hard upper bound on reading a full document (PDF / legacy archive) into memory.
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
/// Maximum slides parsed in the pure-Rust PPTX/ODP fallback preview.
pub(crate) const MAX_SLIDES: usize = 100;
/// Target preview bounding box for paged bitmap documents (PDF, comics, soffice pages).
pub const PREVIEW_TARGET_WIDTH: u32 = 1600;
pub const PREVIEW_TARGET_HEIGHT: u32 = 2200;

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

/// Generates a bounded thumbnail for `path` when the document format supports an in-memory or OS
/// thumbnail without spawning heavy external converters. Returns `Ok(None)` cleanly when no
/// thumbnail is available (e.g. plain text, spreadsheets without embedded previews, Linux Office
/// files without embedded thumbnails, or encrypted documents).
pub fn generate_document_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Pdf) => pdf::render_pdf_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Comic) => comic::extract_comic_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Epub) => epub::extract_epub_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Office) | Some(DocumentKind::Spreadsheet) => {
            office::extract_office_thumbnail(path, max_width, max_height)
        }
        Some(DocumentKind::Markdown) | Some(DocumentKind::PlainText) | None => Ok(None),
    }
}

/// Loads the interactive preview payload for a document at `page_index` (used for PDF pages,
/// comic archive pages, LibreOffice-converted Office pages, or EPUB spine chapters).
pub fn load_document_preview(
    path: &Path,
    page_index: usize,
    enable_libreoffice: bool,
) -> Result<DocumentPreview, DocumentError> {
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Pdf) => pdf::load_pdf_preview(path, page_index),
        Some(DocumentKind::Comic) => comic::load_comic_preview(path, page_index),
        Some(DocumentKind::Epub) => epub::load_epub_preview(path, page_index),
        Some(DocumentKind::Spreadsheet) => office::load_spreadsheet_preview(path),
        Some(DocumentKind::Office) => {
            office::load_office_preview(path, page_index, enable_libreoffice)
        }
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

/// Reads up to `max_bytes` from `path`, failing cleanly with an explicit size error rather than
/// reading unbounded files into memory.
pub(crate) fn read_file_capped(path: &Path, max_bytes: u64) -> Result<Vec<u8>, DocumentError> {
    let file = File::open(path)?;
    let meta_len = file.metadata()?.len();
    if meta_len > max_bytes {
        return Err(DocumentError::Message(format!(
            "File exceeds size limit ({meta_len} > {max_bytes} bytes)"
        )));
    }
    let cap = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut buf = Vec::with_capacity(usize::try_from(meta_len).unwrap_or(0).min(cap));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > max_bytes {
        return Err(DocumentError::Message(format!(
            "File exceeded size limit ({max_bytes} bytes) while reading"
        )));
    }
    Ok(buf)
}

/// Reads an archive entry from a `ZipArchive` using `Read::take` to protect against spoofed headers
/// and zip bombs.
pub(crate) fn read_zip_entry_capped<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, DocumentError> {
    let entry = archive
        .by_name(name)
        .map_err(|e| DocumentError::Message(format!("Failed to open zip entry '{name}': {e}")))?;
    read_capped_zip_file(entry, name, max_bytes)
}

fn read_capped_zip_file<R: Read>(
    entry: zip::read::ZipFile<'_, R>,
    name: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, DocumentError> {
    if entry.size() > max_bytes {
        return Err(DocumentError::Message(format!(
            "Archive entry '{name}' exceeds size limit ({} > {max_bytes} bytes)",
            entry.size()
        )));
    }
    let prealloc = usize::try_from(entry.size()).unwrap_or(0).min(1024 * 1024);
    let mut buf = Vec::with_capacity(prealloc);
    entry
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > max_bytes {
        return Err(DocumentError::Message(format!(
            "Archive entry '{name}' decompressed size exceeded {max_bytes} bytes"
        )));
    }
    Ok(buf)
}

/// Returns true when the process is running inside a strict Flatpak or Snap sandbox where host
/// binaries (`soffice`, `unrar`, etc.) should not be invoked directly.
pub(crate) fn is_sandboxed() -> bool {
    #[cfg(target_os = "linux")]
    {
        if crate::platform::desktop_portal::in_flatpak() {
            return true;
        }
        if std::env::var_os("SNAP").is_some_and(|v| !v.is_empty()) {
            return true;
        }
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
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
    let Some(ext) = file_name.rsplit('.').next() else {
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

/// Natural human sort order so `page2.jpg` comes before `page10.jpg`.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = 0usize;
    let mut bi = 0usize;
    let ab = a.as_bytes();
    let bb = b.as_bytes();

    while ai < ab.len() && bi < bb.len() {
        let ac = ab[ai];
        let bc = bb[bi];

        if ac.is_ascii_digit() && bc.is_ascii_digit() {
            let mut a_end = ai;
            while a_end < ab.len() && ab[a_end].is_ascii_digit() {
                a_end += 1;
            }
            let mut b_end = bi;
            while b_end < bb.len() && bb[b_end].is_ascii_digit() {
                b_end += 1;
            }
            let a_digits = trim_leading_zeros(&ab[ai..a_end]);
            let b_digits = trim_leading_zeros(&bb[bi..b_end]);

            match a_digits.len().cmp(&b_digits.len()) {
                Ordering::Equal => match a_digits.cmp(b_digits) {
                    Ordering::Equal => {
                        // Prefer fewer leading zeros when values tie (`2` before `02`).
                        let len_cmp = (a_end - ai).cmp(&(b_end - bi));
                        if len_cmp != Ordering::Equal {
                            return len_cmp;
                        }
                    }
                    ord => return ord,
                },
                ord => return ord,
            }
            ai = a_end;
            bi = b_end;
        } else {
            let al = ac.to_ascii_lowercase();
            let bl = bc.to_ascii_lowercase();
            match al.cmp(&bl) {
                Ordering::Equal => {
                    ai += 1;
                    bi += 1;
                }
                ord => return ord,
            }
        }
    }

    ai.cmp(&bi)
        .then_with(|| ab.len().cmp(&bb.len()))
        .then_with(|| a.cmp(b))
}

fn trim_leading_zeros(bytes: &[u8]) -> &[u8] {
    let first_non_zero = bytes.iter().position(|&b| b != b'0').unwrap_or(bytes.len());
    &bytes[first_non_zero..]
}
