use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use media_sort_core::natural_sort::natural_cmp;

use super::super::{MAX_ARCHIVE_ENTRY_BYTES, MAX_XML_ENTRY_BYTES, is_image_entry};
use super::window_cache::{IndexLru, lru_get, lru_insert, new_index_lru};
use super::{
    ArchiveFailure, DocumentError, empty_pages_error, is_comic_info_name, normalize_archive_name,
    read_stream_capped,
};
use crate::cache::CanonicalFileKey;

static CBT_INDEX_CACHE: OnceLock<IndexLru<Vec<TarPageEntry>>> = OnceLock::new();

fn cbt_index_cache() -> &'static IndexLru<Vec<TarPageEntry>> {
    CBT_INDEX_CACHE.get_or_init(new_index_lru)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TarPageEntry {
    offset: u64,
    size: u64,
    name: String,
}

fn tar_entry_name<R: Read>(entry: &tar::Entry<'_, R>) -> String {
    if let Ok(path) = entry.path() {
        path.to_string_lossy().into_owned()
    } else {
        String::from_utf8_lossy(&entry.path_bytes()).into_owned()
    }
}

fn is_tar_regular_file<R: Read>(entry: &tar::Entry<'_, R>) -> bool {
    let ty = entry.header().entry_type();
    ty.is_file() || ty.is_contiguous()
}

pub(super) fn tar_extract_page(
    path: &Path,
    page_index: usize,
) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
    let key = CanonicalFileKey::from_path(path)?;
    let pages = if let Some(cached) = lru_get(cbt_index_cache(), &key) {
        cached
    } else {
        let file = File::open(&key.path)?;
        let mut archive = tar::Archive::new(BufReader::new(file));
        let entries = archive.entries_with_seek()?;
        let mut collected = Vec::new();
        for entry_res in entries {
            let entry = entry_res?;
            if !is_tar_regular_file(&entry) {
                continue;
            }
            let raw_name = tar_entry_name(&entry);
            if is_image_entry(&raw_name) {
                collected.push(TarPageEntry {
                    offset: entry.raw_file_position(),
                    size: entry.size(),
                    name: normalize_archive_name(&raw_name),
                });
            }
        }
        collected.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        let arc = Arc::new(collected);
        lru_insert(cbt_index_cache(), key.clone(), Arc::clone(&arc));
        arc
    };

    if pages.is_empty() {
        return Err(empty_pages_error());
    }
    let total = pages.len();
    let clamped = page_index.min(total - 1);
    let target = &pages[clamped];

    let mut file = File::open(&key.path)?;
    file.seek(SeekFrom::Start(target.offset))?;
    let bytes = read_stream_capped(
        file.take(target.size),
        Some(target.size),
        &target.name,
        MAX_ARCHIVE_ENTRY_BYTES,
    )?;
    if bytes.len() as u64 != target.size {
        return Err(ArchiveFailure::Error(DocumentError::Io(
            std::io::Error::new(
                ErrorKind::UnexpectedEof,
                format!("Truncated TAR comic entry '{}'", target.name),
            ),
        )));
    }
    Ok((Arc::new(bytes), clamped, total))
}

pub(super) fn tar_metadata(path: &Path) -> Result<(usize, Option<Vec<u8>>), ArchiveFailure> {
    let file = File::open(path)?;
    let mut archive = tar::Archive::new(BufReader::new(file));
    let entries = archive.entries_with_seek()?;

    let mut page_count = 0usize;
    let mut xml_bytes: Option<Vec<u8>> = None;
    let mut xml_depth = usize::MAX;

    for entry_res in entries {
        let entry = entry_res?;
        if !is_tar_regular_file(&entry) {
            continue;
        }
        let name = tar_entry_name(&entry);
        if is_image_entry(&name) {
            page_count += 1;
        } else if is_comic_info_name(&name) {
            let depth = normalize_archive_name(&name).matches('/').count();
            if xml_bytes.is_none() || depth < xml_depth {
                let declared = entry.size();
                if let Ok(bytes) =
                    read_stream_capped(entry, Some(declared), &name, MAX_XML_ENTRY_BYTES)
                {
                    xml_bytes = Some(bytes);
                    xml_depth = depth;
                }
            }
        }
    }

    Ok((page_count, xml_bytes))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::super::test_support::TempTestDir;
    use super::*;

    fn write_tar_entries(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create tar");
        let mut builder = tar::Builder::new(file);
        for &(name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, name, Cursor::new(data))
                .expect("append tar entry");
        }
        builder.finish().expect("finish tar");
    }

    #[test]
    fn cbt_cached_o1_seek_across_multi_page_tar() {
        let dir = TempTestDir::new("cbt_o1");
        let cbt_path = dir.path().join("book.cbt");
        let long_name = format!(
            "z_chapter/{}/page3.png",
            "very_long_subdirectory_segment_to_force_gnu_longname_extension".repeat(2)
        );
        let p1 = b"PAGE-1-DATA";
        let p2 = b"PAGE-2-PAYLOAD-LONGER";
        let p3 = b"PAGE-3-LONGNAME-DATA";
        let p10 = b"PAGE-10-FINAL";

        write_tar_entries(
            &cbt_path,
            &[
                ("page10.png", p10),
                (
                    "ComicInfo.xml",
                    b"<ComicInfo><Title>Tar</Title></ComicInfo>",
                ),
                (&long_name, p3),
                ("page2.png", p2),
                ("page1.png", p1),
            ],
        );

        let key = CanonicalFileKey::from_path(&cbt_path).expect("tar archive key");
        assert!(lru_get(cbt_index_cache(), &key).is_none());

        let (bytes0, clamped0, total0) = tar_extract_page(&cbt_path, 0).expect("page 0");
        assert_eq!(bytes0.as_slice(), p1);
        assert_eq!((clamped0, total0), (0, 4));

        // Cache must now hold sorted offsets directly usable via O(1) seek.
        let cached = lru_get(cbt_index_cache(), &key).expect("cbt cache populated");
        assert_eq!(cached.len(), 4);
        assert_eq!(cached[0].name, "page1.png");
        assert_eq!(cached[1].name, "page2.png");
        assert_eq!(cached[2].name, "page10.png");
        assert_eq!(cached[3].name, long_name);

        let expected: [(usize, usize, &[u8]); 5] =
            [(0, 0, p1), (1, 1, p2), (2, 2, p10), (3, 3, p3), (99, 3, p3)];
        for (req, exp_clamped, exp_bytes) in expected {
            let (bytes, clamped, total) = tar_extract_page(&cbt_path, req).expect("cached seek");
            assert_eq!(total, 4);
            assert_eq!(clamped, exp_clamped);
            assert_eq!(bytes.as_slice(), exp_bytes);
        }

        // Modifying the file invalidates the ArchiveKey even at the same path.
        write_tar_entries(
            &cbt_path,
            &[("page1.png", b"UPDATED-PAGE-1-DIFFERENT-SIZE")],
        );
        let (updated, clamped, total) = tar_extract_page(&cbt_path, 0).expect("updated tar");
        assert_eq!((clamped, total), (0, 1));
        assert_eq!(updated.as_slice(), b"UPDATED-PAGE-1-DIFFERENT-SIZE");
    }
}
