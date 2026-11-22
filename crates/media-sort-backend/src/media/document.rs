use std::cmp::Ordering;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

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

/// Cache key identifying a specific version of a canonicalized file on disk.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CanonicalFileKey {
    pub(crate) path: PathBuf,
    pub(crate) file_len: u64,
    pub(crate) modified_nanos: u64,
}

impl CanonicalFileKey {
    pub(crate) fn from_canonical_and_meta(path: PathBuf, meta: &std::fs::Metadata) -> Self {
        let modified_nanos = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        Self {
            path,
            file_len: meta.len(),
            modified_nanos,
        }
    }
}

/// Returns `true` when the raw path starts with `-` and could be misinterpreted as a CLI option.
pub(crate) fn path_starts_with_dash(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().first() == Some(&b'-')
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str().encode_wide().next() == Some(u16::from(b'-'))
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.to_str().is_some_and(|s| s.starts_with('-'))
    }
}

/// Scans `PATH` for the first matching regular file among `names`.
pub(crate) fn find_executable_on_path(names: &[&str]) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Creates a `Command` that suppresses console window creation on Windows.
pub(crate) fn new_hidden_command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub(crate) struct CappedCommandOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stdout_truncated: bool,
}

/// Spawns `cmd` with `Stdio::null()` stdin, capped stdout/stderr capture threads, and a hard
/// `timeout` (killing and reaping the child process if exceeded).
pub(crate) fn run_command_capped(
    cmd: &mut Command,
    timeout: Duration,
    max_stdout_bytes: u64,
    max_stderr_bytes: u64,
    tool_label: &str,
) -> Result<CappedCommandOutput, DocumentError> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout_thread = child
        .stdout
        .take()
        .map(|r| spawn_capped_drain(r, max_stdout_bytes));
    let stderr_thread = child
        .stderr
        .take()
        .map(|r| spawn_capped_drain(r, max_stderr_bytes));

    let started = Instant::now();
    let deadline = started + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DocumentError::Message(format!(
                    "{tool_label} timed out after {}s",
                    timeout.as_secs()
                )));
            }
            None => {
                let elapsed = started.elapsed();
                let sleep_ms = if elapsed < Duration::from_millis(20) {
                    1
                } else if elapsed < Duration::from_millis(200) {
                    5
                } else {
                    15
                };
                std::thread::sleep(Duration::from_millis(sleep_ms));
            }
        }
    };

    let (stdout, stdout_truncated) = stdout_thread
        .and_then(|t| t.join().ok())
        .unwrap_or_default();
    let (stderr, _) = stderr_thread
        .and_then(|t| t.join().ok())
        .unwrap_or_default();

    Ok(CappedCommandOutput {
        status,
        stdout,
        stderr,
        stdout_truncated,
    })
}

fn spawn_capped_drain<R: Read + Send + 'static>(
    reader: R,
    max_bytes: u64,
) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut buf);
        let truncated = (buf.len() as u64) > max_bytes;
        if truncated {
            buf.truncate(usize::try_from(max_bytes).unwrap_or(usize::MAX));
        }
        (buf, truncated)
    })
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

    #[test]
    fn detects_leading_dash_paths() {
        assert!(path_starts_with_dash(Path::new("-flag.docx")));
        assert!(!path_starts_with_dash(Path::new("/tmp/-flag.docx")));
    }
}
