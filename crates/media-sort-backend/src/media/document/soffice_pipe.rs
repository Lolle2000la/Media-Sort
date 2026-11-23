use std::collections::HashSet;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::{DocumentError, MAX_PDF_FILE_BYTES, path_to_file_url, read_file_capped};
use crate::cache::{BoundedLru, CanonicalFileKey};
use crate::process::{
    find_executable_on_path, is_sandboxed, new_hidden_command, path_starts_with_dash,
    run_command_capped,
};

const PDF_CACHE_CAPACITY: usize = 32;
const SOFFICE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STDIO_CAPTURE_BYTES: u64 = 64 * 1024;

static RUNTIME: Mutex<Option<Arc<SofficeRuntime>>> = Mutex::new(None);
static RUNTIME_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct ManagedTempDir(PathBuf);

impl ManagedTempDir {
    fn create_unique(
        base: &Path,
        prefix: &str,
        counter: &AtomicU64,
    ) -> Result<Self, DocumentError> {
        std::fs::create_dir_all(base)?;
        let pid = std::process::id();
        for _ in 0..32 {
            let seq = counter.fetch_add(1, Ordering::Relaxed);
            let candidate = base.join(format!("{prefix}-{pid}-{seq}"));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(Self(candidate)),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(DocumentError::Io(err)),
            }
        }
        Err(DocumentError::Message(
            "Failed to create unique temporary directory for LibreOffice".to_string(),
        ))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn ensure_exists(&self) -> Result<&Path, DocumentError> {
        if !self.0.is_dir() {
            std::fs::create_dir_all(&self.0)?;
        }
        Ok(&self.0)
    }
}

impl Drop for ManagedTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Isolated staging directory holding a bounded `input.<ext>` copy of the source document.
///
/// Dropping `StagedConversion` recursively removes the staging directory and all LibreOffice
/// lock/output artifacts inside it.
pub(super) struct StagedConversion {
    work_dir: ManagedTempDir,
    staged_input: PathBuf,
}

impl StagedConversion {
    /// Stages `canonical_src` into a fresh temporary directory under `std::env::temp_dir()`.
    pub(super) fn new(canonical_src: &Path, temp_seq: &AtomicU64) -> Result<Self, DocumentError> {
        Self::new_in(&std::env::temp_dir(), canonical_src, temp_seq)
    }

    /// Stages `canonical_src` into a fresh temporary directory under `base`.
    fn new_in(
        base: &Path,
        canonical_src: &Path,
        temp_seq: &AtomicU64,
    ) -> Result<Self, DocumentError> {
        let work_dir = ManagedTempDir::create_unique(base, "media-sort-soffice", temp_seq)?;
        let staged_input = stage_input_file(canonical_src, work_dir.path())?;
        Ok(Self {
            work_dir,
            staged_input,
        })
    }

    /// Path to the staged `input.<ext>` copy inside the staging directory.
    pub(super) fn staged_input(&self) -> &Path {
        &self.staged_input
    }

    /// Returns a path inside the staging directory for a conversion artifact named `name`.
    pub(super) fn output_path(&self, name: &str) -> PathBuf {
        self.work_dir.path().join(name)
    }

    /// Creates and returns the `out/` subdirectory used as `--outdir` for headless `soffice`.
    pub(super) fn create_cli_out_dir(&self) -> Result<PathBuf, DocumentError> {
        let out_dir = self.output_path("out");
        std::fs::create_dir(&out_dir)?;
        Ok(out_dir)
    }
}

/// Removes any stale `.parentlock` before `soffice` launches and unconditionally deletes
/// `profile_dir/.parentlock` on drop (while holding `exec_lock`).
struct ProfileLockGuard<'a>(&'a Path);

impl<'a> ProfileLockGuard<'a> {
    fn new(profile_dir: &'a Path) -> Self {
        let _ = std::fs::remove_file(profile_dir.join(".parentlock"));
        Self(profile_dir)
    }
}

impl Drop for ProfileLockGuard<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.join(".parentlock"));
    }
}

struct DiskPdfCache {
    lru: BoundedLru<CanonicalFileKey, PathBuf>,
    tracked: HashSet<PathBuf>,
}

