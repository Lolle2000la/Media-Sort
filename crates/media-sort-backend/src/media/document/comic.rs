use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};
use zip::ZipArchive;
use zip::result::ZipError;

use super::{
    DecodedImage, DocumentError, DocumentPreview, DocumentUnavailableReason,
    MAX_ARCHIVE_ENTRY_BYTES, MAX_XML_ENTRY_BYTES, PREVIEW_TARGET_HEIGHT, PREVIEW_TARGET_WIDTH,
    is_image_entry, is_sandboxed, natural_cmp,
};
use crate::media::format_pipeline::decode_and_resize_bytes;

/// Maximum dictionary/model memory budget passed to `sevenz_rust2` (512 MiB, in KiB).
const SEVENZ_MEM_LIMIT_KB: usize = 512 * 1024;
/// Maximum memory budget passed to `rarchive` (256 MiB).
const RARCHIVE_MEM_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
/// Hard execution timeout for external CBR CLI fallback invocations.
const CBR_CLI_TIMEOUT: Duration = Duration::from_secs(15);
/// Cap on stderr captured from external CBR CLI fallback invocations.
const MAX_CLI_STDERR_BYTES: u64 = 64 * 1024;
/// Windows `CREATE_NO_WINDOW` creation flag (`winbase.h`).
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Sentinel `std::io::Error` message used to abort `rarchive` cleanly after capturing target entries.
const RARCHIVE_EARLY_STOP: &str = "media-sort-rarchive-early-stop";
/// LRU entry cap for per-archive page/package index caches across comic and EPUB modules.
pub(super) const INDEX_CACHE_CAPACITY: usize = 8;
/// Total raw page buffer budget for solid archive (`.cb7`/`.cbr`) page window caching (24 MiB).
const COMIC_PAGE_WINDOW_CACHE_MAX_BYTES: usize = 24 * 1024 * 1024;
/// Maximum total bytes accumulated in a single solid archive decompression batch (16 MiB).
const SOLID_BATCH_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Lookahead span `[clamped .. (clamped + SOLID_UPCOMING_WINDOW_SPAN).min(total)]` for solid pages.
const SOLID_UPCOMING_WINDOW_SPAN: usize = 10;
/// Maximum number of upcoming pages opportunistically captured alongside `clamped`.
const SOLID_UPCOMING_MAX_PAGES: usize = 8;

static CBR_CLI: OnceLock<Option<CbrCli>> = OnceLock::new();
static CBZ_INDEX_CACHE: OnceLock<IndexLru<CbzPageIndex>> = OnceLock::new();
static CBT_INDEX_CACHE: OnceLock<IndexLru<Vec<TarPageEntry>>> = OnceLock::new();
static CB7_INDEX_CACHE: OnceLock<IndexLru<Vec<usize>>> = OnceLock::new();
static CBR_INDEX_CACHE: OnceLock<IndexLru<CbrPageIndex>> = OnceLock::new();
static COMIC_PAGE_WINDOW_CACHE: OnceLock<Mutex<WindowCacheState>> = OnceLock::new();

pub(super) type IndexLru<V> = Mutex<VecDeque<(ArchiveKey, Arc<V>)>>;

/// File identity key used to invalidate cached archive indices and page buffers on modification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ArchiveKey {
    pub(super) path: PathBuf,
    pub(super) file_len: u64,
    pub(super) modified_nanos: u64,
}

/// Builds an [`ArchiveKey`] from the canonicalized path and file metadata of `path`.
pub(super) fn archive_key(path: &Path) -> std::io::Result<ArchiveKey> {
    let canonical = std::fs::canonicalize(path)?;
    let meta = std::fs::metadata(&canonical)?;
    let file_len = meta.len();
    let modified_nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    Ok(ArchiveKey {
        path: canonical,
        file_len,
        modified_nanos,
    })
}

/// Looks up `key` in an [`IndexLru`] cache and promotes hits to most-recently-used.
pub(super) fn lru_get<V>(cache: &IndexLru<V>, key: &ArchiveKey) -> Option<Arc<V>> {
    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pos = guard.iter().position(|(k, _)| k == key)?;
    let entry = guard.remove(pos)?;
    let val = Arc::clone(&entry.1);
    guard.push_back(entry);
    Some(val)
}

/// Inserts `(key, value)` into an [`IndexLru`] cache, evicting oldest entries past `capacity`.
pub(super) fn lru_insert<V>(cache: &IndexLru<V>, capacity: usize, key: ArchiveKey, value: Arc<V>) {
    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(pos) = guard.iter().position(|(k, _)| k == &key) {
        guard.remove(pos);
    }
    while guard.len() >= capacity {
        guard.pop_front();
    }
    guard.push_back((key, value));
}

