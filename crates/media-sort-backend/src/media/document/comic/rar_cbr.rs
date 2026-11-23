use std::io::ErrorKind;
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
    normalize_archive_name, prefer_comic_info_candidate, sort_page_ids,
};
use crate::cache::CanonicalFileKey;

/// Maximum memory budget passed to `rarchive` (256 MiB).
const RARCHIVE_MEM_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
/// Sentinel `std::io::Error` message used to abort `rarchive` cleanly after capturing target entries.
const RARCHIVE_EARLY_STOP: &str = "media-sort-rarchive-early-stop";

static CBR_INDEX_CACHE: OnceLock<IndexLru<CbrPageIndex>> = OnceLock::new();

fn cbr_index_cache() -> &'static IndexLru<CbrPageIndex> {
    CBR_INDEX_CACHE.get_or_init(new_index_lru)
}

struct CbrPageIndex {
    pages: Vec<usize>,
    any_encrypted: bool,
}

struct UpcomingRarTarget {
    ptr: *const rarchive::Entry,
    page_idx: usize,
}

struct ActiveRarCapture {
    ptr: *const rarchive::Entry,
    page_idx: usize,
    is_clamped: bool,
    name: String,
    max_bytes: u64,
    buf: Vec<u8>,
}

struct RarchiveBatchSink {
    target_ptr: *const rarchive::Entry,
    target_name: String,
    target_declared_size: u64,
    max_bytes: u64,
    upcoming_targets: Vec<UpcomingRarTarget>,
    active: Option<ActiveRarCapture>,
    target_buf: Option<Vec<u8>>,
    too_large: Option<String>,
    upcoming_captured: Vec<(usize, Vec<u8>)>,
    batch_bytes: usize,
    budget_reached: bool,
}

impl RarchiveBatchSink {
    fn should_stop_early(&self) -> bool {
        should_stop_solid_batch(
            self.target_buf.is_some(),
            self.upcoming_targets.len(),
            self.upcoming_captured.len(),
            self.batch_bytes,
            self.budget_reached,
        )
    }
}

impl rarchive::Sink for RarchiveBatchSink {
    fn begin(&mut self, entry: &rarchive::Entry) -> std::io::Result<bool> {
        if self.should_stop_early() {
            return Err(std::io::Error::other(RARCHIVE_EARLY_STOP));
        }

        if self.target_buf.is_none() && std::ptr::eq(entry, self.target_ptr) {
            if entry.size > self.max_bytes {
                let msg = format!(
                    "Archive entry '{}' exceeds size limit ({} > {} bytes)",
                    self.target_name, entry.size, self.max_bytes
                );
                self.too_large = Some(msg.clone());
                return Err(std::io::Error::new(ErrorKind::InvalidData, msg));
            }
            let prealloc = usize::try_from(entry.size).unwrap_or(0).min(1024 * 1024);
            self.active = Some(ActiveRarCapture {
                ptr: entry,
                page_idx: 0,
                is_clamped: true,
                name: self.target_name.clone(),
                max_bytes: self.max_bytes,
                buf: Vec::with_capacity(prealloc),
            });
            return Ok(true);
        }

        if let Some(pos) = self
            .upcoming_targets
            .iter()
            .position(|t| std::ptr::eq(entry, t.ptr))
        {
            let target = self.upcoming_targets.swap_remove(pos);
            let reserved = if self.target_buf.is_some() {
                0
            } else {
                self.target_declared_size
            };
            let used_plus_reserved = (self.batch_bytes as u64).saturating_add(reserved);
            if self.upcoming_captured.len() < SOLID_UPCOMING_MAX_PAGES
                && entry.size <= MAX_ARCHIVE_ENTRY_BYTES
                && used_plus_reserved.saturating_add(entry.size) <= SOLID_BATCH_MAX_BYTES as u64
            {
                let remaining_cap = (SOLID_BATCH_MAX_BYTES as u64)
                    .saturating_sub(used_plus_reserved)
                    .min(MAX_ARCHIVE_ENTRY_BYTES);
                let prealloc = usize::try_from(entry.size).unwrap_or(0).min(1024 * 1024);
                self.active = Some(ActiveRarCapture {
                    ptr: entry,
                    page_idx: target.page_idx,
                    is_clamped: false,
                    name: entry.name.clone(),
                    max_bytes: remaining_cap,
                    buf: Vec::with_capacity(prealloc),
                });
                return Ok(true);
            }
            if self.target_buf.is_some() {
                self.budget_reached = true;
                return Err(std::io::Error::other(RARCHIVE_EARLY_STOP));
            }
        }

        Ok(false)
    }

