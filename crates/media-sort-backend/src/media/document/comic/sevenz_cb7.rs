use std::fs::File;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use super::super::{MAX_ARCHIVE_ENTRY_BYTES, MAX_XML_ENTRY_BYTES, is_image_entry};
use super::window_cache::{
    IndexLru, SOLID_BATCH_MAX_BYTES, SOLID_UPCOMING_MAX_PAGES, SOLID_UPCOMING_WINDOW_SPAN, lru_get,
    lru_insert, new_index_lru, should_stop_solid_batch, store_window_batch, window_cache_get,
    window_cache_uncached_pages,
};
use super::{
    ArchiveFailure, DocumentError, PageEntry, empty_pages_error, is_comic_info_name,
    normalize_archive_name, prefer_comic_info_candidate, read_stream_capped, sort_page_ids,
};
use crate::cache::CanonicalFileKey;

/// Maximum dictionary/model memory budget passed to `sevenz_rust2` (512 MiB, in KiB).
const SEVENZ_MEM_LIMIT_KB: usize = 512 * 1024;

static CB7_INDEX_CACHE: OnceLock<IndexLru<Vec<usize>>> = OnceLock::new();

fn cb7_index_cache() -> &'static IndexLru<Vec<usize>> {
    CB7_INDEX_CACHE.get_or_init(new_index_lru)
}

struct SevenzBatchOutcome {
    target_bytes: Vec<u8>,
    upcoming_captured: Vec<(usize, Vec<u8>)>,
}

fn map_sevenz_error(err: sevenz_rust2::Error) -> ArchiveFailure {
    match err {
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_) => {
            ArchiveFailure::Encrypted
        }
        sevenz_rust2::Error::Io(io_err, _) => ArchiveFailure::Error(DocumentError::Io(io_err)),
        sevenz_rust2::Error::Unsupported(msg) => {
            ArchiveFailure::UnsupportedOrEmpty(format!("Unsupported 7z feature: {msg}"))
        }
        sevenz_rust2::Error::UnsupportedCompressionMethod(method) => {
            ArchiveFailure::UnsupportedOrEmpty(format!("Unsupported 7z compression: {method}"))
        }
        sevenz_rust2::Error::UnsupportedVersion { major, minor } => {
            ArchiveFailure::UnsupportedOrEmpty(format!("Unsupported 7z version {major}.{minor}"))
        }
        other => ArchiveFailure::Error(DocumentError::Message(format!(
            "Failed to read 7z comic archive: {other}"
        ))),
    }
}

fn open_sevenz_archive(path: &Path) -> Result<(File, sevenz_rust2::Archive), ArchiveFailure> {
    let mut file = File::open(path)?;
    let password = sevenz_rust2::Password::empty();
    let archive =
        sevenz_rust2::Archive::read_with_memory_limit(&mut file, &password, SEVENZ_MEM_LIMIT_KB)
            .map_err(map_sevenz_error)?;
    Ok((file, archive))
}