impl DiskPdfCache {
    fn new(capacity: NonZeroUsize) -> Self {
        Self {
            lru: BoundedLru::new(capacity),
            tracked: HashSet::new(),
        }
    }

    fn get(&mut self, key: &CanonicalFileKey) -> Option<PathBuf> {
        let pdf_path = self.lru.get(key)?.clone();
        if pdf_path.is_file() {
            Some(pdf_path)
        } else {
            self.lru.remove(key);
            self.tracked.remove(&pdf_path);
            None
        }
    }

    fn insert(&mut self, key: CanonicalFileKey, pdf_path: PathBuf) {
        let mut evicted_paths = Vec::new();
        if let Some(old_path) =
            self.lru
                .insert_with_evict(key, pdf_path.clone(), |_, evicted_path| {
                    evicted_paths.push(evicted_path);
                })
            && old_path != pdf_path
        {
            self.tracked.remove(&old_path);
            let _ = std::fs::remove_file(&old_path);
        }
        for evicted in evicted_paths {
            if evicted != pdf_path {
                self.tracked.remove(&evicted);
                let _ = std::fs::remove_file(&evicted);
            }
        }
        self.tracked.insert(pdf_path);
    }

    fn clear(&mut self) {
        self.lru.clear();
        for path in self.tracked.drain() {
            let _ = std::fs::remove_file(path);
        }
    }

    #[cfg(test)]
    fn contains(&self, key: &CanonicalFileKey) -> bool {
        self.lru.contains(key)
    }
}

impl Drop for DiskPdfCache {
    fn drop(&mut self) {
        self.clear();
    }
}

struct SofficeRuntime {
    soffice_path: OnceLock<Option<PathBuf>>,
    root_dir: Option<PathBuf>,
    profile_dir: OnceLock<ManagedTempDir>,
    cache_dir: OnceLock<ManagedTempDir>,
    pdf_cache: Mutex<DiskPdfCache>,
    exec_lock: Mutex<()>,
    temp_counter: AtomicU64,
    cache_file_counter: AtomicU64,
}

impl SofficeRuntime {
    fn new() -> Self {
        Self::with_root(None)
    }

    #[cfg(test)]
    fn new_in(root: PathBuf) -> Self {
        Self::with_root(Some(root))
    }

    fn with_root(root_dir: Option<PathBuf>) -> Self {
        Self {
            soffice_path: OnceLock::new(),
            root_dir,
            profile_dir: OnceLock::new(),
            cache_dir: OnceLock::new(),
            pdf_cache: Mutex::new(DiskPdfCache::new(
                NonZeroUsize::new(PDF_CACHE_CAPACITY).expect("PDF_CACHE_CAPACITY is non-zero"),
            )),
            exec_lock: Mutex::new(()),
            temp_counter: AtomicU64::new(0),
            cache_file_counter: AtomicU64::new(0),
        }
    }

    fn find_soffice(&self) -> Option<PathBuf> {
        self.soffice_path.get_or_init(find_soffice_uncached).clone()
    }

    fn persistent_profile_dir(&self) -> Result<PathBuf, DocumentError> {
        let dir = self.profile_dir.get_or_init(|| {
            let path = match &self.root_dir {
                Some(root) => root.join("profile"),
                None => {
                    let seq = RUNTIME_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
                    std::env::temp_dir().join(format!(
                        "media-sort-soffice-profile-{}-{seq}",
                        std::process::id()
                    ))
                }
            };
            let _ = std::fs::create_dir_all(&path);
            ManagedTempDir(path)
        });
        dir.ensure_exists().map(Path::to_path_buf)
    }

    fn persistent_cache_dir(&self) -> Result<PathBuf, DocumentError> {
        let dir = self.cache_dir.get_or_init(|| {
            let path = match &self.root_dir {
                Some(root) => root.join("cache"),
                None => {
                    let seq = RUNTIME_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
                    std::env::temp_dir().join(format!(
                        "media-sort-soffice-cache-{}-{seq}",
                        std::process::id()
                    ))
                }
            };
            let _ = std::fs::create_dir_all(&path);
            ManagedTempDir(path)
        });
        dir.ensure_exists().map(Path::to_path_buf)
    }

