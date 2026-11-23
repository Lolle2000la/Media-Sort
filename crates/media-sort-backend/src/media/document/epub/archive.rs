use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use zip::ZipArchive;
use zip::result::ZipError;

use super::super::{DocumentError, sanitize_archive_rel_path};

/// Failure modes produced while opening or parsing an EPUB ZIP container.
#[derive(Debug)]
pub(super) enum EpubFailure {
    Encrypted,
    NotFound(String),
    TooLarge(String),
    Corrupt(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for EpubFailure {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl EpubFailure {
    pub(super) fn into_document_error(self) -> DocumentError {
        match self {
            Self::Io(err) => DocumentError::Io(err),
            Self::Encrypted => DocumentError::Message("EPUB archive is encrypted".to_string()),
            Self::NotFound(msg) | Self::TooLarge(msg) | Self::Corrupt(msg) => {
                DocumentError::Message(msg)
            }
        }
    }
}

pub(super) fn open_epub_zip(path: &Path) -> Result<ZipArchive<BufReader<File>>, EpubFailure> {
    let file = File::open(path)?;
    ZipArchive::new(BufReader::new(file)).map_err(map_zip_error)
}

fn map_zip_error(err: ZipError) -> EpubFailure {
    match err {
        ZipError::Io(io_err) => EpubFailure::Io(io_err),
        ZipError::InvalidPassword => EpubFailure::Encrypted,
        ZipError::FileNotFound => EpubFailure::NotFound("Entry not found in EPUB".to_string()),
        ZipError::UnsupportedArchive(msg)
            if msg == ZipError::PASSWORD_REQUIRED
                || msg.to_ascii_lowercase().contains("password")
                || msg.to_ascii_lowercase().contains("encrypt") =>
        {
            EpubFailure::Encrypted
        }
        other => EpubFailure::Corrupt(format!("Failed to read EPUB archive: {other}")),
    }
}

pub(super) fn find_zip_entry_index(
    archive: &ZipArchive<BufReader<File>>,
    target: &str,
) -> Option<usize> {
    if let Some(idx) = archive.index_for_name(target) {
        return Some(idx);
    }
    let normalized_target = target.trim_start_matches("./");
    let mut ci_match = None;
    for (idx, name_res) in archive.file_names().enumerate() {
        let Ok(name) = name_res else {
            continue;
        };
        let norm = name.replace('\\', "/");
        let trimmed = norm.trim_start_matches("./");
        if trimmed == normalized_target {
            return Some(idx);
        }
        if ci_match.is_none() && trimmed.eq_ignore_ascii_case(normalized_target) {
            ci_match = Some(idx);
        }
    }
    ci_match
}

pub(super) fn read_zip_index_capped(
    archive: &mut ZipArchive<BufReader<File>>,
    idx: usize,
    entry_path: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, EpubFailure> {
    let meta = archive.by_index_data(idx).map_err(map_zip_error)?;
    if meta.encrypted() {
        return Err(EpubFailure::Encrypted);
    }
    if meta.size() > max_bytes {
        return Err(EpubFailure::TooLarge(format!(
            "EPUB entry '{entry_path}' exceeds size limit ({} > {max_bytes} bytes)",
            meta.size()
        )));
    }
    let prealloc = usize::try_from(meta.size()).unwrap_or(0).min(1024 * 1024);
    drop(meta);

    let file = archive.by_index(idx).map_err(map_zip_error)?;
    if file.encrypted() {
        return Err(EpubFailure::Encrypted);
    }
    let mut buf = Vec::with_capacity(prealloc);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > max_bytes {
        return Err(EpubFailure::TooLarge(format!(
            "EPUB entry '{entry_path}' decompressed size exceeded {max_bytes} bytes"
        )));
    }
    Ok(buf)
}

pub(super) fn read_zip_path_capped(
    archive: &mut ZipArchive<BufReader<File>>,
    entry_path: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, EpubFailure> {
    let Some(idx) = find_zip_entry_index(archive, entry_path) else {
        return Err(EpubFailure::NotFound(format!(
            "Missing EPUB entry '{entry_path}'"
        )));
    };
    read_zip_index_capped(archive, idx, entry_path, max_bytes)
}

fn percent_decode_path(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            let decoded = (hi << 4) | lo;
            if decoded == 0 {
                return None;
            }
            out.push(decoded);
            i += 3;
            continue;
        }
        if bytes[i] == 0 {
            return None;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Resolves an archive-relative `href` against `base_dir`, returning `(primary, fallback_raw)`
/// when percent-decoding produces a distinct valid path.
pub(super) fn resolve_href(base_dir: &str, href: &str) -> (Option<String>, Option<String>) {
    let Some(decoded) =
        percent_decode_path(href).and_then(|d| sanitize_archive_rel_path(base_dir, &d))
    else {
        return (None, None);
    };
    let raw = sanitize_archive_rel_path(base_dir, href).filter(|r| r != &decoded);
    (Some(decoded), raw)
}

#[cfg(test)]
mod tests {
    use zip::CompressionMethod;

    use super::super::test_support::{TempTestDir, write_epub_entries};
    use super::*;

    #[test]
    fn percent_decode_and_resolve_href_sanitize_and_decode() {
        assert_eq!(
            percent_decode_path("OEBPS/my%20chapter.xhtml").as_deref(),
            Some("OEBPS/my chapter.xhtml")
        );
        assert_eq!(percent_decode_path("bad%00path"), None);
        assert_eq!(percent_decode_path("bad\0path"), None);
        assert_eq!(
            percent_decode_path("keep%ZZpct").as_deref(),
            Some("keep%ZZpct")
        );

        assert_eq!(
            resolve_href("OEBPS", "my%20chapter.xhtml#sec1?x=2"),
            (
                Some("OEBPS/my chapter.xhtml".to_string()),
                Some("OEBPS/my%20chapter.xhtml".to_string()),
            )
        );
        assert_eq!(
            resolve_href("OEBPS/text", "../images/cover.png#frag"),
            (Some("OEBPS/images/cover.png".to_string()), None)
        );

        // Traversal and absolute paths are rejected in both raw and percent-encoded forms.
        assert_eq!(resolve_href("OEBPS", "../../etc/passwd"), (None, None));
        assert_eq!(
            resolve_href("OEBPS", "%2e%2e/%2e%2e/etc/passwd"),
            (None, None)
        );
        assert_eq!(resolve_href("", "%2e%2e%2fsecret.txt"), (None, None));
        assert_eq!(resolve_href("OEBPS", "/abs/path.xhtml"), (None, None));
        assert_eq!(resolve_href("OEBPS", "%2fabs/path.xhtml"), (None, None));
        assert_eq!(resolve_href("OEBPS", "file%00.xhtml"), (None, None));
    }

    #[test]
    fn find_zip_entry_index_normalizes_prefixes_separators_and_case() {
        let dir = TempTestDir::new("archive_find");
        let epub_path = dir.path().join("find.epub");
        write_epub_entries(
            &epub_path,
            &[
                ("./OEBPS/ch1.xhtml", b"one", CompressionMethod::Deflated),
                (
                    r"OEBPS\Images\Cover.PNG",
                    b"two",
                    CompressionMethod::Deflated,
                ),
            ],
        );

        let archive = open_epub_zip(&epub_path).expect("open zip");
        assert_eq!(find_zip_entry_index(&archive, "OEBPS/ch1.xhtml"), Some(0));
        assert_eq!(
            find_zip_entry_index(&archive, "oebps/images/cover.png"),
            Some(1)
        );
        assert_eq!(find_zip_entry_index(&archive, "OEBPS/missing.xhtml"), None);
    }

    #[test]
    fn read_zip_capped_enforces_size_limit_and_reports_not_found() {
        let dir = TempTestDir::new("archive_capped");
        let epub_path = dir.path().join("capped.epub");
        write_epub_entries(
            &epub_path,
            &[
                ("small.txt", b"hello", CompressionMethod::Deflated),
                ("big.txt", b"0123456789abcdef", CompressionMethod::Deflated),
            ],
        );

        let mut archive = open_epub_zip(&epub_path).expect("open zip");
        let small = read_zip_path_capped(&mut archive, "small.txt", 8).expect("read small");
        assert_eq!(small, b"hello");

        let too_large = read_zip_path_capped(&mut archive, "big.txt", 8);
        assert!(matches!(too_large, Err(EpubFailure::TooLarge(_))));

        let idx_big = find_zip_entry_index(&archive, "big.txt").expect("big idx");
        let idx_too_large = read_zip_index_capped(&mut archive, idx_big, "big.txt", 8);
        assert!(matches!(idx_too_large, Err(EpubFailure::TooLarge(_))));

        let missing = read_zip_path_capped(&mut archive, "nope.txt", 64);
        assert!(matches!(missing, Err(EpubFailure::NotFound(_))));
    }
}
