use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};

use crate::cache::{BoundedLru, CanonicalFileKey};

/// LRU entry cap for per-archive page index caches.
pub(super) const INDEX_CACHE_CAPACITY: usize = 8;
/// Total raw page buffer budget for solid archive (`.cb7`/`.cbr`) page window caching (24 MiB).
pub(super) const COMIC_PAGE_WINDOW_CACHE_MAX_BYTES: usize = 24 * 1024 * 1024;
/// Maximum total bytes accumulated in a single solid archive decompression batch (16 MiB).
pub(super) const SOLID_BATCH_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Lookahead span `[clamped .. (clamped + SOLID_UPCOMING_WINDOW_SPAN).min(total)]` for solid pages.
pub(super) const SOLID_UPCOMING_WINDOW_SPAN: usize = 10;
/// Maximum number of upcoming pages opportunistically captured alongside `clamped`.
pub(super) const SOLID_UPCOMING_MAX_PAGES: usize = 8;

static COMIC_PAGE_WINDOW_CACHE: OnceLock<Mutex<WindowCacheState>> = OnceLock::new();

pub(super) type IndexLru<V> = Mutex<BoundedLru<CanonicalFileKey, Arc<V>>>;

pub(super) fn new_index_lru<V>() -> IndexLru<V> {
    Mutex::new(BoundedLru::new(
        NonZeroUsize::new(INDEX_CACHE_CAPACITY).expect("INDEX_CACHE_CAPACITY is non-zero"),
    ))
}

/// Looks up `key` in an [`IndexLru`] cache and promotes hits to most-recently-used.
pub(super) fn lru_get<V>(cache: &IndexLru<V>, key: &CanonicalFileKey) -> Option<Arc<V>> {
    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.get(key).map(Arc::clone)
}

/// Inserts `(key, value)` into an [`IndexLru`] cache, evicting oldest entries past capacity.
pub(super) fn lru_insert<V>(cache: &IndexLru<V>, key: CanonicalFileKey, value: Arc<V>) {
    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.insert(key, value);
}

struct WindowCacheEntry {
    archive: CanonicalFileKey,
    page_index: usize,
    bytes: Arc<Vec<u8>>,
}

pub(super) struct WindowCacheState {
    entries: VecDeque<WindowCacheEntry>,
    total_bytes: usize,
    max_bytes: usize,
}

impl WindowCacheState {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    fn get(&mut self, archive: &CanonicalFileKey, page_index: usize) -> Option<Arc<Vec<u8>>> {
        let pos = self
            .entries
            .iter()
            .position(|e| &e.archive == archive && e.page_index == page_index)?;
        let entry = self.entries.remove(pos)?;
        let bytes = Arc::clone(&entry.bytes);
        self.entries.push_back(entry);
        Some(bytes)
    }

    pub(super) fn contains(&self, archive: &CanonicalFileKey, page_index: usize) -> bool {
        self.entries
            .iter()
            .any(|e| &e.archive == archive && e.page_index == page_index)
    }

    fn insert(&mut self, archive: CanonicalFileKey, page_index: usize, bytes: Arc<Vec<u8>>) {
        let entry_len = bytes.len();
        if entry_len > self.max_bytes {
            return;
        }
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.archive == archive && e.page_index == page_index)
            && let Some(old) = self.entries.remove(pos)
        {
            self.total_bytes = self.total_bytes.saturating_sub(old.bytes.len());
        }
        while self.total_bytes.saturating_add(entry_len) > self.max_bytes {
            let Some(evicted) = self.entries.pop_front() else {
                break;
            };
            self.total_bytes = self.total_bytes.saturating_sub(evicted.bytes.len());
        }
        self.total_bytes += entry_len;
        self.entries.push_back(WindowCacheEntry {
            archive,
            page_index,
            bytes,
        });
    }
}

pub(super) fn comic_page_window_cache() -> &'static Mutex<WindowCacheState> {
    COMIC_PAGE_WINDOW_CACHE
        .get_or_init(|| Mutex::new(WindowCacheState::new(COMIC_PAGE_WINDOW_CACHE_MAX_BYTES)))
}

pub(super) fn window_cache_get(key: &CanonicalFileKey, page_index: usize) -> Option<Arc<Vec<u8>>> {
    let mut cache = comic_page_window_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.get(key, page_index)
}

pub(super) fn window_cache_uncached_pages(
    key: &CanonicalFileKey,
    range: std::ops::Range<usize>,
) -> Vec<usize> {
    let cache = comic_page_window_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    range.filter(|&p| !cache.contains(key, p)).collect()
}