    fn cache_get(&self, key: &CanonicalFileKey) -> Option<PathBuf> {
        let mut cache = self
            .pdf_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.get(key)
    }

    fn cache_insert(&self, key: CanonicalFileKey, pdf_path: PathBuf) {
        let mut cache = self
            .pdf_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.insert(key, pdf_path);
    }

    fn persist_cached_pdf(&self, produced_pdf: &Path) -> Result<PathBuf, DocumentError> {
        let pdf_len = std::fs::metadata(produced_pdf)?.len();
        if pdf_len == 0 {
            return Err(DocumentError::Message(
                "LibreOffice produced an empty PDF file".to_string(),
            ));
        }
        if pdf_len > MAX_PDF_FILE_BYTES {
            return Err(DocumentError::Message(format!(
                "Converted PDF exceeds size limit ({pdf_len} > {MAX_PDF_FILE_BYTES} bytes)"
            )));
        }

        let cache_dir = self.persistent_cache_dir()?;
        let seq = self.cache_file_counter.fetch_add(1, Ordering::Relaxed);
        let cached_pdf = cache_dir.join(format!("doc-{seq}.pdf"));
        if std::fs::rename(produced_pdf, &cached_pdf).is_err() {
            std::fs::copy(produced_pdf, &cached_pdf)?;
        }
        Ok(cached_pdf)
    }

    fn run_soffice_conversion_on_staged(
        &self,
        soffice: &Path,
        staged: &StagedConversion,
    ) -> Result<PathBuf, DocumentError> {
        let profile_dir = self.persistent_profile_dir()?;
        let out_dir = staged.create_cli_out_dir()?;
        let staged_input = staged.staged_input();

        if path_starts_with_dash(staged_input)
            || path_starts_with_dash(&out_dir)
            || path_starts_with_dash(&profile_dir)
        {
            return Err(DocumentError::Message(
                "Refusing to pass '-' prefixed path to LibreOffice".to_string(),
            ));
        }

        let _profile_lock = ProfileLockGuard::new(&profile_dir);
        let profile_url = path_to_file_url(&profile_dir)?;

        let mut cmd = new_hidden_command(soffice);
        cmd.arg("--headless")
            .arg("--nologo")
            .arg("--nodefault")
            .arg("--nofirststartwizard")
            .arg("--norestore")
            .arg(format!("-env:UserInstallation={profile_url}"))
            .arg("--convert-to")
            .arg("pdf")
            .arg("--outdir")
            .arg(&out_dir)
            .arg(staged_input);

        let output = run_command_capped(
            &mut cmd,
            SOFFICE_TIMEOUT,
            MAX_STDIO_CAPTURE_BYTES,
            MAX_STDIO_CAPTURE_BYTES,
            "LibreOffice",
        )
        .map_err(DocumentError::Message)?;

        if !output.status.success() {
            let msg = String::from_utf8_lossy(&output.stderr);
            let detail = msg.trim();
            return Err(DocumentError::Message(if detail.is_empty() {
                format!("LibreOffice exited with status {}", output.status)
            } else {
                format!("LibreOffice exited with status {}: {detail}", output.status)
            }));
        }

        let expected_pdf = out_dir.join("input.pdf");
        let produced_pdf = if expected_pdf.is_file() {
            expected_pdf
        } else {
            find_first_pdf_in_dir(&out_dir)?.ok_or_else(|| {
                DocumentError::Message(
                    "LibreOffice completed without producing a PDF file".to_string(),
                )
            })?
        };

        self.persist_cached_pdf(&produced_pdf)
    }

    fn convert_to_pdf_path_with_staged(
        &self,
        path: &Path,
        pre_staged: Option<StagedConversion>,
    ) -> Result<Option<PathBuf>, DocumentError> {
        let Some(soffice) = self.find_soffice() else {
            return Ok(None);
        };

        let (canonical, key) = canonical_key_with_limit(path)?;

        if let Some(cached) = self.cache_get(&key) {
            return Ok(Some(cached));
        }

        // Serialize headless LibreOffice invocations because concurrent runs sharing the
        // persistent profile/UNO locks would fail or deadlock.
        let _exec_guard = self
            .exec_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(cached) = self.cache_get(&key) {
            return Ok(Some(cached));
        }

        // Defer copying the source file until after both cache checks miss.
        let staged = match pre_staged {
            Some(s) => s,
            None => match &self.root_dir {
                Some(root) => StagedConversion::new_in(root, &canonical, &self.temp_counter)?,
                None => StagedConversion::new(&canonical, &self.temp_counter)?,
            },
        };

        let pdf_path = self.run_soffice_conversion_on_staged(&soffice, &staged)?;
        self.cache_insert(key, pdf_path.clone());
        Ok(Some(pdf_path))
    }
}