    fn data(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        if active.buf.len() as u64 + bytes.len() as u64 > active.max_bytes {
            if active.is_clamped {
                let msg = format!(
                    "Archive entry '{}' decompressed size exceeded {} bytes",
                    active.name, active.max_bytes
                );
                self.too_large = Some(msg.clone());
                return Err(std::io::Error::new(ErrorKind::InvalidData, msg));
            }
            self.active = None;
            if self.target_buf.is_some() {
                self.budget_reached = true;
                return Err(std::io::Error::other(RARCHIVE_EARLY_STOP));
            }
            return Ok(());
        }
        active.buf.extend_from_slice(bytes);
        Ok(())
    }

    fn end(&mut self, entry: &rarchive::Entry) -> std::io::Result<()> {
        if let Some(active) = self.active.take()
            && std::ptr::eq(entry, active.ptr)
        {
            self.batch_bytes = self.batch_bytes.saturating_add(active.buf.len());
            if active.is_clamped {
                self.target_buf = Some(active.buf);
            } else {
                self.upcoming_captured.push((active.page_idx, active.buf));
            }
        }
        if self.should_stop_early() {
            return Err(std::io::Error::other(RARCHIVE_EARLY_STOP));
        }
        Ok(())
    }
}

fn map_rarchive_error(err: rarchive::Error) -> ArchiveFailure {
    match err {
        rarchive::Error::NeedPassword | rarchive::Error::BadPassword => ArchiveFailure::Encrypted,
        rarchive::Error::OldFormat => {
            ArchiveFailure::UnsupportedOrEmpty("Legacy RAR 1.3/1.4 format is unsupported".into())
        }
        rarchive::Error::Unsupported(what) => {
            if what.to_ascii_lowercase().contains("encrypt") {
                ArchiveFailure::Encrypted
            } else {
                ArchiveFailure::UnsupportedOrEmpty(format!("Unsupported RAR feature: {what}"))
            }
        }
        rarchive::Error::Corrupt(what) => ArchiveFailure::Error(DocumentError::Message(format!(
            "Corrupt RAR archive: {what}"
        ))),
        rarchive::Error::NotArchive => {
            ArchiveFailure::Error(DocumentError::Message("Not a valid RAR archive".into()))
        }
        rarchive::Error::BadChecksum(name) => ArchiveFailure::Error(DocumentError::Message(
            format!("RAR checksum mismatch for '{name}'"),
        )),
        rarchive::Error::Io(io_err) => ArchiveFailure::Error(DocumentError::Io(io_err)),
    }
}

fn open_rarchive(path: &Path) -> Result<rarchive::Archive, ArchiveFailure> {
    let mut archive = rarchive::Archive::open(path).map_err(map_rarchive_error)?;
    archive.set_memory_budget(RARCHIVE_MEM_LIMIT_BYTES);
    Ok(archive)
}

struct RarBatchOutcome {
    target_bytes: Vec<u8>,
    upcoming_captured: Vec<(usize, Vec<u8>)>,
}