fn read_sevenz_batch_entries(
    file: &mut File,
    archive: &sevenz_rust2::Archive,
    file_index: usize,
    max_bytes: u64,
    upcoming_pages: &[(usize, usize)],
) -> Result<SevenzBatchOutcome, ArchiveFailure> {
    if archive.is_file_encrypted(file_index) {
        return Err(ArchiveFailure::Encrypted);
    }
    let Some(entry) = archive.files.get(file_index) else {
        return Err(ArchiveFailure::Error(DocumentError::Message(
            "7z entry index out of range".to_string(),
        )));
    };
    if !entry.has_stream() {
        return Ok(SevenzBatchOutcome {
            target_bytes: Vec::new(),
            upcoming_captured: Vec::new(),
        });
    }
    if entry.size() > max_bytes {
        return Err(ArchiveFailure::TooLarge(format!(
            "Archive entry '{}' exceeds size limit ({} > {max_bytes} bytes)",
            entry.name(),
            entry.size()
        )));
    }
    let Some(block_index) = archive
        .stream_map
        .file_block_index
        .get(file_index)
        .copied()
        .flatten()
    else {
        return Ok(SevenzBatchOutcome {
            target_bytes: Vec::new(),
            upcoming_captured: Vec::new(),
        });
    };

    let target_ptr = entry as *const sevenz_rust2::ArchiveEntry;
    let target_declared_size = entry.size();
    let mut upcoming_targets: Vec<(*const sevenz_rust2::ArchiveEntry, usize)> = upcoming_pages
        .iter()
        .filter_map(|&(page_idx, f_idx)| {
            if archive
                .stream_map
                .file_block_index
                .get(f_idx)
                .copied()
                .flatten()
                != Some(block_index)
                || archive.is_file_encrypted(f_idx)
            {
                return None;
            }
            let e = archive.files.get(f_idx)?;
            if !e.has_stream() || e.size() > MAX_ARCHIVE_ENTRY_BYTES {
                return None;
            }
            Some((e as *const sevenz_rust2::ArchiveEntry, page_idx))
        })
        .collect();

    let password = sevenz_rust2::Password::empty();
    let decoder = sevenz_rust2::BlockDecoder::new_with_memory_limit(
        1,
        SEVENZ_MEM_LIMIT_KB,
        block_index,
        archive,
        &password,
        file,
    );

    let mut target_outcome: Option<Result<Vec<u8>, ArchiveFailure>> = None;
    let mut upcoming_captured: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut batch_bytes: usize = 0;
    let mut budget_reached = false;

    let res = decoder.for_each_entries(&mut |cur_entry, reader| {
        if std::ptr::eq(cur_entry, target_ptr) {
            let name = cur_entry.name().to_string();
            let read_res = read_stream_capped(reader, Some(cur_entry.size()), &name, max_bytes);
            let keep_going = match &read_res {
                Ok(bytes) => {
                    batch_bytes = batch_bytes.saturating_add(bytes.len());
                    !should_stop_solid_batch(
                        true,
                        upcoming_targets.len(),
                        upcoming_captured.len(),
                        batch_bytes,
                        budget_reached,
                    )
                }
                Err(_) => false,
            };
            target_outcome = Some(read_res);
            return Ok(keep_going);
        }

        if let Some(pos) = upcoming_targets
            .iter()
            .position(|(ptr, _)| std::ptr::eq(cur_entry, *ptr))
        {
            let (_, page_idx) = upcoming_targets.swap_remove(pos);
            let reserved = if target_outcome.is_some() {
                0
            } else {
                target_declared_size
            };
            let used_plus_reserved = (batch_bytes as u64).saturating_add(reserved);
            if upcoming_captured.len() < SOLID_UPCOMING_MAX_PAGES
                && used_plus_reserved.saturating_add(cur_entry.size())
                    <= SOLID_BATCH_MAX_BYTES as u64
            {
                let remaining_cap = (SOLID_BATCH_MAX_BYTES as u64)
                    .saturating_sub(used_plus_reserved)
                    .min(MAX_ARCHIVE_ENTRY_BYTES);
                let name = cur_entry.name().to_string();
                match read_stream_capped(reader, Some(cur_entry.size()), &name, remaining_cap) {
                    Ok(bytes) => {
                        batch_bytes = batch_bytes.saturating_add(bytes.len());
                        upcoming_captured.push((page_idx, bytes));
                    }
                    Err(_) => {
                        if target_outcome.is_some() {
                            return Ok(false);
                        }
                    }
                }
            } else if target_outcome.is_some() {
                budget_reached = true;
            }
        }

        let target_ok = matches!(target_outcome, Some(Ok(_)));
        Ok(!should_stop_solid_batch(
            target_ok,
            upcoming_targets.len(),
            upcoming_captured.len(),
            batch_bytes,
            budget_reached,
        ))
    });

    match target_outcome {
        Some(Ok(target_bytes)) => Ok(SevenzBatchOutcome {
            target_bytes,
            upcoming_captured,
        }),
        Some(Err(err)) => Err(err),
        None => {
            res.map_err(map_sevenz_error)?;
            Err(ArchiveFailure::Error(DocumentError::Message(
                "Target entry missing from 7z compression block".to_string(),
            )))
        }
    }
}

