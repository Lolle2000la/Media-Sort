use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use media_sort_core::media_type::DocumentKind;

use super::DecodedImage;

pub mod comic;
pub mod epub;
pub mod lok_worker;
pub mod office;
pub mod os_preview;
pub mod pdf;
pub mod poppler_pipe;
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
/// When `enable_libreoffice` is `true`, Office/spreadsheet formats without an embedded or OS
/// thumbnail may fall back to headless LibreOffice conversion. Returns `Ok(None)` cleanly when no
/// thumbnail is available (e.g. plain text, spreadsheets without previews, or encrypted documents).
pub fn generate_document_thumbnail_with_options(
    path: &Path,
    max_width: u32,
    max_height: u32,
    enable_libreoffice: bool,
) -> Result<Option<DecodedImage>, DocumentError> {
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Pdf) => pdf::render_pdf_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Comic) => comic::extract_comic_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Epub) => epub::extract_epub_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Office) | Some(DocumentKind::Spreadsheet) => {
            office::extract_office_thumbnail(path, max_width, max_height, enable_libreoffice)
        }
        Some(DocumentKind::Markdown) | Some(DocumentKind::PlainText) | None => Ok(None),
    }
}

/// Generates a bounded thumbnail for `path` using default converter options
/// (`enable_libreoffice = true`).
///
/// Returns `Ok(None)` cleanly when no thumbnail is available (e.g. plain text, spreadsheets without
/// previews, or encrypted documents).
pub fn generate_document_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    generate_document_thumbnail_with_options(path, max_width, max_height, true)
}

/// Generates a fast bounded thumbnail for `path` without invoking LibreOffice.
///
/// For Office and spreadsheet formats this checks embedded archive thumbnails and the OS preview
/// provider only; LibreOffice conversion is skipped so slow conversions can be scheduled separately
/// via [`render_libreoffice_document_thumbnail`].
pub fn extract_fast_document_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Pdf) => pdf::render_pdf_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Comic) => comic::extract_comic_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Epub) => epub::extract_epub_thumbnail(path, max_width, max_height),
        Some(DocumentKind::Office) | Some(DocumentKind::Spreadsheet) => {
            office::extract_fast_office_thumbnail(path, max_width, max_height)
        }
        Some(DocumentKind::Markdown) | Some(DocumentKind::PlainText) | None => Ok(None),
    }
}

/// Renders a bounded thumbnail for an Office or spreadsheet document via LibreOffice.
///
/// Returns `Ok(None)` for non-Office formats or when LibreOffice is unavailable or fails.
pub fn render_libreoffice_document_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    match DocumentKind::from_path(path) {
        Some(DocumentKind::Office) | Some(DocumentKind::Spreadsheet) => {
            office::render_libreoffice_thumbnail(path, max_width, max_height)
        }
        _ => Ok(None),
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

/// Reads up to `max_bytes` from `path`, failing cleanly when the file exceeds the cap.
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

/// Canonicalizes `path` and converts it to a percent-encoded `file://` URI.
pub(crate) fn path_to_file_url(path: &Path) -> Result<String, DocumentError> {
    let canon = std::fs::canonicalize(path)?;
    Ok(canonical_path_to_file_url(&canon))
}

/// Converts an already-canonicalized path to a percent-encoded `file://` URI.
pub(crate) fn canonical_path_to_file_url(canon: &Path) -> String {
    #[cfg(windows)]
    {
        let raw = canon.to_string_lossy();
        format_file_url(&raw, true)
    }
    #[cfg(not(windows))]
    {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            format_unix_file_url(canon.as_os_str().as_bytes())
        }
        #[cfg(not(unix))]
        {
            let raw = canon.to_string_lossy();
            format_file_url(&raw, false)
        }
    }
}

#[cfg(unix)]
fn format_unix_file_url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity("file://".len() + bytes.len());
    out.push_str("file://");
    percent_encode_bytes(bytes, false, &mut out);
    out
}

#[cfg(any(not(unix), test))]
fn format_file_url(raw: &str, is_windows: bool) -> String {
    let mut out = String::with_capacity("file:///".len() + raw.len());
    if is_windows {
        if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
            out.push_str("file://");
            let normalized = unc.replace('\\', "/");
            percent_encode_bytes(normalized.as_bytes(), false, &mut out);
            return out;
        }
        let stripped = raw.strip_prefix(r"\\?\").unwrap_or(raw);
        let normalized = stripped.replace('\\', "/");
        if normalized.starts_with("//") {
            out.push_str("file:");
        } else if normalized.starts_with('/') {
            out.push_str("file://");
        } else {
            out.push_str("file:///");
        }
        percent_encode_bytes(normalized.as_bytes(), true, &mut out);
    } else {
        out.push_str("file://");
        if !raw.starts_with('/') {
            out.push('/');
        }
        percent_encode_bytes(raw.as_bytes(), false, &mut out);
    }
    out
}

fn percent_encode_bytes(bytes: &[u8], allow_colon: bool, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in bytes {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~')
            || (allow_colon && b == b':');
        if keep {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[(b >> 4) as usize]));
            out.push(char::from(HEX[(b & 0x0F) as usize]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_and_windows_file_urls() {
        assert_eq!(
            format_file_url("/tmp/my dir/profile", false),
            "file:///tmp/my%20dir/profile"
        );
        assert_eq!(
            format_file_url(r"\\?\C:\Users\John Doe\Temp\profile", true),
            "file:///C:/Users/John%20Doe/Temp/profile"
        );
        assert_eq!(
            format_file_url(r"\\?\UNC\server\share\profile", true),
            "file://server/share/profile"
        );
    }
}
