use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use super::{
    CanonicalFileKey, DecodedImage, DocumentError, MAX_PDF_FILE_BYTES, find_executable_on_path,
    is_sandboxed, new_hidden_command, path_starts_with_dash, run_command_capped,
};
use crate::media::image_decoder::MAX_DECODE_DIMENSION;
use crate::media::thumbnail::calculate_thumbnail_dimensions;

const POPPLER_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_PDFINFO_STDOUT_BYTES: u64 = 256 * 1024;
const MAX_PDFTOPPM_STDOUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_STDERR_BYTES: u64 = 64 * 1024;
const PDFINFO_CACHE_CAPACITY: usize = 64;
const MAX_POPPLER_SCALE_TARGET: u32 = 2048;

#[cfg(target_os = "windows")]
const PDFTOPPM_BIN: &str = "pdftoppm.exe";
#[cfg(not(target_os = "windows"))]
const PDFTOPPM_BIN: &str = "pdftoppm";

#[cfg(target_os = "windows")]
const PDFINFO_BIN: &str = "pdfinfo.exe";
#[cfg(not(target_os = "windows"))]
const PDFINFO_BIN: &str = "pdfinfo";

static PDFTOPPM_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static PDFINFO_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static PDFINFO_CACHE: OnceLock<Mutex<VecDeque<PdfInfoCacheEntry>>> = OnceLock::new();

/// Outcome of invoking host Poppler CLI tools (`pdfinfo` / `pdftoppm`).
pub(crate) enum PopplerOutcome<T> {
    Ok(T),
    Encrypted,
    Err(DocumentError),
}

/// Parsed page count and metadata fields returned by `pdfinfo`.
#[derive(Clone)]
pub(crate) struct PopplerPdfInfo {
    pub(crate) pages: usize,
    pub(crate) fields: BTreeMap<String, String>,
}

#[derive(Clone)]
enum CachedPdfInfo {
    Info(PopplerPdfInfo),
    Encrypted,
}

struct PdfInfoCacheEntry {
    key: CanonicalFileKey,
    outcome: CachedPdfInfo,
}

fn find_pdftoppm() -> Option<&'static PathBuf> {
    PDFTOPPM_PATH
        .get_or_init(|| find_executable_on_path(&[PDFTOPPM_BIN]))
        .as_ref()
}

fn find_pdfinfo() -> Option<&'static PathBuf> {
    PDFINFO_PATH
        .get_or_init(|| find_executable_on_path(&[PDFINFO_BIN]))
        .as_ref()
}

/// Returns `true` when both `pdftoppm` and `pdfinfo` are discovered on `PATH` and the process is
/// not running inside a Flatpak/Snap sandbox.
pub(crate) fn is_available() -> bool {
    !is_sandboxed() && find_pdftoppm().is_some() && find_pdfinfo().is_some()
}

fn pdfinfo_cache() -> &'static Mutex<VecDeque<PdfInfoCacheEntry>> {
    PDFINFO_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(PDFINFO_CACHE_CAPACITY)))
}

fn cache_get(key: &CanonicalFileKey) -> Option<CachedPdfInfo> {
    let mut cache = pdfinfo_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pos = cache.iter().position(|entry| &entry.key == key)?;
    let entry = cache.remove(pos)?;
    let outcome = entry.outcome.clone();
    cache.push_back(entry);
    Some(outcome)
}

fn cache_insert(key: CanonicalFileKey, outcome: CachedPdfInfo) {
    let mut cache = pdfinfo_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(pos) = cache.iter().position(|entry| entry.key == key) {
        cache.remove(pos);
    }
    while cache.len() >= PDFINFO_CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back(PdfInfoCacheEntry { key, outcome });
}

fn prepare_safe_pdf_path(path: &Path) -> Result<(PathBuf, CanonicalFileKey), DocumentError> {
    let canonical = std::fs::canonicalize(path)?;
    if path_starts_with_dash(&canonical) {
        return Err(DocumentError::Message(
            "Refusing to pass '-' prefixed path to Poppler".to_string(),
        ));
    }
    let meta = std::fs::metadata(&canonical)?;
    if meta.len() > MAX_PDF_FILE_BYTES {
        return Err(DocumentError::Message(format!(
            "PDF exceeds size limit ({} > {MAX_PDF_FILE_BYTES} bytes)",
            meta.len()
        )));
    }
    let key = CanonicalFileKey::from_canonical_and_meta(canonical.clone(), &meta);
    Ok((canonical, key))
}