fn read_sevenz_entry_capped(
    file: &mut File,
    archive: &sevenz_rust2::Archive,
    file_index: usize,
    max_bytes: u64,
) -> Result<Vec<u8>, ArchiveFailure> {
    read_sevenz_batch_entries(file, archive, file_index, max_bytes, &[])
        .map(|outcome| outcome.target_bytes)
}

fn collect_sevenz_pages(archive: &sevenz_rust2::Archive) -> Vec<PageEntry<usize>> {
    archive
        .files
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.is_directory() && !e.is_anti_item() && is_image_entry(e.name()))
        .map(|(idx, e)| PageEntry {
            id: idx,
            sort_name: normalize_archive_name(e.name()),
        })
        .collect()
}

pub(super) fn sevenz_extract_page(
    path: &Path,
    page_index: usize,
) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
    let key = CanonicalFileKey::from_path(path)?;
    let (pages, opened) = if let Some(cached) = lru_get(cb7_index_cache(), &key) {
        (cached, None)
    } else {
        let (file, archive) = open_sevenz_archive(&key.path)?;
        let sorted = Arc::new(sort_page_ids(collect_sevenz_pages(&archive)));
        lru_insert(cb7_index_cache(), key.clone(), Arc::clone(&sorted));
        (sorted, Some((file, archive)))
    };

    if pages.is_empty() {
        return Err(empty_pages_error());
    }
    let total = pages.len();
    let clamped = page_index.min(total - 1);

    if let Some(cached_bytes) = window_cache_get(&key, clamped) {
        return Ok((cached_bytes, clamped, total));
    }

    let (mut file, archive) = match opened {
        Some(pair) => pair,
        None => open_sevenz_archive(&key.path)?,
    };

    let chosen_idx = pages[clamped];
    let window_end = clamped
        .saturating_add(SOLID_UPCOMING_WINDOW_SPAN)
        .min(total);
    let upcoming_pages: Vec<(usize, usize)> =
        window_cache_uncached_pages(&key, (clamped + 1)..window_end)
            .into_iter()
            .map(|p| (p, pages[p]))
            .collect();

    let outcome = read_sevenz_batch_entries(
        &mut file,
        &archive,
        chosen_idx,
        MAX_ARCHIVE_ENTRY_BYTES,
        &upcoming_pages,
    )?;
    let bytes = Arc::new(outcome.target_bytes);
    store_window_batch(&key, clamped, Arc::clone(&bytes), outcome.upcoming_captured);
    Ok((bytes, clamped, total))
}

pub(super) fn sevenz_metadata(path: &Path) -> Result<(usize, Option<Vec<u8>>), ArchiveFailure> {
    let (mut file, archive) = open_sevenz_archive(path)?;
    let pages = collect_sevenz_pages(&archive);
    let page_count = pages.len();

    let mut xml_candidate: Option<(usize, String)> = None;
    for (idx, entry) in archive.files.iter().enumerate() {
        if entry.is_directory() || entry.is_anti_item() {
            continue;
        }
        let name = entry.name();
        if is_comic_info_name(name) && prefer_comic_info_candidate(&xml_candidate, name) {
            xml_candidate = Some((idx, normalize_archive_name(name)));
        }
    }

    let xml_bytes = match xml_candidate {
        Some((idx, _)) => {
            read_sevenz_entry_capped(&mut file, &archive, idx, MAX_XML_ENTRY_BYTES).ok()
        }
        None => None,
    };

    Ok((page_count, xml_bytes))
}