pub(super) fn store_window_batch(
    key: &CanonicalFileKey,
    clamped: usize,
    clamped_bytes: Arc<Vec<u8>>,
    mut upcoming: Vec<(usize, Vec<u8>)>,
) {
    // Insert furthest upcoming pages first so closer upcoming pages and `clamped` stay MRU.
    upcoming.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut cache = comic_page_window_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (page_idx, bytes) in upcoming {
        cache.insert(key.clone(), page_idx, Arc::new(bytes));
    }
    cache.insert(key.clone(), clamped, clamped_bytes);
}

pub(super) fn should_stop_solid_batch(
    target_done: bool,
    remaining_targets: usize,
    captured_upcoming: usize,
    batch_bytes: usize,
    budget_reached: bool,
) -> bool {
    target_done
        && (remaining_targets == 0
            || captured_upcoming >= SOLID_UPCOMING_MAX_PAGES
            || batch_bytes >= SOLID_BATCH_MAX_BYTES
            || budget_reached)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn synthetic_key(id: u64) -> CanonicalFileKey {
        CanonicalFileKey {
            path: PathBuf::from(format!("/virtual/archive-{id}.cbz")),
            file_len: id,
            modified_nanos: id,
        }
    }

    #[test]
    fn comic_page_window_cache_enforces_24_mib_lru_cap() {
        let mut cache = WindowCacheState::new(COMIC_PAGE_WINDOW_CACHE_MAX_BYTES);
        let key = CanonicalFileKey {
            path: PathBuf::from("/virtual/solid.cb7"),
            file_len: 1234,
            modified_nanos: 5678,
        };
        let nine_mib = 9 * 1024 * 1024;

        cache.insert(key.clone(), 0, Arc::new(vec![0u8; nine_mib]));
        cache.insert(key.clone(), 1, Arc::new(vec![1u8; nine_mib]));
        assert_eq!(cache.total_bytes, 2 * nine_mib);

        // Inserting a 3rd 9 MiB page (27 MiB > 24 MiB) must evict page 0.
        cache.insert(key.clone(), 2, Arc::new(vec![2u8; nine_mib]));
        assert_eq!(cache.total_bytes, 2 * nine_mib);
        assert!(!cache.contains(&key, 0));
        assert!(cache.contains(&key, 1));
        assert!(cache.contains(&key, 2));

        // Reading page 1 promotes it to MRU, so inserting page 3 evicts page 2 instead.
        assert!(cache.get(&key, 1).is_some());
        cache.insert(key.clone(), 3, Arc::new(vec![3u8; nine_mib]));
        assert!(cache.contains(&key, 1));
        assert!(!cache.contains(&key, 2));
        assert!(cache.contains(&key, 3));
        assert!(cache.total_bytes <= COMIC_PAGE_WINDOW_CACHE_MAX_BYTES);

        // Entries larger than the 24 MiB cap are skipped without evicting existing entries.
        let oversized = COMIC_PAGE_WINDOW_CACHE_MAX_BYTES + 1;
        cache.insert(key.clone(), 4, Arc::new(vec![4u8; oversized]));
        assert!(!cache.contains(&key, 4));
        assert!(cache.contains(&key, 1));
        assert!(cache.contains(&key, 3));
    }

    #[test]
    fn index_lru_enforces_capacity_and_promotes_on_get() {
        let cache: IndexLru<usize> = new_index_lru();
        for i in 0..INDEX_CACHE_CAPACITY as u64 {
            lru_insert(&cache, synthetic_key(i), Arc::new(i as usize));
        }

        // Promote entry 0 to MRU before inserting one more than capacity.
        assert_eq!(lru_get(&cache, &synthetic_key(0)).as_deref(), Some(&0));
        lru_insert(
            &cache,
            synthetic_key(INDEX_CACHE_CAPACITY as u64),
            Arc::new(INDEX_CACHE_CAPACITY),
        );

        // Entry 1 (now LRU) should be evicted while promoted entry 0 remains.
        assert!(lru_get(&cache, &synthetic_key(1)).is_none());
        assert_eq!(lru_get(&cache, &synthetic_key(0)).as_deref(), Some(&0));
        assert_eq!(
            lru_get(&cache, &synthetic_key(INDEX_CACHE_CAPACITY as u64)).as_deref(),
            Some(&INDEX_CACHE_CAPACITY)
        );
    }
}