fn stderr_indicates_encrypted(stderr: &[u8]) -> bool {
    let lower = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    lower.contains("incorrect password") || lower.contains("encrypted")
}

/// Queries page count and document metadata from `pdfinfo`, caching parsed results by
/// `(canonical_path, file_len, modified_nanos)` so repeated page renders avoid re-spawning `pdfinfo`.
pub(crate) fn query_pdf_info(path: &Path) -> PopplerOutcome<PopplerPdfInfo> {
    if is_sandboxed() {
        return PopplerOutcome::Err(DocumentError::Message(
            "Poppler disabled inside sandbox".to_string(),
        ));
    }
    let Some(pdfinfo) = find_pdfinfo() else {
        return PopplerOutcome::Err(DocumentError::Message("pdfinfo not found".to_string()));
    };
    let (canonical, key) = match prepare_safe_pdf_path(path) {
        Ok(pair) => pair,
        Err(err) => return PopplerOutcome::Err(err),
    };

    if let Some(cached) = cache_get(&key) {
        return match cached {
            CachedPdfInfo::Info(info) => PopplerOutcome::Ok(info),
            CachedPdfInfo::Encrypted => PopplerOutcome::Encrypted,
        };
    }

    let mut cmd = new_hidden_command(pdfinfo);
    cmd.arg("--").arg(&canonical);

    let output = match run_command_capped(
        &mut cmd,
        POPPLER_TIMEOUT,
        MAX_PDFINFO_STDOUT_BYTES,
        MAX_STDERR_BYTES,
        "pdfinfo",
    ) {
        Ok(out) => out,
        Err(err) => return PopplerOutcome::Err(err),
    };

    if output.stdout_truncated {
        return PopplerOutcome::Err(DocumentError::Message(
            "pdfinfo stdout exceeded 256 KiB cap".to_string(),
        ));
    }

    if !output.status.success() {
        if stderr_indicates_encrypted(&output.stderr) {
            cache_insert(key, CachedPdfInfo::Encrypted);
            return PopplerOutcome::Encrypted;
        }
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return PopplerOutcome::Err(DocumentError::Message(if detail.is_empty() {
            format!("pdfinfo exited with status {}", output.status)
        } else {
            format!("pdfinfo exited with status {}: {detail}", output.status)
        }));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    match parse_pdfinfo_output(&text) {
        PopplerOutcome::Ok(info) => {
            cache_insert(key, CachedPdfInfo::Info(info.clone()));
            PopplerOutcome::Ok(info)
        }
        PopplerOutcome::Encrypted => {
            cache_insert(key, CachedPdfInfo::Encrypted);
            PopplerOutcome::Encrypted
        }
        PopplerOutcome::Err(err) => PopplerOutcome::Err(err),
    }
}

/// Renders `page_index` (0-based, clamped to `total_pages - 1`) via `pdftoppm` reading binary `P6`
/// PPM directly from stdout, downscaling if needed to fit inside `max_width x max_height`.
pub(crate) fn render_pdf_page(
    path: &Path,
    page_index: usize,
    max_width: u32,
    max_height: u32,
) -> PopplerOutcome<(DecodedImage, usize)> {
    if is_sandboxed() {
        return PopplerOutcome::Err(DocumentError::Message(
            "Poppler disabled inside sandbox".to_string(),
        ));
    }
    let Some(pdftoppm) = find_pdftoppm() else {
        return PopplerOutcome::Err(DocumentError::Message("pdftoppm not found".to_string()));
    };

    let info = match query_pdf_info(path) {
        PopplerOutcome::Ok(info) => info,
        PopplerOutcome::Encrypted => return PopplerOutcome::Encrypted,
        PopplerOutcome::Err(err) => return PopplerOutcome::Err(err),
    };
    if info.pages == 0 {
        return PopplerOutcome::Err(DocumentError::Message("PDF has no pages".to_string()));
    }

    let (canonical, _) = match prepare_safe_pdf_path(path) {
        Ok(pair) => pair,
        Err(err) => return PopplerOutcome::Err(err),
    };

    let max_width = max_width.max(1);
    let max_height = max_height.max(1);
    let total_pages = info.pages;
    let clamped = page_index.min(total_pages - 1);
    let page_1 = clamped + 1;
    let scale_target = max_width.max(max_height).clamp(1, MAX_POPPLER_SCALE_TARGET);

    let mut cmd = new_hidden_command(pdftoppm);
    cmd.arg("-scale-to")
        .arg(scale_target.to_string())
        .arg("-f")
        .arg(page_1.to_string())
        .arg("-l")
        .arg(page_1.to_string())
        .arg("--")
        .arg(&canonical);

    let output = match run_command_capped(
        &mut cmd,
        POPPLER_TIMEOUT,
        MAX_PDFTOPPM_STDOUT_BYTES,
        MAX_STDERR_BYTES,
        "pdftoppm",
    ) {
        Ok(out) => out,
        Err(err) => return PopplerOutcome::Err(err),
    };

    if output.stdout_truncated {
        return PopplerOutcome::Err(DocumentError::Message(
            "pdftoppm stdout exceeded 32 MiB cap".to_string(),
        ));
    }

    if !output.status.success() {
        if stderr_indicates_encrypted(&output.stderr) {
            return PopplerOutcome::Encrypted;
        }
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return PopplerOutcome::Err(DocumentError::Message(if detail.is_empty() {
            format!("pdftoppm exited with status {}", output.status)
        } else {
            format!("pdftoppm exited with status {}: {detail}", output.status)
        }));
    }

    let decoded = match parse_ppm_p6(&output.stdout) {
        Ok(img) => img,
        Err(err) => return PopplerOutcome::Err(err),
    };

    let fitted = if decoded.width > max_width || decoded.height > max_height {
        let (dst_w, dst_h) =
            calculate_thumbnail_dimensions(decoded.width, decoded.height, max_width, max_height);
        match downscale_rgba(decoded, dst_w.max(1), dst_h.max(1)) {
            Ok(img) => img,
            Err(err) => return PopplerOutcome::Err(err),
        }
    } else {
        decoded
    };

    PopplerOutcome::Ok((fitted, total_pages))
}

fn downscale_rgba(
    img: DecodedImage,
    dst_w: u32,
    dst_h: u32,
) -> Result<DecodedImage, DocumentError> {
    if dst_w == img.width && dst_h == img.height {
        return Ok(img);
    }
    let src_img = image::RgbaImage::from_raw(img.width, img.height, img.rgba)
        .ok_or_else(|| DocumentError::Message("Invalid rendered PDF RGBA buffer".to_string()))?;
    let mut dst_img = Image::new(dst_w, dst_h, PixelType::U8x4);
    let mut resizer = Resizer::new();
    resizer
        .resize(
            &src_img,
            &mut dst_img,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .map_err(|e| DocumentError::Message(format!("Failed to resize Poppler PDF page: {e}")))?;
    Ok(DecodedImage::new(dst_w, dst_h, dst_img.into_vec()))
}

fn parse_pdfinfo_output(text: &str) -> PopplerOutcome<PopplerPdfInfo> {
    let mut pages: Option<usize> = None;
    let mut fields = BTreeMap::new();

    for line in text.lines() {
        let Some((raw_key, raw_val)) = line.split_once(':') else {
            continue;
        };
        let key = raw_key.trim();
        let val = raw_val.trim();
        match key {
            "Encrypted" => {
                if val.to_ascii_lowercase().starts_with("yes") {
                    return PopplerOutcome::Encrypted;
                }
            }
            "Pages" => {
                if let Ok(n) = val.parse::<usize>() {
                    pages = Some(n);
                    fields.insert("Pages".into(), n.to_string());
                }
            }
            "Title" | "Author" | "Subject" | "Creator" | "Producer" if !val.is_empty() => {
                fields.insert(key.into(), val.to_string());
            }
            "CreationDate" if !val.is_empty() => {
                fields.insert("Created".into(), val.to_string());
            }
            _ => {}
        }
    }

    let Some(pages) = pages else {
        return PopplerOutcome::Err(DocumentError::Message(
            "pdfinfo output missing Pages field".to_string(),
        ));
    };

    PopplerOutcome::Ok(PopplerPdfInfo { pages, fields })
}

fn is_ppm_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0B | 0x0C)
}