impl Drop for SofficeRuntime {
    fn drop(&mut self) {
        let mut cache = self
            .pdf_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.clear();
    }
}

/// RAII session guard that shuts down the global LibreOffice runtime when dropped.
pub struct SofficeSessionGuard {
    _private: (),
}

impl Drop for SofficeSessionGuard {
    fn drop(&mut self) {
        shutdown();
    }
}

/// Initializes (if needed) and returns an RAII guard that tears down the global LibreOffice
/// runtime, cached PDFs, and temporary profile/cache directories when dropped.
pub fn session_guard() -> SofficeSessionGuard {
    let _ = runtime();
    SofficeSessionGuard { _private: () }
}

/// Tears down the global LibreOffice runtime, deleting cached PDFs and temporary profile/cache
/// directories once any in-flight conversion finishes.
pub fn shutdown() {
    let rt = {
        let mut slot = RUNTIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.take()
    };
    drop(rt);
}

fn runtime() -> Arc<SofficeRuntime> {
    let mut slot = RUNTIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    slot.get_or_insert_with(|| Arc::new(SofficeRuntime::new()))
        .clone()
}

/// Returns `true` when LibreOffice (`soffice`/`libreoffice`) is discovered on the host system and
/// the process is not running inside a Flatpak/Snap sandbox.
pub fn is_libreoffice_available() -> bool {
    if is_sandboxed() {
        return false;
    }
    find_soffice().is_some()
}

/// Discovers and caches the path to the host `soffice` / `libreoffice` executable.
pub fn find_soffice() -> Option<PathBuf> {
    runtime().find_soffice()
}

/// Converts an Office document at `path` to a cached `.pdf` file on disk via LibreOffice.
///
/// Runs headless `soffice --convert-to pdf` under `exec_lock` with a reusable process-scoped
/// `UserInstallation` profile. Maintains a 32-entry LRU disk cache keyed by
/// `(canonical_path, file_len, modified_nanos)` so thumbnail generation and preview page flipping
/// share the same converted PDF without re-invoking LibreOffice.
///
/// Returns `Ok(None)` when LibreOffice is unavailable or the process is sandboxed.
pub fn convert_to_pdf_path(path: &Path) -> Result<Option<PathBuf>, DocumentError> {
    convert_to_pdf_path_with_staged(path, None)
}

/// Internal variant of [`convert_to_pdf_path`] that can reuse an already-staged copy of `path`
/// to avoid copying large documents twice when falling back from another converter.
pub(super) fn convert_to_pdf_path_with_staged(
    path: &Path,
    pre_staged: Option<StagedConversion>,
) -> Result<Option<PathBuf>, DocumentError> {
    if is_sandboxed() {
        return Ok(None);
    }
    runtime().convert_to_pdf_path_with_staged(path, pre_staged)
}

/// Converts an Office document at `path` to PDF bytes via headless LibreOffice.
///
/// Uses [`convert_to_pdf_path`] and reads the resulting cached PDF under `MAX_PDF_FILE_BYTES`.
/// Returns `Ok(None)` when LibreOffice is unavailable or the process is sandboxed.
pub fn convert_to_pdf_bytes(path: &Path) -> Result<Option<Vec<u8>>, DocumentError> {
    let Some(pdf_path) = convert_to_pdf_path(path)? else {
        return Ok(None);
    };
    read_file_capped(&pdf_path, MAX_PDF_FILE_BYTES).map(Some)
}