fn extract_rarchive_batch(
    archive: &rarchive::Archive,
    entry_idx: usize,
    max_bytes: u64,
    upcoming_pages: &[(usize, usize)],
) -> Result<RarBatchOutcome, ArchiveFailure> {
    let entries = archive.entries();
    let Some(entry) = entries.get(entry_idx) else {
        return Err(ArchiveFailure::Error(DocumentError::Message(
            "RAR entry index out of range".to_string(),
        )));
    };
    if entry.encrypted {
        return Err(ArchiveFailure::Encrypted);
    }
    if entry.size > max_bytes {
        return Err(ArchiveFailure::TooLarge(format!(
            "Archive entry '{}' exceeds size limit ({} > {} bytes)",
            entry.name, entry.size, max_bytes
        )));
    }

    let upcoming_targets = upcoming_pages
        .iter()
        .filter_map(|&(page_idx, e_idx)| {
            let e = entries.get(e_idx)?;
            if e.encrypted || e.size > MAX_ARCHIVE_ENTRY_BYTES {
                return None;
            }
            Some(UpcomingRarTarget {
                ptr: e as *const rarchive::Entry,
                page_idx,
            })
        })
        .collect();

    let mut sink = RarchiveBatchSink {
        target_ptr: entry as *const rarchive::Entry,
        target_name: entry.name.clone(),
        target_declared_size: entry.size,
        max_bytes,
        upcoming_targets,
        active: None,
        target_buf: None,
        too_large: None,
        upcoming_captured: Vec::new(),
        batch_bytes: 0,
        budget_reached: false,
    };

    match archive.extract(&mut sink) {
        Ok(()) => Ok(RarBatchOutcome {
            target_bytes: sink.target_buf.unwrap_or_default(),
            upcoming_captured: sink.upcoming_captured,
        }),
        Err(err) => {
            if let Some(target_bytes) = sink.target_buf {
                return Ok(RarBatchOutcome {
                    target_bytes,
                    upcoming_captured: sink.upcoming_captured,
                });
            }
            if let Some(msg) = sink.too_large {
                return Err(ArchiveFailure::TooLarge(msg));
            }
            Err(map_rarchive_error(err))
        }
    }
}

fn extract_rarchive_entry(
    archive: &rarchive::Archive,
    entry_idx: usize,
    max_bytes: u64,
) -> Result<Vec<u8>, ArchiveFailure> {
    extract_rarchive_batch(archive, entry_idx, max_bytes, &[]).map(|o| o.target_bytes)
}

fn collect_rarchive_pages(archive: &rarchive::Archive) -> (Vec<PageEntry<usize>>, bool) {
    let mut pages = Vec::new();
    let mut any_encrypted = archive.needs_password();
    for (idx, entry) in archive.entries().iter().enumerate() {
        if entry.is_dir || entry.is_link {
            continue;
        }
        if entry.encrypted {
            any_encrypted = true;
        }
        if is_image_entry(&entry.name) {
            pages.push(PageEntry {
                id: idx,
                sort_name: normalize_archive_name(&entry.name),
            });
        }
    }
    (pages, any_encrypted)
}

pub(super) fn rar_extract_page(
    path: &Path,
    page_index: usize,
) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
    let key = CanonicalFileKey::from_path(path)?;
    let (index, opened) = if let Some(cached) = lru_get(cbr_index_cache(), &key) {
        (cached, None)
    } else {
        let archive = open_rarchive(&key.path)?;
        let (pages, any_encrypted) = collect_rarchive_pages(&archive);
        let sorted = Arc::new(CbrPageIndex {
            pages: sort_page_ids(pages),
            any_encrypted,
        });
        lru_insert(cbr_index_cache(), key.clone(), Arc::clone(&sorted));
        (sorted, Some(archive))
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

    if let Some(cached_bytes) = window_cache_get(&key, clamped) {
        return Ok((cached_bytes, clamped, total));
    }

    let archive = match opened {
        Some(archive) => archive,
        None => open_rarchive(&key.path)?,
    };

    let chosen_idx = index.pages[clamped];
    let window_end = clamped
        .saturating_add(SOLID_UPCOMING_WINDOW_SPAN)
        .min(total);
    let upcoming_pages: Vec<(usize, usize)> =
        window_cache_uncached_pages(&key, (clamped + 1)..window_end)
            .into_iter()
            .map(|p| (p, index.pages[p]))
            .collect();

    let outcome = extract_rarchive_batch(
        &archive,
        chosen_idx,
        MAX_ARCHIVE_ENTRY_BYTES,
        &upcoming_pages,
    )?;
    let bytes = Arc::new(outcome.target_bytes);
    store_window_batch(&key, clamped, Arc::clone(&bytes), outcome.upcoming_captured);
    Ok((bytes, clamped, total))
}