fn skip_ppm_ws_and_comments(bytes: &[u8], idx: &mut usize) {
    while *idx < bytes.len() {
        let b = bytes[*idx];
        if is_ppm_whitespace(b) {
            *idx += 1;
        } else if b == b'#' {
            *idx += 1;
            while *idx < bytes.len() && bytes[*idx] != b'\n' && bytes[*idx] != b'\r' {
                *idx += 1;
            }
        } else {
            break;
        }
    }
}

fn read_ppm_token<'a>(bytes: &'a [u8], idx: &mut usize) -> Option<&'a [u8]> {
    skip_ppm_ws_and_comments(bytes, idx);
    let start = *idx;
    while *idx < bytes.len() {
        let b = bytes[*idx];
        if is_ppm_whitespace(b) || b == b'#' {
            break;
        }
        *idx += 1;
    }
    (*idx > start).then(|| &bytes[start..*idx])
}

fn parse_ppm_p6(bytes: &[u8]) -> Result<DecodedImage, DocumentError> {
    let mut idx = 0usize;

    let magic = read_ppm_token(bytes, &mut idx)
        .ok_or_else(|| DocumentError::Message("Empty PPM output from pdftoppm".to_string()))?;
    if magic != b"P6" {
        return Err(DocumentError::Message(format!(
            "Unsupported PPM magic '{}'; expected P6",
            String::from_utf8_lossy(magic)
        )));
    }

    let width_tok = read_ppm_token(bytes, &mut idx)
        .ok_or_else(|| DocumentError::Message("PPM header missing width".to_string()))?;
    let height_tok = read_ppm_token(bytes, &mut idx)
        .ok_or_else(|| DocumentError::Message("PPM header missing height".to_string()))?;
    let maxval_tok = read_ppm_token(bytes, &mut idx)
        .ok_or_else(|| DocumentError::Message("PPM header missing maxval".to_string()))?;

    let width: u32 = std::str::from_utf8(width_tok)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| DocumentError::Message("Invalid PPM width".to_string()))?;
    let height: u32 = std::str::from_utf8(height_tok)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| DocumentError::Message("Invalid PPM height".to_string()))?;
    let maxval: u32 = std::str::from_utf8(maxval_tok)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| DocumentError::Message("Invalid PPM maxval".to_string()))?;

    if maxval != 255 {
        return Err(DocumentError::Message(format!(
            "Unsupported PPM maxval {maxval}; expected 255"
        )));
    }
    if width == 0 || height == 0 || width > MAX_DECODE_DIMENSION || height > MAX_DECODE_DIMENSION {
        return Err(DocumentError::Message(format!(
            "PPM dimensions {width}x{height} out of bounds"
        )));
    }

    // Consume an optional `# ...` comment directly attached to maxval, then exactly ONE
    // whitespace delimiter byte so binary pixel bytes starting with whitespace or `#` stay intact.
    if bytes.get(idx) == Some(&b'#') {
        idx += 1;
        while idx < bytes.len() && bytes[idx] != b'\n' && bytes[idx] != b'\r' {
            idx += 1;
        }
    }
    let delim = bytes
        .get(idx)
        .copied()
        .ok_or_else(|| DocumentError::Message("PPM header truncated before pixels".to_string()))?;
    if !is_ppm_whitespace(delim) {
        return Err(DocumentError::Message(
            "PPM header missing delimiter byte after maxval".to_string(),
        ));
    }
    idx += 1;

    let pixel_count = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .ok_or_else(|| DocumentError::Message("PPM pixel count overflow".to_string()))?;
    let rgb_len = pixel_count
        .checked_mul(3)
        .ok_or_else(|| DocumentError::Message("PPM RGB size overflow".to_string()))?;
    let rgba_len = pixel_count
        .checked_mul(4)
        .ok_or_else(|| DocumentError::Message("PPM RGBA size overflow".to_string()))?;

    let payload = &bytes[idx..];
    if payload.len() < rgb_len {
        return Err(DocumentError::Message(format!(
            "Truncated PPM P6 pixel payload ({} < {rgb_len} bytes)",
            payload.len()
        )));
    }
    if payload.len() != rgb_len {
        return Err(DocumentError::Message(format!(
            "Unexpected PPM P6 payload size ({} != {rgb_len} bytes)",
            payload.len()
        )));
    }

    let mut rgba = vec![255u8; rgba_len];
    for (dst, src) in rgba
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(payload.as_chunks::<3>().0.iter())
    {
        dst[..3].copy_from_slice(src);
    }

    Ok(DecodedImage::new(width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ppm_p6_basic_and_pixel_values() {
        let mut data = b"P6\n2 1\n255\n".to_vec();
        data.extend_from_slice(&[10, 20, 30, 40, 50, 60]);
        let img = parse_ppm_p6(&data).expect("valid P6 PPM");
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 1);
        assert_eq!(img.rgba, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn parse_ppm_p6_handles_comments_whitespace_and_whitespace_first_pixel() {
        // First pixel starts with 0x20 (space), 0x0A (newline), 0x23 ('#') — only ONE
        // whitespace byte after `255` may be consumed.
        let mut data = b"P6 \t\r\n# generated by test\n2 # width\n1\n# maxval next\n255\n".to_vec();
        data.extend_from_slice(&[b' ', b'\n', b'#', 1, 2, 3]);
        let img = parse_ppm_p6(&data).expect("P6 PPM with comments");
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 1);
        assert_eq!(img.rgba, vec![b' ', b'\n', b'#', 255, 1, 2, 3, 255]);
    }

    #[test]
    fn parse_ppm_p6_rejects_invalid_magic_maxval_and_truncated_payload() {
        assert!(parse_ppm_p6(b"").is_err());
        assert!(parse_ppm_p6(b"P5\n1 1\n255\n\x00").is_err());
        assert!(parse_ppm_p6(b"P3\n1 1\n255\n255 0 0\n").is_err());
        assert!(parse_ppm_p6(b"P6\n1 1\n65535\n\x00\x00\x00\x00\x00\x00").is_err());
        assert!(parse_ppm_p6(b"P6\n2 2\n255\n\xff\x00\x00").is_err());
        assert!(parse_ppm_p6(b"P6\n0 2\n255\n").is_err());
    }

    #[test]
    fn parse_pdfinfo_output_extracts_fields_and_encrypted() {
        let sample = "\
Title:           Sample Document
Subject:         Testing
Author:          Ada Lovelace
Creator:         Writer
Producer:        Poppler Test
CreationDate:    2026-01-02T12:34:56Z
Pages:           12
Encrypted:       no
";
        match parse_pdfinfo_output(sample) {
            PopplerOutcome::Ok(info) => {
                assert_eq!(info.pages, 12);
                assert_eq!(info.fields.get("Pages").map(String::as_str), Some("12"));
                assert_eq!(
                    info.fields.get("Title").map(String::as_str),
                    Some("Sample Document")
                );
                assert_eq!(
                    info.fields.get("Author").map(String::as_str),
                    Some("Ada Lovelace")
                );
                assert_eq!(
                    info.fields.get("Subject").map(String::as_str),
                    Some("Testing")
                );
                assert_eq!(
                    info.fields.get("Creator").map(String::as_str),
                    Some("Writer")
                );
                assert_eq!(
                    info.fields.get("Producer").map(String::as_str),
                    Some("Poppler Test")
                );
                assert_eq!(
                    info.fields.get("Created").map(String::as_str),
                    Some("2026-01-02T12:34:56Z")
                );
            }
            PopplerOutcome::Encrypted => panic!("expected Ok, got Encrypted"),
            PopplerOutcome::Err(e) => panic!("expected Ok, got Err: {e}"),
        }

        let encrypted = "Pages:           3\nEncrypted:       yes (print:yes copy:no)\n";
        assert!(matches!(
            parse_pdfinfo_output(encrypted),
            PopplerOutcome::Encrypted
        ));

        let missing_pages = "Title: Incomplete\nEncrypted: no\n";
        assert!(matches!(
            parse_pdfinfo_output(missing_pages),
            PopplerOutcome::Err(_)
        ));
    }
}