fn find_soffice_uncached() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let names: &[&str] = &["soffice.exe"];
    #[cfg(not(target_os = "windows"))]
    let names: &[&str] = &["soffice", "libreoffice"];

    if let Some(found) = find_executable_on_path(names, false) {
        return Some(found);
    }

    #[cfg(target_os = "macos")]
    {
        let mac_path = PathBuf::from("/Applications/LibreOffice.app/Contents/MacOS/soffice");
        if mac_path.is_file() {
            return Some(mac_path);
        }
    }

    #[cfg(target_os = "windows")]
    {
        for env_var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(env_var) {
                let candidate = PathBuf::from(base)
                    .join("LibreOffice")
                    .join("program")
                    .join("soffice.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

fn canonical_key_with_limit(path: &Path) -> Result<(PathBuf, CanonicalFileKey), DocumentError> {
    let canonical = std::fs::canonicalize(path)?;
    let meta = std::fs::metadata(&canonical)?;
    let file_len = meta.len();
    if file_len > MAX_PDF_FILE_BYTES {
        return Err(DocumentError::Message(format!(
            "Document exceeds LibreOffice conversion limit ({file_len} > {MAX_PDF_FILE_BYTES} bytes)"
        )));
    }
    let key = CanonicalFileKey::from_canonical_and_meta(canonical.clone(), &meta);
    Ok((canonical, key))
}

fn stage_input_file(canonical_src: &Path, work_dir: &Path) -> Result<PathBuf, DocumentError> {
    // Copy into the isolated staging dir under a fixed safe filename (`input.<ext>`). This
    // prevents leading `-` option injection in `soffice` arguments and keeps LibreOffice `.~lock.*`
    // files out of the user's sorted directory.
    let safe_ext = canonical_src
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| {
            s.chars()
                .filter(char::is_ascii_alphanumeric)
                .take(16)
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "bin".to_string());
    let staged_input = work_dir.join(format!("input.{safe_ext}"));

    let src_file = File::open(canonical_src)?;
    let mut dst_file = File::create(&staged_input)?;
    let mut limited = src_file.take(MAX_PDF_FILE_BYTES.saturating_add(1));
    let copied = std::io::copy(&mut limited, &mut dst_file)?;
    if copied > MAX_PDF_FILE_BYTES {
        return Err(DocumentError::Message(format!(
            "Document exceeds LibreOffice size cap ({MAX_PDF_FILE_BYTES} bytes)"
        )));
    }

    Ok(staged_input)
}

fn find_first_pdf_in_dir(dir: &Path) -> Result<Option<PathBuf>, DocumentError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file()
            && p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(label: &str) -> ManagedTempDir {
        static TEST_SEQ: AtomicU64 = AtomicU64::new(0);
        ManagedTempDir::create_unique(
            &std::env::temp_dir(),
            &format!("ms_soffice_unit_{label}"),
            &TEST_SEQ,
        )
        .expect("create unit test root")
    }

    #[test]
    fn disk_cache_lru_evicts_oldest_and_deletes_file_and_drops_remaining() {
        let guard = test_root("cache");
        let make_key = |idx: usize| CanonicalFileKey {
            path: guard.path().join(format!("src-{idx}.docx")),
            file_len: 100 + idx as u64,
            modified_nanos: 1,
        };

        let mut paths = Vec::new();
        {
            let mut cache =
                DiskPdfCache::new(NonZeroUsize::new(2).expect("capacity must be non-zero"));

            for idx in 0..3 {
                let p = guard.path().join(format!("cached-{idx}.pdf"));
                std::fs::write(&p, b"%PDF-1.4\n").expect("write dummy pdf");
                cache.insert(make_key(idx), p.clone());
                paths.push(p);
            }

            // Capacity is 2, so index 0 was evicted and its file removed.
            assert!(!paths[0].exists());
            assert!(paths[1].exists());
            assert!(paths[2].exists());
            assert!(cache.get(&make_key(0)).is_none());
            assert_eq!(cache.get(&make_key(1)), Some(paths[1].clone()));

            // Inserting key 3 now evicts key 2 (since key 1 was just touched).
            let p3 = guard.path().join("cached-3.pdf");
            std::fs::write(&p3, b"%PDF-1.4\n").expect("write dummy pdf 3");
            cache.insert(make_key(3), p3.clone());
            assert!(paths[1].exists());
            assert!(!paths[2].exists());
            assert!(p3.exists());

            // Replacing key 1 with a new path deletes the previous file.
            let p1_new = guard.path().join("cached-1-new.pdf");
            std::fs::write(&p1_new, b"%PDF-1.4\n").expect("write dummy pdf 1 new");
            cache.insert(make_key(1), p1_new.clone());
            assert!(!paths[1].exists());
            assert!(p1_new.exists());

            // Missing cached file on disk is purged on lookup.
            std::fs::remove_file(&p3).expect("remove cached-3.pdf");
            assert!(cache.get(&make_key(3)).is_none());
            assert!(!cache.contains(&make_key(3)));

            // `p1_new` remains tracked until `cache` drops.
            assert!(p1_new.exists());
            paths.push(p1_new);
        }

        // Dropping DiskPdfCache deletes all remaining tracked PDF files.
        assert!(!paths.last().expect("p1_new").exists());
    }

    #[test]
    fn staged_conversion_cleans_up_temp_dir_on_drop() {
        let root = test_root("staged");
        let src = root.path().join("My Doc.DOCX");
        std::fs::write(&src, b"dummy docx payload").expect("write source");
        let canonical = std::fs::canonicalize(&src).expect("canonicalize");
        let seq = AtomicU64::new(0);

        let staged_dir_path;
        {
            let staged = StagedConversion::new_in(root.path(), &canonical, &seq).expect("stage");
            staged_dir_path = staged.work_dir.path().to_path_buf();
            assert!(staged_dir_path.is_dir());
            assert_eq!(
                staged.staged_input(),
                staged_dir_path.join("input.DOCX").as_path()
            );
            assert_eq!(
                std::fs::read(staged.staged_input()).unwrap(),
                b"dummy docx payload"
            );
            assert_eq!(
                staged.output_path("out.pdf"),
                staged_dir_path.join("out.pdf")
            );
            let out_dir = staged.create_cli_out_dir().expect("create out dir");
            assert!(out_dir.is_dir());
        }

        assert!(!staged_dir_path.exists());
    }

    #[test]
    fn profile_lock_guard_removes_stale_and_drops_on_early_return() {
        let root = test_root("lock");
        let lock_file = root.path().join(".parentlock");
        std::fs::write(&lock_file, b"stale").expect("write stale lock");

        let run_early = || -> Result<(), ()> {
            let _guard = ProfileLockGuard::new(root.path());
            // Stale lock is removed immediately upon constructing the guard.
            assert!(!lock_file.exists());
            // Simulate soffice writing a new `.parentlock` before failing early.
            std::fs::write(&lock_file, b"active").expect("write active lock");
            assert!(lock_file.exists());
            Err(())
        };

        assert!(run_early().is_err());
        assert!(!lock_file.exists());
    }

    #[test]
    fn soffice_runtime_drop_removes_profile_cache_and_pdfs() {
        let root = test_root("runtime");
        let profile_dir;
        let cache_dir;
        let cached_pdf;

        {
            let rt = SofficeRuntime::new_in(root.path().to_path_buf());
            profile_dir = rt.persistent_profile_dir().expect("profile dir");
            cache_dir = rt.persistent_cache_dir().expect("cache dir");
            assert!(profile_dir.is_dir());
            assert!(cache_dir.is_dir());

            let lock = profile_dir.join(".parentlock");
            std::fs::write(&lock, b"lock").unwrap();

            let produced = root.path().join("produced.pdf");
            std::fs::write(&produced, b"%PDF-1.4\n").unwrap();
            cached_pdf = rt.persist_cached_pdf(&produced).expect("persist pdf");
            assert!(cached_pdf.is_file());
            assert!(cached_pdf.starts_with(&cache_dir));

            let key = CanonicalFileKey {
                path: root.path().join("doc.docx"),
                file_len: 12,
                modified_nanos: 34,
            };
            rt.cache_insert(key.clone(), cached_pdf.clone());
            assert_eq!(rt.cache_get(&key), Some(cached_pdf.clone()));
        }

        assert!(!cached_pdf.exists());
        assert!(!cache_dir.exists());
        assert!(!profile_dir.exists());
    }
}
