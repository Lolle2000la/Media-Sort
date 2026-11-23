use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use zip::ZipArchive;
use zip::result::ZipError;

use super::super::{MAX_ARCHIVE_ENTRY_BYTES, MAX_XML_ENTRY_BYTES, is_image_entry};
use super::window_cache::{IndexLru, lru_get, lru_insert, new_index_lru};
use super::{
    ArchiveFailure, DocumentError, PageEntry, empty_pages_error, normalize_archive_name,
    prefer_comic_info_candidate, read_stream_capped, sort_page_ids,
};
use crate::cache::CanonicalFileKey;

static CBZ_INDEX_CACHE: OnceLock<IndexLru<CbzPageIndex>> = OnceLock::new();

fn cbz_index_cache() -> &'static IndexLru<CbzPageIndex> {
    CBZ_INDEX_CACHE.get_or_init(new_index_lru)
}

struct CbzPageIndex {
    pages: Vec<usize>,
    any_encrypted: bool,
}

fn open_zip_archive(path: &Path) -> Result<ZipArchive<BufReader<File>>, ArchiveFailure> {
    let file = File::open(path)?;
    ZipArchive::new(BufReader::new(file)).map_err(map_zip_error)
}

fn map_zip_error(err: ZipError) -> ArchiveFailure {
    match err {
        ZipError::Io(io_err) => ArchiveFailure::Error(DocumentError::Io(io_err)),
        ZipError::InvalidPassword => ArchiveFailure::Encrypted,
        ZipError::UnsupportedArchive(msg)
            if msg == ZipError::PASSWORD_REQUIRED
                || msg.to_ascii_lowercase().contains("password")
                || msg.to_ascii_lowercase().contains("encrypt") =>
        {
            ArchiveFailure::Encrypted
        }
        ZipError::CompressionMethodNotSupported(id) => {
            ArchiveFailure::UnsupportedOrEmpty(format!("Unsupported ZIP compression method ({id})"))
        }
        ZipError::UnsupportedArchive(msg) => {
            ArchiveFailure::UnsupportedOrEmpty(format!("Unsupported ZIP archive: {msg}"))
        }
        other => ArchiveFailure::Error(DocumentError::Message(format!(
            "Failed to read ZIP comic archive: {other}"
        ))),
    }
}

fn read_zip_index_capped(
    archive: &mut ZipArchive<BufReader<File>>,
    index: usize,
    max_bytes: u64,
) -> Result<Vec<u8>, ArchiveFailure> {
    let meta = archive.by_index_data(index).map_err(map_zip_error)?;
    if meta.encrypted() {
        return Err(ArchiveFailure::Encrypted);
    }
    let name = meta
        .name()
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| format!("#{index}"));
    drop(meta);
    let entry = archive.by_index(index).map_err(map_zip_error)?;
    if entry.encrypted() {
        return Err(ArchiveFailure::Encrypted);
    }
    let declared = entry.size();
    read_stream_capped(entry, Some(declared), &name, max_bytes)
}

fn collect_zip_pages(archive: &ZipArchive<BufReader<File>>) -> (Vec<PageEntry<usize>>, bool) {
    let mut pages = Vec::new();
    let mut any_encrypted = false;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index_data(i) else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        if entry.encrypted() {
            any_encrypted = true;
        }
        let Ok(name) = entry.name() else {
            continue;
        };
        if is_image_entry(&name) {
            pages.push(PageEntry {
                id: i,
                sort_name: normalize_archive_name(&name),
            });
        }
    }
    (pages, any_encrypted)
}

pub(super) fn zip_extract_page(
    path: &Path,
    page_index: usize,
) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
    let key = CanonicalFileKey::from_path(path)?;
    let mut archive = open_zip_archive(&key.path)?;
    let index = if let Some(cached) = lru_get(cbz_index_cache(), &key) {
        cached
    } else {
        let (pages, any_encrypted) = collect_zip_pages(&archive);
        let sorted = CbzPageIndex {
            pages: sort_page_ids(pages),
            any_encrypted,
        };
        let arc = Arc::new(sorted);
        lru_insert(cbz_index_cache(), key, Arc::clone(&arc));
        arc
    };

    if index.pages.is_empty() {
        return Err(if index.any_encrypted {
            ArchiveFailure::Encrypted
        } else {
            empty_pages_error()
        });
    }
    let total = index.pages.len();
    let clamped = page_index.min(total - 1);
    let chosen_idx = index.pages[clamped];
    let bytes = read_zip_index_capped(&mut archive, chosen_idx, MAX_ARCHIVE_ENTRY_BYTES)?;
    Ok((Arc::new(bytes), clamped, total))
}

pub(super) fn zip_metadata(path: &Path) -> Result<(usize, Option<Vec<u8>>), ArchiveFailure> {
    let mut archive = open_zip_archive(path)?;
    let (pages, _) = collect_zip_pages(&archive);
    let page_count = pages.len();

    let mut xml_candidate: Option<(usize, String)> = None;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index_data(i) else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        let Ok(name) = entry.name() else {
            continue;
        };
        if super::is_comic_info_name(&name) && prefer_comic_info_candidate(&xml_candidate, &name) {
            xml_candidate = Some((i, normalize_archive_name(&name)));
        }
    }

    let xml_bytes = match xml_candidate {
        Some((idx, _)) => match read_zip_index_capped(&mut archive, idx, MAX_XML_ENTRY_BYTES) {
            Ok(bytes) => Some(bytes),
            Err(ArchiveFailure::Encrypted) => {
                if page_count == 0 {
                    return Err(ArchiveFailure::Encrypted);
                }
                None
            }
            Err(_) => None,
        },
        None => None,
    };

    Ok((page_count, xml_bytes))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{TempTestDir, write_zip_entries};
    use super::*;

    #[test]
    fn cbz_cached_index_page_turns() {
        let dir = TempTestDir::new("cbz_cache");
        let cbz_path = dir.path().join("issue.cbz");
        let p1 = b"cbz-page-1";
        let p2 = b"cbz-page-2";
        let p10 = b"cbz-page-10";

        write_zip_entries(
            &cbz_path,
            &[
                ("page10.png", p10),
                ("ComicInfo.xml", b"<ComicInfo/>"),
                ("page2.png", p2),
                ("page1.png", p1),
            ],
        );

        let key = CanonicalFileKey::from_path(&cbz_path).expect("cbz archive key");
        assert!(lru_get(cbz_index_cache(), &key).is_none());

        let (bytes0, clamped0, total0) = zip_extract_page(&cbz_path, 0).expect("cbz page 0");
        assert_eq!(bytes0.as_slice(), p1);
        assert_eq!((clamped0, total0), (0, 3));
        assert!(lru_get(cbz_index_cache(), &key).is_some());

        for (req, exp_clamped, exp_bytes) in [
            (1, 1, p2.as_slice()),
            (2, 2, p10.as_slice()),
            (50, 2, p10.as_slice()),
        ] {
            let (bytes, clamped, total) =
                zip_extract_page(&cbz_path, req).expect("cbz cached turn");
            assert_eq!((clamped, total), (exp_clamped, 3));
            assert_eq!(bytes.as_slice(), exp_bytes);
        }

        // Overwrite with a different-sized archive to verify key invalidation.
        write_zip_entries(&cbz_path, &[("only.png", b"only-page-updated")]);
        let (bytes, clamped, total) = zip_extract_page(&cbz_path, 0).expect("updated cbz");
        assert_eq!((clamped, total), (0, 1));
        assert_eq!(bytes.as_slice(), b"only-page-updated");
    }
}