pub(super) fn rar_metadata(path: &Path) -> Result<(usize, Option<Vec<u8>>), ArchiveFailure> {
    let archive = open_rarchive(path)?;
    let (pages, any_encrypted) = collect_rarchive_pages(&archive);
    let page_count = pages.len();
    if page_count == 0 && any_encrypted {
        return Err(ArchiveFailure::Encrypted);
    }

    let mut xml_candidate: Option<(usize, String)> = None;
    for (idx, entry) in archive.entries().iter().enumerate() {
        if entry.is_dir || entry.is_link {
            continue;
        }
        if is_comic_info_name(&entry.name)
            && prefer_comic_info_candidate(&xml_candidate, &entry.name)
        {
            xml_candidate = Some((idx, normalize_archive_name(&entry.name)));
        }
    }

    let xml_bytes = match xml_candidate {
        Some((idx, _)) => extract_rarchive_entry(&archive, idx, MAX_XML_ENTRY_BYTES).ok(),
        None => None,
    };

    Ok((page_count, xml_bytes))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{TempTestDir, write_zip_entries};
    use super::super::window_cache::comic_page_window_cache;
    use super::*;

    #[test]
    fn cbr_batch_window_caches_upcoming_pages_and_never_stops_before_clamped() {
        let dir = TempTestDir::new("cbr_window");

        // Forward order: page 0 caches page 0 + 8 upcoming (0..=8), leaving 9..12 uncached.
        let fwd_path = dir.path().join("forward.cbr");
        let fwd_data: Vec<(String, Vec<u8>)> = (1..=12)
            .map(|i| {
                (
                    format!("page{i:02}.png"),
                    format!("fwd-page-{i:02}").into_bytes(),
                )
            })
            .collect();
        let fwd_refs: Vec<(&str, &[u8])> = fwd_data
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        write_zip_entries(&fwd_path, &fwd_refs);

        let fwd_key = CanonicalFileKey::from_path(&fwd_path).expect("fwd key");
        let (bytes0, clamped0, total0) = rar_extract_page(&fwd_path, 0).expect("fwd page 0");
        assert_eq!((clamped0, total0), (0, 12));
        assert_eq!(bytes0.as_slice(), b"fwd-page-01");

        {
            let guard = comic_page_window_cache()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for p in 0..=8 {
                assert!(
                    guard.contains(&fwd_key, p),
                    "expected forward page {p} in window cache"
                );
            }
            for p in 9..12 {
                assert!(
                    !guard.contains(&fwd_key, p),
                    "did not expect forward page {p} in window cache yet"
                );
            }
        }

        // Subsequent page turn hits window cache and matches payload.
        let (bytes5, clamped5, total5) = rar_extract_page(&fwd_path, 5).expect("fwd page 5");
        assert_eq!((clamped5, total5), (5, 12));
        assert_eq!(bytes5.as_slice(), b"fwd-page-06");

        // Reverse archive order: page01 is the LAST entry in the archive.
        // Requesting page 0 must NOT stop early after capturing 8 upcoming entries before it.
        let rev_path = dir.path().join("reverse.cbr");
        let rev_data: Vec<(String, Vec<u8>)> = (1..=12)
            .rev()
            .map(|i| {
                (
                    format!("page{i:02}.png"),
                    format!("rev-page-{i:02}").into_bytes(),
                )
            })
            .collect();
        let rev_refs: Vec<(&str, &[u8])> = rev_data
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        write_zip_entries(&rev_path, &rev_refs);

        let rev_key = CanonicalFileKey::from_path(&rev_path).expect("rev key");
        let (rev_bytes0, rev_clamped0, rev_total0) =
            rar_extract_page(&rev_path, 0).expect("reverse page 0");
        assert_eq!((rev_clamped0, rev_total0), (0, 12));
        assert_eq!(rev_bytes0.as_slice(), b"rev-page-01");
        assert!(
            window_cache_get(&rev_key, 0).is_some(),
            "clamped target must be cached even when encountered last"
        );
    }
}
