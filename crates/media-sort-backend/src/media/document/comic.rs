use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use media_sort_core::natural_sort::natural_cmp;

use super::{
    DecodedImage, DocumentError, DocumentPreview, DocumentUnavailableReason, PREVIEW_TARGET_HEIGHT,
    PREVIEW_TARGET_WIDTH,
};
use crate::media::format_pipeline::decode_and_resize_bytes;

mod comic_info;
mod rar_cbr;
mod sevenz_cb7;
mod tar_cbt;
mod window_cache;
mod zip_cbz;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComicFormat {
    Cbz,
    Cb7,
    Cbt,
    Cbr,
}

impl ComicFormat {
    fn from_path(path: &Path) -> Result<Self, DocumentError> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "cbz" => Ok(Self::Cbz),
            "cb7" => Ok(Self::Cb7),
            "cbt" => Ok(Self::Cbt),
            "cbr" => Ok(Self::Cbr),
            _ => Err(DocumentError::Message(format!(
                "Unsupported comic archive extension: .{ext}"
            ))),
        }
    }
}

#[derive(Debug)]
enum ArchiveFailure {
    Encrypted,
    UnsupportedOrEmpty(String),
    TooLarge(String),
    Error(DocumentError),
}

impl From<std::io::Error> for ArchiveFailure {
    fn from(err: std::io::Error) -> Self {
        Self::Error(DocumentError::Io(err))
    }
}

impl From<DocumentError> for ArchiveFailure {
    fn from(err: DocumentError) -> Self {
        Self::Error(err)
    }
}

#[derive(Debug)]
struct PageEntry<T> {
    id: T,
    sort_name: String,
}

/// Extracts a thumbnail from page 0 of a comic archive (`cbz`, `cb7`, `cbt`, `cbr`).
///
/// Encrypted or image-less comic archives cleanly return `Ok(None)`.
pub fn extract_comic_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    match extract_comic_page(path, 0) {
        Ok((bytes, _, _)) => decode_and_resize_bytes(&bytes, max_width, max_height)
            .map(Some)
            .map_err(DocumentError::Image),
        Err(ArchiveFailure::Encrypted) | Err(ArchiveFailure::UnsupportedOrEmpty(_)) => Ok(None),
        Err(ArchiveFailure::TooLarge(msg)) => Err(DocumentError::Message(msg)),
        Err(ArchiveFailure::Error(err)) => Err(err),
    }
}

/// Loads page `page_index` (clamped to the available page range) from a comic archive
/// for interactive previewing.
pub fn load_comic_preview(
    path: &Path,
    page_index: usize,
) -> Result<DocumentPreview, DocumentError> {
    match extract_comic_page(path, page_index) {
        Ok((bytes, clamped_page, total_pages)) => {
            let image =
                decode_and_resize_bytes(&bytes, PREVIEW_TARGET_WIDTH, PREVIEW_TARGET_HEIGHT)?;
            Ok(DocumentPreview::PagedImage {
                image,
                page_index: clamped_page,
                total_pages,
            })
        }
        Err(ArchiveFailure::Encrypted) => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::Encrypted,
        }),
        Err(ArchiveFailure::TooLarge(_)) => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::TooLarge,
        }),
        Err(ArchiveFailure::UnsupportedOrEmpty(msg)) => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::UnsupportedOrCorrupt(msg),
        }),
        Err(ArchiveFailure::Error(err)) => Err(err),
    }
}

/// Extracts `"Pages"` count and optional `ComicInfo.xml` fields (`Title`, `Series`,
/// `Number`, `Writer`, `Publisher`, `Year`, `Genre`) from a comic archive.
pub fn extract_comic_metadata(path: &Path) -> Result<BTreeMap<String, String>, DocumentError> {
    let format = ComicFormat::from_path(path)?;
    let outcome = match format {
        ComicFormat::Cbz => zip_cbz::zip_metadata(path),
        ComicFormat::Cb7 => sevenz_cb7::sevenz_metadata(path),
        ComicFormat::Cbt => tar_cbt::tar_metadata(path),
        ComicFormat::Cbr => rar_cbr::rar_metadata(path),
    };

    let mut fields = BTreeMap::new();
    match outcome {
        Ok((page_count, comic_info_bytes)) => {
            fields.insert("Pages".into(), page_count.to_string());
            if let Some(xml_bytes) = comic_info_bytes {
                comic_info::parse_comic_info_xml(&xml_bytes, &mut fields);
            }
            Ok(fields)
        }
        Err(ArchiveFailure::Encrypted) => {
            fields.insert("Encrypted".into(), "Yes".into());
            Ok(fields)
        }
        Err(ArchiveFailure::UnsupportedOrEmpty(_)) => {
            fields.insert("Pages".into(), "0".into());
            Ok(fields)
        }
        Err(ArchiveFailure::TooLarge(msg)) => Err(DocumentError::Message(msg)),
        Err(ArchiveFailure::Error(err)) => Err(err),
    }
}

fn extract_comic_page(
    path: &Path,
    page_index: usize,
) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
    match ComicFormat::from_path(path)? {
        ComicFormat::Cbz => zip_cbz::zip_extract_page(path, page_index),
        ComicFormat::Cb7 => sevenz_cb7::sevenz_extract_page(path, page_index),
        ComicFormat::Cbt => tar_cbt::tar_extract_page(path, page_index),
        ComicFormat::Cbr => rar_cbr::rar_extract_page(path, page_index),
    }
}

fn empty_pages_error() -> ArchiveFailure {
    ArchiveFailure::UnsupportedOrEmpty("Comic archive contains no readable images".to_string())
}