struct WindowCacheEntry {
    archive: ArchiveKey,
    page_index: usize,
    bytes: Arc<Vec<u8>>,
}

struct WindowCacheState {
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

    fn get(&mut self, archive: &ArchiveKey, page_index: usize) -> Option<Arc<Vec<u8>>> {
        let pos = self
            .entries
            .iter()
            .position(|e| &e.archive == archive && e.page_index == page_index)?;
        let entry = self.entries.remove(pos)?;
        let bytes = Arc::clone(&entry.bytes);
        self.entries.push_back(entry);
        Some(bytes)
    }

    fn contains(&self, archive: &ArchiveKey, page_index: usize) -> bool {
        self.entries
            .iter()
            .any(|e| &e.archive == archive && e.page_index == page_index)
    }

    fn insert(&mut self, archive: ArchiveKey, page_index: usize, bytes: Arc<Vec<u8>>) {
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

fn cbz_index_cache() -> &'static IndexLru<CbzPageIndex> {
    CBZ_INDEX_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(INDEX_CACHE_CAPACITY)))
}

fn cbt_index_cache() -> &'static IndexLru<Vec<TarPageEntry>> {
    CBT_INDEX_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(INDEX_CACHE_CAPACITY)))
}

fn cb7_index_cache() -> &'static IndexLru<Vec<usize>> {
    CB7_INDEX_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(INDEX_CACHE_CAPACITY)))
}

fn cbr_index_cache() -> &'static IndexLru<CbrPageIndex> {
    CBR_INDEX_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(INDEX_CACHE_CAPACITY)))
}

fn comic_page_window_cache() -> &'static Mutex<WindowCacheState> {
    COMIC_PAGE_WINDOW_CACHE
        .get_or_init(|| Mutex::new(WindowCacheState::new(COMIC_PAGE_WINDOW_CACHE_MAX_BYTES)))
}

fn window_cache_get(key: &ArchiveKey, page_index: usize) -> Option<Arc<Vec<u8>>> {
    let mut cache = comic_page_window_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.get(key, page_index)
}

fn window_cache_uncached_pages(key: &ArchiveKey, range: std::ops::Range<usize>) -> Vec<usize> {
    let cache = comic_page_window_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    range.filter(|&p| !cache.contains(key, p)).collect()
}

