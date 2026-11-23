use std::hash::Hash;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lru::LruCache;

/// Cache key identifying a file by its canonical path, byte length, and
/// modification timestamp (nanoseconds since the Unix epoch).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalFileKey {
    pub path: PathBuf,
    pub file_len: u64,
    pub modified_nanos: u64,
}

impl CanonicalFileKey {
    /// Construct a key from an already-canonicalized path and its filesystem metadata.
    pub fn from_canonical_and_meta(path: PathBuf, meta: &std::fs::Metadata) -> Self {
        let modified_nanos = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);

        Self {
            path,
            file_len: meta.len(),
            modified_nanos,
        }
    }

    /// Canonicalize `path` and read its metadata to construct a [`CanonicalFileKey`].
    pub fn from_path(path: &Path) -> std::io::Result<Self> {
        let canonical = path.canonicalize()?;
        let meta = std::fs::metadata(&canonical)?;
        Ok(Self::from_canonical_and_meta(canonical, &meta))
    }
}

/// Bounded least-recently-used cache wrapping [`lru::LruCache`].
pub struct BoundedLru<K, V> {
    inner: LruCache<K, V>,
}

impl<K: Hash + Eq + std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug for BoundedLru<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedLru")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<K: Hash + Eq, V> BoundedLru<K, V> {
    /// Create a new LRU cache with the given non-zero capacity.
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: LruCache::new(capacity),
        }
    }

    /// Look up `key`, promoting it to most-recently-used if present.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.inner.get(key)
    }

    /// Returns `true` if `key` is present without updating recency order.
    pub fn contains(&self, key: &K) -> bool {
        self.inner.contains(key)
    }

    /// Insert `(key, value)`, returning the previous value if `key` already existed.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.inner.put(key, value)
    }

    /// Insert `(key, value)` and invoke `on_evict` if another entry is evicted
    /// to stay within capacity.
    ///
    /// Updating an existing `key` returns `Some(old_value)` and does not invoke
    /// `on_evict`. Inserting a new key at full capacity invokes `on_evict` with
    /// the evicted least-recently-used `(key, value)` pair and returns `None`.
    pub fn insert_with_evict(
        &mut self,
        key: K,
        value: V,
        mut on_evict: impl FnMut(K, V),
    ) -> Option<V> {
        if self.inner.contains(&key) {
            return self.inner.put(key, value);
        }
        if let Some((evicted_key, evicted_val)) = self.inner.push(key, value) {
            on_evict(evicted_key, evicted_val);
        }
        None
    }

    /// Remove `key` from the cache, returning its value if it was present.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.inner.pop(key)
    }

    /// Number of entries currently stored in the cache.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` when the cache contains no entries.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Remove all entries from the cache.
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("capacity must be non-zero")
    }

    #[test]
    fn test_eviction_order_respects_recency() {
        let mut cache = BoundedLru::new(cap(2));
        assert!(cache.is_empty());

        assert_eq!(cache.insert("a", 1), None);
        assert_eq!(cache.insert("b", 2), None);
        assert_eq!(cache.len(), 2);

        // Touch "a" so "b" becomes least-recently-used.
        assert_eq!(cache.get(&"a"), Some(&1));

        // Inserting "c" evicts "b", keeping "a" and "c".
        assert_eq!(cache.insert("c", 3), None);
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains(&"b"));
        assert_eq!(cache.get(&"a"), Some(&1));
        assert_eq!(cache.get(&"c"), Some(&3));

        assert_eq!(cache.remove(&"a"), Some(1));
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_insert_with_evict_callback() {
        let mut cache = BoundedLru::new(cap(2));
        let mut evicted = Vec::new();

        assert_eq!(
            cache.insert_with_evict("a", 10, |k, v| evicted.push((k, v))),
            None
        );
        assert_eq!(
            cache.insert_with_evict("b", 20, |k, v| evicted.push((k, v))),
            None
        );
        assert!(evicted.is_empty());

        // Updating an existing key returns old value and does NOT trigger on_evict.
        let prev = cache.insert_with_evict("a", 11, |k, v| evicted.push((k, v)));
        assert_eq!(prev, Some(10));
        assert!(evicted.is_empty());

        // "b" is now LRU because "a" was updated. Inserting "c" evicts "b".
        let prev = cache.insert_with_evict("c", 30, |k, v| evicted.push((k, v)));
        assert_eq!(prev, None);
        assert_eq!(evicted, vec![("b", 20)]);
        assert_eq!(cache.get(&"a"), Some(&11));
        assert_eq!(cache.get(&"c"), Some(&30));
    }

    #[test]
    fn test_canonical_file_key_from_path() {
        let dir = std::env::temp_dir().join(format!("mediasort_cache_key_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.bin");
        std::fs::write(&file, b"hello").unwrap();

        let key = CanonicalFileKey::from_path(&file).expect("key should resolve");
        assert_eq!(key.file_len, 5);
        assert_eq!(key.path, file.canonicalize().unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }
}