fn sort_page_ids<T>(mut pages: Vec<PageEntry<T>>) -> Vec<T> {
    pages.sort_by(|a, b| natural_cmp(&a.sort_name, &b.sort_name));
    pages.into_iter().map(|p| p.id).collect()
}

fn normalize_archive_name(name: &str) -> String {
    name.replace('\\', "/")
}

fn is_comic_info_name(name: &str) -> bool {
    let normalized = normalize_archive_name(name);
    if normalized.ends_with('/') {
        return false;
    }
    let lower = normalized.to_ascii_lowercase();
    if lower.starts_with("__macosx/") || lower.contains("/__macosx/") {
        return false;
    }
    let basename = normalized.rsplit('/').next().unwrap_or("");
    basename.eq_ignore_ascii_case("comicinfo.xml")
}

fn prefer_comic_info_candidate<T>(current: &Option<(T, String)>, candidate: &str) -> bool {
    let Some((_, existing)) = current else {
        return true;
    };
    let existing_depth = existing.matches('/').count();
    let cand_depth = normalize_archive_name(candidate).matches('/').count();
    cand_depth < existing_depth
}

fn read_stream_capped<R: Read>(
    reader: R,
    declared_size: Option<u64>,
    name: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, ArchiveFailure> {
    if let Some(size) = declared_size
        && size > max_bytes
    {
        return Err(ArchiveFailure::TooLarge(format!(
            "Archive entry '{name}' exceeds size limit ({size} > {max_bytes} bytes)"
        )));
    }
    let prealloc = declared_size
        .and_then(|s| usize::try_from(s).ok())
        .unwrap_or(0)
        .min(1024 * 1024);
    let mut buf = Vec::with_capacity(prealloc);
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > max_bytes {
        return Err(ArchiveFailure::TooLarge(format!(
            "Archive entry '{name}' decompressed size exceeded {max_bytes} bytes"
        )));
    }
    Ok(buf)
}

#[cfg(test)]
mod test_support {
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    use zip::CompressionMethod;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    pub(super) struct TempTestDir(PathBuf);

    impl TempTestDir {
        pub(super) fn new(prefix: &str) -> Self {
            let id = TEST_DIR_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "media-sort-comic-{prefix}-{}-{id}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }

        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(super) fn write_zip_entries(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create zip");
        let mut writer = ZipWriter::new(BufWriter::new(file));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for &(name, data) in entries {
            writer.start_file(name, opts).expect("start zip entry");
            writer.write_all(data).expect("write zip entry");
        }
        writer
            .finish()
            .expect("finish zip")
            .into_inner()
            .expect("flush zip");
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn comic_format_from_path_detects_supported_and_rejects_other_extensions() {
        assert_eq!(
            ComicFormat::from_path(Path::new("issue.cbz")).unwrap(),
            ComicFormat::Cbz
        );
        assert_eq!(
            ComicFormat::from_path(Path::new("ISSUE.CB7")).unwrap(),
            ComicFormat::Cb7
        );
        assert_eq!(
            ComicFormat::from_path(Path::new("book.Cbt")).unwrap(),
            ComicFormat::Cbt
        );
        assert_eq!(
            ComicFormat::from_path(Path::new("vol1.cbr")).unwrap(),
            ComicFormat::Cbr
        );
        assert!(ComicFormat::from_path(Path::new("archive.zip")).is_err());
        assert!(ComicFormat::from_path(Path::new("no_ext")).is_err());
    }

    #[test]
    fn sort_page_ids_orders_entries_naturally() {
        let pages = vec![
            PageEntry {
                id: 0,
                sort_name: "ch1/page10.png".to_string(),
            },
            PageEntry {
                id: 1,
                sort_name: "ch1/page2.png".to_string(),
            },
            PageEntry {
                id: 2,
                sort_name: "ch1/page1.png".to_string(),
            },
        ];
        assert_eq!(sort_page_ids(pages), vec![2, 1, 0]);
    }

    #[test]
    fn normalize_archive_name_replaces_backslashes() {
        assert_eq!(
            normalize_archive_name(r"vol1\ch2\page01.jpg"),
            "vol1/ch2/page01.jpg"
        );
    }

    #[test]
    fn prefer_comic_info_candidate_prefers_shallower_paths() {
        let none: Option<(usize, String)> = None;
        assert!(prefer_comic_info_candidate(&none, "sub/ComicInfo.xml"));

        let deep = Some((0usize, "a/b/ComicInfo.xml".to_string()));
        assert!(prefer_comic_info_candidate(&deep, "a/ComicInfo.xml"));
        assert!(prefer_comic_info_candidate(&deep, "ComicInfo.xml"));
        assert!(!prefer_comic_info_candidate(&deep, "a/b/c/ComicInfo.xml"));
        assert!(!prefer_comic_info_candidate(&deep, "x/y/ComicInfo.xml"));
    }

    #[test]
    fn read_stream_capped_enforces_declared_and_actual_limits() {
        let data = b"hello world";
        let ok = read_stream_capped(Cursor::new(data), Some(11), "ok.bin", 16).unwrap();
        assert_eq!(ok, data);

        let too_big_declared = read_stream_capped(Cursor::new(data), Some(17), "big_decl.bin", 16);
        assert!(matches!(too_big_declared, Err(ArchiveFailure::TooLarge(_))));

        let too_big_actual = read_stream_capped(
            Cursor::new(b"0123456789abcdefX"),
            None,
            "big_stream.bin",
            16,
        );
        assert!(matches!(too_big_actual, Err(ArchiveFailure::TooLarge(_))));
    }
}