fn store_window_batch(
    key: &ArchiveKey,
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
        ComicFormat::Cbz => zip_metadata(path),
        ComicFormat::Cb7 => sevenz_metadata(path),
        ComicFormat::Cbt => tar_metadata(path),
        ComicFormat::Cbr => rar_metadata(path),
    };

    let mut fields = BTreeMap::new();
    match outcome {
        Ok((page_count, comic_info_bytes)) => {
            fields.insert("Pages".into(), page_count.to_string());
            if let Some(xml_bytes) = comic_info_bytes {
                parse_comic_info_xml(&xml_bytes, &mut fields);
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
        ComicFormat::Cbz => zip_extract_page(path, page_index),
        ComicFormat::Cb7 => sevenz_extract_page(path, page_index),
        ComicFormat::Cbt => tar_extract_page(path, page_index),
        ComicFormat::Cbr => rar_extract_page(path, page_index),
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

fn prefer_comic_info_candidate(current: &Option<(usize, String)>, candidate: &str) -> bool {
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

// ── CBZ (ZIP) ─────────────────────────────────────────────────────────

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

struct CbzPageIndex {
    pages: Vec<usize>,
    any_encrypted: bool,
}

fn zip_extract_page(
    path: &Path,
    page_index: usize,
) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
    let key = archive_key(path)?;
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
        lru_insert(
            cbz_index_cache(),
            INDEX_CACHE_CAPACITY,
            key,
            Arc::clone(&arc),
        );
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

fn zip_metadata(path: &Path) -> Result<(usize, Option<Vec<u8>>), ArchiveFailure> {
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
        if is_comic_info_name(&name) && prefer_comic_info_candidate(&xml_candidate, &name) {
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

// ── CB7 (7z) ──────────────────────────────────────────────────────────

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

struct SevenzBatchOutcome {
    target_bytes: Vec<u8>,
    upcoming_captured: Vec<(usize, Vec<u8>)>,
}

fn should_stop_solid_batch(
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

fn sevenz_extract_page(
    path: &Path,
    page_index: usize,
) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
    let key = archive_key(path)?;
    let (pages, opened) = if let Some(cached) = lru_get(cb7_index_cache(), &key) {
        (cached, None)
    } else {
        let (file, archive) = open_sevenz_archive(&key.path)?;
        let sorted = Arc::new(sort_page_ids(collect_sevenz_pages(&archive)));
        lru_insert(
            cb7_index_cache(),
            INDEX_CACHE_CAPACITY,
            key.clone(),
            Arc::clone(&sorted),
        );
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

fn sevenz_metadata(path: &Path) -> Result<(usize, Option<Vec<u8>>), ArchiveFailure> {
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

// ── CBT (TAR) ─────────────────────────────────────────────────────────

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

fn tar_extract_page(
    path: &Path,
    page_index: usize,
) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
    let key = archive_key(path)?;
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
        lru_insert(
            cbt_index_cache(),
            INDEX_CACHE_CAPACITY,
            key.clone(),
            Arc::clone(&arc),
        );
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

fn tar_metadata(path: &Path) -> Result<(usize, Option<Vec<u8>>), ArchiveFailure> {
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

// ── CBR (RAR via rarchive + host CLI fallback when !is_sandboxed()) ────

enum CbrPageIndex {
    Rarchive {
        pages: Vec<usize>,
        any_encrypted: bool,
    },
    Cli {
        pages: Vec<(String, bool)>,
        any_encrypted: bool,
    },
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

enum RarFallbackPolicy {
    TryCliOnUnsupported,
    NeverCli,
}

fn map_rarchive_error(err: rarchive::Error) -> (ArchiveFailure, RarFallbackPolicy) {
    match err {
        rarchive::Error::NeedPassword | rarchive::Error::BadPassword => {
            (ArchiveFailure::Encrypted, RarFallbackPolicy::NeverCli)
        }
        rarchive::Error::OldFormat => (
            ArchiveFailure::UnsupportedOrEmpty("Legacy RAR 1.3/1.4 format is unsupported".into()),
            RarFallbackPolicy::TryCliOnUnsupported,
        ),
        rarchive::Error::Unsupported(what) => {
            if what.to_ascii_lowercase().contains("encrypt") {
                (ArchiveFailure::Encrypted, RarFallbackPolicy::NeverCli)
            } else {
                (
                    ArchiveFailure::UnsupportedOrEmpty(format!("Unsupported RAR feature: {what}")),
                    RarFallbackPolicy::TryCliOnUnsupported,
                )
            }
        }
        rarchive::Error::Corrupt(what) => (
            ArchiveFailure::Error(DocumentError::Message(format!(
                "Corrupt RAR archive: {what}"
            ))),
            RarFallbackPolicy::TryCliOnUnsupported,
        ),
        rarchive::Error::NotArchive => (
            ArchiveFailure::Error(DocumentError::Message("Not a valid RAR archive".into())),
            RarFallbackPolicy::TryCliOnUnsupported,
        ),
        rarchive::Error::BadChecksum(name) => (
            ArchiveFailure::Error(DocumentError::Message(format!(
                "RAR checksum mismatch for '{name}'"
            ))),
            RarFallbackPolicy::TryCliOnUnsupported,
        ),
        rarchive::Error::Io(io_err) => {
            let policy = if io_err.kind() == ErrorKind::NotFound {
                RarFallbackPolicy::NeverCli
            } else {
                RarFallbackPolicy::TryCliOnUnsupported
            };
            (ArchiveFailure::Error(DocumentError::Io(io_err)), policy)
        }
    }
}

struct RarBatchOutcome {
    target_bytes: Vec<u8>,
    upcoming_captured: Vec<(usize, Vec<u8>)>,
}

type RarBatchResult = Result<RarBatchOutcome, (ArchiveFailure, RarFallbackPolicy)>;
type RarExtractResult = Result<(Arc<Vec<u8>>, usize, usize), (ArchiveFailure, RarFallbackPolicy)>;

fn extract_rarchive_batch(
    archive: &rarchive::Archive,
    entry_idx: usize,
    max_bytes: u64,
    upcoming_pages: &[(usize, usize)],
) -> RarBatchResult {
    let entries = archive.entries();
    let Some(entry) = entries.get(entry_idx) else {
        return Err((
            ArchiveFailure::Error(DocumentError::Message(
                "RAR entry index out of range".to_string(),
            )),
            RarFallbackPolicy::NeverCli,
        ));
    };
    if entry.encrypted {
        return Err((ArchiveFailure::Encrypted, RarFallbackPolicy::NeverCli));
    }
    if entry.size > max_bytes {
        return Err((
            ArchiveFailure::TooLarge(format!(
                "Archive entry '{}' exceeds size limit ({} > {} bytes)",
                entry.name, entry.size, max_bytes
            )),
            RarFallbackPolicy::NeverCli,
        ));
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
                return Err((ArchiveFailure::TooLarge(msg), RarFallbackPolicy::NeverCli));
            }
            Err(map_rarchive_error(err))
        }
    }
}

fn extract_rarchive_entry(
    archive: &rarchive::Archive,
    entry_idx: usize,
    max_bytes: u64,
) -> Result<Vec<u8>, (ArchiveFailure, RarFallbackPolicy)> {
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

fn rar_extract_page(
    path: &Path,
    page_index: usize,
) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
    let key = archive_key(path)?;
    let cached_index = lru_get(cbr_index_cache(), &key);

    if let Some(ref index) = cached_index {
        let (total, any_encrypted) = match index.as_ref() {
            CbrPageIndex::Rarchive {
                pages,
                any_encrypted,
            } => (pages.len(), *any_encrypted),
            CbrPageIndex::Cli {
                pages,
                any_encrypted,
            } => (pages.len(), *any_encrypted),
        };
        if total == 0 {
            return Err(if any_encrypted {
                ArchiveFailure::Encrypted
            } else {
                empty_pages_error()
            });
        }
        let clamped = page_index.min(total - 1);
        if let Some(cached_bytes) = window_cache_get(&key, clamped) {
            return Ok((cached_bytes, clamped, total));
        }
        if let CbrPageIndex::Cli { pages, .. } = index.as_ref() {
            if !is_sandboxed()
                && let Some(cli) = find_cbr_cli()
            {
                let (chosen_name, is_enc) = &pages[clamped];
                if *is_enc {
                    return Err(ArchiveFailure::Encrypted);
                }
                let canonical = canonical_safe_archive_path(path)?;
                let bytes = Arc::new(cli.extract_single_entry(
                    &canonical,
                    chosen_name,
                    MAX_ARCHIVE_ENTRY_BYTES,
                )?);
                store_window_batch(&key, clamped, Arc::clone(&bytes), Vec::new());
                return Ok((bytes, clamped, total));
            }
            return Err(ArchiveFailure::UnsupportedOrEmpty(
                "CBR CLI helper unavailable".to_string(),
            ));
        }
    }

    let try_rarchive = || -> RarExtractResult {
        let mut archive = rarchive::Archive::open(&key.path).map_err(map_rarchive_error)?;
        archive.set_memory_budget(RARCHIVE_MEM_LIMIT_BYTES);
        let index = match cached_index {
            Some(idx) => idx,
            None => {
                let (pages, any_encrypted) = collect_rarchive_pages(&archive);
                let sorted = Arc::new(CbrPageIndex::Rarchive {
                    pages: sort_page_ids(pages),
                    any_encrypted,
                });
                lru_insert(
                    cbr_index_cache(),
                    INDEX_CACHE_CAPACITY,
                    key.clone(),
                    Arc::clone(&sorted),
                );
                sorted
            }
        };
        let CbrPageIndex::Rarchive {
            pages,
            any_encrypted,
        } = index.as_ref()
        else {
            return Err((empty_pages_error(), RarFallbackPolicy::NeverCli));
        };
        if pages.is_empty() {
            return Err((
                if *any_encrypted {
                    ArchiveFailure::Encrypted
                } else {
                    empty_pages_error()
                },
                RarFallbackPolicy::NeverCli,
            ));
        }
        let total = pages.len();
        let clamped = page_index.min(total - 1);
        if let Some(cached_bytes) = window_cache_get(&key, clamped) {
            return Ok((cached_bytes, clamped, total));
        }

        let chosen_idx = pages[clamped];
        let window_end = clamped
            .saturating_add(SOLID_UPCOMING_WINDOW_SPAN)
            .min(total);
        let upcoming_pages: Vec<(usize, usize)> =
            window_cache_uncached_pages(&key, (clamped + 1)..window_end)
                .into_iter()
                .map(|p| (p, pages[p]))
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
    };

    match try_rarchive() {
        Ok(res) => Ok(res),
        Err((failure, RarFallbackPolicy::NeverCli)) => Err(failure),
        Err((failure, RarFallbackPolicy::TryCliOnUnsupported)) => {
            if !is_sandboxed()
                && let Some(cli) = find_cbr_cli()
            {
                return cli.extract_page(path, &key, page_index);
            }
            Err(failure)
        }
    }
}

type RarMetadataResult = Result<(usize, Option<Vec<u8>>), (ArchiveFailure, RarFallbackPolicy)>;

fn rar_metadata(path: &Path) -> Result<(usize, Option<Vec<u8>>), ArchiveFailure> {
    let try_rarchive = || -> RarMetadataResult {
        let mut archive = rarchive::Archive::open(path).map_err(map_rarchive_error)?;
        archive.set_memory_budget(RARCHIVE_MEM_LIMIT_BYTES);
        let (pages, any_encrypted) = collect_rarchive_pages(&archive);
        let page_count = pages.len();
        if page_count == 0 && any_encrypted {
            return Err((ArchiveFailure::Encrypted, RarFallbackPolicy::NeverCli));
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
    };

    match try_rarchive() {
        Ok(res) => Ok(res),
        Err((failure, RarFallbackPolicy::NeverCli)) => Err(failure),
        Err((failure, RarFallbackPolicy::TryCliOnUnsupported)) => {
            if !is_sandboxed()
                && let Some(cli) = find_cbr_cli()
            {
                return cli.metadata(path);
            }
            Err(failure)
        }
    }
}

// ── Host CLI fallback for CBR (`unrar` / `7z`, never when sandboxed) ──

#[derive(Clone, Copy, Debug)]
enum CbrCliKind {
    Unrar,
    SevenZ,
}

#[derive(Clone, Debug)]
struct CbrCli {
    kind: CbrCliKind,
    path: PathBuf,
}

fn no_window_command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn path_starts_with_dash(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().first() == Some(&b'-')
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str().encode_wide().next() == Some(0x002D)
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.to_str().is_some_and(|s| s.starts_with('-'))
    }
}

fn find_cbr_cli() -> Option<&'static CbrCli> {
    if is_sandboxed() {
        return None;
    }
    CBR_CLI.get_or_init(find_cbr_cli_uncached).as_ref()
}

fn find_cbr_cli_uncached() -> Option<CbrCli> {
    if is_sandboxed() {
        return None;
    }

    #[cfg(target_os = "windows")]
    let candidates: &[(&str, CbrCliKind)] = &[
        ("unrar.exe", CbrCliKind::Unrar),
        ("7z.exe", CbrCliKind::SevenZ),
    ];
    #[cfg(not(target_os = "windows"))]
    let candidates: &[(&str, CbrCliKind)] =
        &[("unrar", CbrCliKind::Unrar), ("7z", CbrCliKind::SevenZ)];

    for &(bin_name, kind) in candidates {
        if let Some(bin_path) = locate_binary(bin_name)
            && verify_cbr_cli(kind, &bin_path)
        {
            return Some(CbrCli {
                kind,
                path: bin_path,
            });
        }
    }
    None
}

fn locate_binary(name: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    #[cfg(target_os = "windows")]
    if name.eq_ignore_ascii_case("7z.exe") {
        let pf = Path::new(r"C:\Program Files\7-Zip\7z.exe");
        if pf.is_file() {
            return Some(pf.to_path_buf());
        }
    }
    None
}

fn verify_cbr_cli(kind: CbrCliKind, bin_path: &Path) -> bool {
    let mut cmd = no_window_command(bin_path);
    match kind {
        CbrCliKind::Unrar => {
            // Non-free RARLAB `unrar` exits 0 and prints usage with no args; `unrar-free` does not
            // support `vb` or `p -inul`.
            let Ok((status, stdout, _)) =
                run_cli_capped(&mut cmd, Duration::from_secs(5), 64 * 1024)
            else {
                return false;
            };
            let text = String::from_utf8_lossy(&stdout);
            status.success() && text.contains("UNRAR") && !text.contains("unrar-free")
        }
        CbrCliKind::SevenZ => {
            cmd.arg("i");
            run_cli_capped(&mut cmd, Duration::from_secs(5), 64 * 1024)
                .is_ok_and(|(status, _, _)| status.success())
        }
    }
}

fn read_capped_from_pipe<R: Read>(
    mut reader: R,
    max_bytes: u64,
    exceeded_flag: &AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.len() as u64 + n as u64 > max_bytes {
            exceeded_flag.store(true, AtomicOrdering::Release);
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                format!("CLI output exceeded {max_bytes} byte cap"),
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

fn run_cli_capped(
    cmd: &mut Command,
    timeout: Duration,
    max_stdout: u64,
) -> Result<(ExitStatus, Vec<u8>, String), ArchiveFailure> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| DocumentError::Message("Failed to capture CLI stdout".into()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| DocumentError::Message("Failed to capture CLI stderr".into()))?;

    let exceeded = Arc::new(AtomicBool::new(false));
    let exceeded_stdout = Arc::clone(&exceeded);
    let stdout_thread =
        std::thread::spawn(move || read_capped_from_pipe(stdout, max_stdout, &exceeded_stdout));
    let stderr_thread = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        stderr
            .by_ref()
            .take(MAX_CLI_STDERR_BYTES)
            .read_to_end(&mut buf)?;
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        Ok(buf)
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        if exceeded.load(AtomicOrdering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ArchiveFailure::TooLarge(format!(
                "Archive entry output exceeded {max_stdout} bytes"
            )));
        }
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ArchiveFailure::Error(DocumentError::Message(format!(
                    "CBR helper timed out after {timeout:?}"
                ))));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };

    let stdout_bytes = stdout_thread
        .join()
        .map_err(|_| DocumentError::Message("CLI stdout reader panicked".into()))?
        .map_err(|e| {
            if exceeded.load(AtomicOrdering::Acquire) {
                ArchiveFailure::TooLarge(format!(
                    "Archive entry output exceeded {max_stdout} bytes"
                ))
            } else {
                ArchiveFailure::Error(DocumentError::Io(e))
            }
        })?;
    let stderr_bytes = stderr_thread
        .join()
        .map_err(|_| DocumentError::Message("CLI stderr reader panicked".into()))??;
    let stderr_str = String::from_utf8_lossy(&stderr_bytes).into_owned();
    Ok((status, stdout_bytes, stderr_str))
}

fn canonical_safe_archive_path(path: &Path) -> Result<PathBuf, ArchiveFailure> {
    if path_starts_with_dash(path) {
        return Err(ArchiveFailure::Error(DocumentError::Message(
            "Refusing to pass '-' prefixed path to external CLI".to_string(),
        )));
    }
    let canonical = std::fs::canonicalize(path)?;
    if path_starts_with_dash(&canonical) {
        return Err(ArchiveFailure::Error(DocumentError::Message(
            "Refusing to pass '-' prefixed canonical path to external CLI".to_string(),
        )));
    }
    Ok(canonical)
}

fn validate_cli_entry_arg(entry: &str) -> Result<(), ArchiveFailure> {
    if entry.is_empty()
        || entry.contains('\0')
        || entry.starts_with('-')
        || entry.starts_with('@')
        || entry.contains('*')
        || entry.contains('?')
    {
        return Err(ArchiveFailure::Error(DocumentError::Message(
            "Unsafe archive entry name for CLI extraction".to_string(),
        )));
    }
    Ok(())
}

fn cli_output_indicates_encrypted(stderr: &str, stdout: &[u8]) -> bool {
    let err_lower = stderr.to_ascii_lowercase();
    if err_lower.contains("password") || err_lower.contains("encrypted") {
        return true;
    }
    let out_lower = String::from_utf8_lossy(stdout).to_ascii_lowercase();
    out_lower.contains("wrong password") || out_lower.contains("encrypted")
}

#[derive(Debug)]
struct CliListedEntry {
    raw_name: String,
    is_dir: bool,
    encrypted: bool,
}

impl CbrCli {
    fn list_entries(&self, canonical_path: &Path) -> Result<Vec<CliListedEntry>, ArchiveFailure> {
        let mut cmd = no_window_command(&self.path);
        match self.kind {
            CbrCliKind::Unrar => {
                cmd.args(["vb", "-p-", "--"]).arg(canonical_path);
            }
            CbrCliKind::SevenZ => {
                cmd.args(["l", "-slt", "-p-", "-y", "--"])
                    .arg(canonical_path);
            }
        }
        let (status, stdout, stderr) =
            run_cli_capped(&mut cmd, CBR_CLI_TIMEOUT, MAX_XML_ENTRY_BYTES)?;
        if !status.success() {
            if cli_output_indicates_encrypted(&stderr, &stdout) {
                return Err(ArchiveFailure::Encrypted);
            }
            return Err(ArchiveFailure::Error(DocumentError::Message(format!(
                "CBR CLI listing failed ({status}): {}",
                stderr.trim()
            ))));
        }

        let text = String::from_utf8_lossy(&stdout);
        let entries = match self.kind {
            CbrCliKind::Unrar => text
                .lines()
                .map(|l| l.trim_end_matches('\r'))
                .filter(|l| !l.is_empty())
                .map(|l| CliListedEntry {
                    raw_name: l.to_string(),
                    is_dir: l.ends_with('/') || l.ends_with('\\'),
                    encrypted: false,
                })
                .collect(),
            CbrCliKind::SevenZ => parse_sevenz_slt_entries(&text),
        };
        Ok(entries)
    }

    fn extract_single_entry(
        &self,
        canonical_path: &Path,
        entry_name: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ArchiveFailure> {
        validate_cli_entry_arg(entry_name)?;
        let mut cmd = no_window_command(&self.path);
        match self.kind {
            CbrCliKind::Unrar => {
                cmd.args(["p", "-inul", "-p-", "--"])
                    .arg(canonical_path)
                    .arg(entry_name);
            }
            CbrCliKind::SevenZ => {
                cmd.args(["e", "-so", "-p-", "-y", "--"])
                    .arg(canonical_path)
                    .arg(entry_name);
            }
        }
        let (status, stdout, stderr) = run_cli_capped(&mut cmd, CBR_CLI_TIMEOUT, max_bytes)?;
        if !status.success() {
            if cli_output_indicates_encrypted(&stderr, &stdout) {
                return Err(ArchiveFailure::Encrypted);
            }
            return Err(ArchiveFailure::Error(DocumentError::Message(format!(
                "CBR CLI extraction failed ({status}): {}",
                stderr.trim()
            ))));
        }
        Ok(stdout)
    }

    fn extract_page(
        &self,
        path: &Path,
        key: &ArchiveKey,
        page_index: usize,
    ) -> Result<(Arc<Vec<u8>>, usize, usize), ArchiveFailure> {
        let canonical = canonical_safe_archive_path(path)?;
        let entries = self.list_entries(&canonical)?;
        let mut any_encrypted = false;
        let mut pages = Vec::new();
        for entry in entries {
            if entry.encrypted {
                any_encrypted = true;
            }
            if !entry.is_dir && is_image_entry(&entry.raw_name) {
                let sort_name = normalize_archive_name(&entry.raw_name);
                pages.push(PageEntry {
                    id: (entry.raw_name, entry.encrypted),
                    sort_name,
                });
            }
        }
        let sorted = sort_page_ids(pages);
        let cli_index = Arc::new(CbrPageIndex::Cli {
            pages: sorted,
            any_encrypted,
        });
        lru_insert(
            cbr_index_cache(),
            INDEX_CACHE_CAPACITY,
            key.clone(),
            Arc::clone(&cli_index),
        );

        let CbrPageIndex::Cli {
            pages: ref sorted,
            any_encrypted,
        } = *cli_index
        else {
            return Err(empty_pages_error());
        };
        if sorted.is_empty() {
            return Err(if any_encrypted {
                ArchiveFailure::Encrypted
            } else {
                empty_pages_error()
            });
        }
        let total = sorted.len();
        let clamped = page_index.min(total - 1);
        let (ref chosen_name, is_enc) = sorted[clamped];
        if is_enc {
            return Err(ArchiveFailure::Encrypted);
        }
        let bytes = Arc::new(self.extract_single_entry(
            &canonical,
            chosen_name,
            MAX_ARCHIVE_ENTRY_BYTES,
        )?);
        store_window_batch(key, clamped, Arc::clone(&bytes), Vec::new());
        Ok((bytes, clamped, total))
    }

    fn metadata(&self, path: &Path) -> Result<(usize, Option<Vec<u8>>), ArchiveFailure> {
        let canonical = canonical_safe_archive_path(path)?;
        let entries = self.list_entries(&canonical)?;
        let mut page_count = 0usize;
        let mut xml_candidate: Option<(String, String)> = None;
        let mut any_encrypted = false;

        for entry in entries {
            if entry.encrypted {
                any_encrypted = true;
            }
            if entry.is_dir {
                continue;
            }
            if is_image_entry(&entry.raw_name) {
                page_count += 1;
            } else if is_comic_info_name(&entry.raw_name) {
                let norm = normalize_archive_name(&entry.raw_name);
                let replace = match &xml_candidate {
                    None => true,
                    Some((_, existing)) => {
                        norm.matches('/').count() < existing.matches('/').count()
                    }
                };
                if replace {
                    xml_candidate = Some((entry.raw_name, norm));
                }
            }
        }

        if page_count == 0 && any_encrypted {
            return Err(ArchiveFailure::Encrypted);
        }

        let xml_bytes = match xml_candidate {
            Some((raw_name, _)) => self
                .extract_single_entry(&canonical, &raw_name, MAX_XML_ENTRY_BYTES)
                .ok(),
            None => None,
        };
        Ok((page_count, xml_bytes))
    }
}

fn parse_sevenz_slt_entries(text: &str) -> Vec<CliListedEntry> {
    let mut entries = Vec::new();
    let mut in_entries = false;
    let mut cur_path: Option<String> = None;
    let mut cur_dir = false;
    let mut cur_enc = false;

    let flush = |entries: &mut Vec<CliListedEntry>,
                 path: &mut Option<String>,
                 dir: &mut bool,
                 enc: &mut bool| {
        if let Some(raw_name) = path.take() {
            entries.push(CliListedEntry {
                raw_name,
                is_dir: *dir,
                encrypted: *enc,
            });
        }
        *dir = false;
        *enc = false;
    };

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if !in_entries {
            if line.starts_with("----------") {
                in_entries = true;
            }
            continue;
        }
        if line.is_empty() {
            flush(&mut entries, &mut cur_path, &mut cur_dir, &mut cur_enc);
            continue;
        }
        if let Some((key, val)) = line.split_once(" = ") {
            match key {
                "Path" => cur_path = Some(val.to_string()),
                "Folder" => cur_dir = val == "+",
                "Attributes" => {
                    if val.starts_with('D') {
                        cur_dir = true;
                    }
                }
                "Encrypted" => cur_enc = val == "+",
                _ => {}
            }
        }
    }
    flush(&mut entries, &mut cur_path, &mut cur_dir, &mut cur_enc);
    entries
}

// ── ComicInfo.xml metadata parser ─────────────────────────────────────

fn decode_xml_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c));
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c));
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn push_xml_ref(out: &mut String, r: &BytesRef<'_>) {
    if let Ok(Some(ch)) = r.resolve_char_ref() {
        out.push(ch);
    } else if let Some(s) = quick_xml::escape::resolve_xml_entity(r.as_ref()) {
        out.push_str(s);
    } else {
        out.push('&');
        out.push_str(r.as_ref());
        out.push(';');
    }
}

fn comic_info_field_name(local: &str) -> Option<&'static str> {
    match local {
        "Title" => Some("Title"),
        "Series" => Some("Series"),
        "Number" => Some("Number"),
        "Writer" => Some("Writer"),
        "Publisher" => Some("Publisher"),
        "Year" => Some("Year"),
        "Genre" => Some("Genre"),
        _ => None,
    }
}

fn parse_comic_info_xml(bytes: &[u8], fields: &mut BTreeMap<String, String>) {
    let xml = decode_xml_bytes(bytes);
    let mut reader = Reader::from_str(&xml);
    let cfg = reader.config_mut();
    cfg.check_end_names = false;
    cfg.allow_dangling_amp = true;
    cfg.allow_unmatched_ends = true;

    let mut depth: usize = 0;
    let mut comic_info_depth: Option<usize> = None;
    let mut active_field: Option<(&'static str, usize)> = None;
    let mut text_buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                depth += 1;
                let local = e.local_name().into_inner();
                if comic_info_depth.is_none() && local.eq_ignore_ascii_case("ComicInfo") {
                    comic_info_depth = Some(depth);
                } else if comic_info_depth.is_some_and(|root| depth == root + 1)
                    && let Some(key) = comic_info_field_name(local)
                {
                    active_field = Some((key, depth));
                    text_buf.clear();
                }
            }
            Ok(Event::End(e)) => {
                if let Some((key, tag_depth)) = active_field
                    && depth == tag_depth
                {
                    let trimmed = text_buf.trim();
                    if !trimmed.is_empty() {
                        fields
                            .entry(key.to_string())
                            .or_insert_with(|| trimmed.to_string());
                    }
                    active_field = None;
                    text_buf.clear();
                }
                if comic_info_depth == Some(depth)
                    && e.local_name()
                        .into_inner()
                        .eq_ignore_ascii_case("ComicInfo")
                {
                    comic_info_depth = None;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(t)) => {
                if active_field.is_some() {
                    text_buf.push_str(&t.xml10_content());
                }
            }
            Ok(Event::CData(c)) => {
                if active_field.is_some() {
                    text_buf.push_str(&c.xml10_content());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if active_field.is_some() {
                    push_xml_ref(&mut text_buf, &r);
                }
            }
            Ok(Event::Eof) => break,
            Err(quick_xml::Error::IllFormed(_)) => {}
            Err(_) => break,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufWriter, Cursor, Write};
    use std::sync::atomic::AtomicU64;

    use zip::CompressionMethod;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempTestDir(PathBuf);

    impl TempTestDir {
        fn new(prefix: &str) -> Self {
            let id = TEST_DIR_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "media-sort-comic-{prefix}-{}-{id}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_zip_entries(path: &Path, entries: &[(&str, &[u8])]) {
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

        let key = archive_key(&cbt_path).expect("tar archive key");
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

        let key = archive_key(&cbz_path).expect("cbz archive key");
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

    #[test]
    fn comic_page_window_cache_enforces_24_mib_lru_cap() {
        let mut cache = WindowCacheState::new(COMIC_PAGE_WINDOW_CACHE_MAX_BYTES);
        let key = ArchiveKey {
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

        let fwd_key = archive_key(&fwd_path).expect("fwd key");
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

        let rev_key = archive_key(&rev_path).expect("rev key");
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
