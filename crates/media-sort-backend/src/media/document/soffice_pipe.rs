use std::collections::VecDeque;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::{
    CanonicalFileKey, DocumentError, MAX_PDF_FILE_BYTES, find_executable_on_path, is_sandboxed,
    lok_worker, new_hidden_command, path_starts_with_dash, path_to_file_url, read_file_capped,
    run_command_capped,
};

const PDF_CACHE_CAPACITY: usize = 32;
const THUMB_PDF_CACHE_CAPACITY: usize = 64;
const SOFFICE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STDIO_CAPTURE_BYTES: u64 = 64 * 1024;

static SOFFICE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static SOFFICE_PROFILE_DIR: OnceLock<PathBuf> = OnceLock::new();
static SOFFICE_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
static SOFFICE_EXEC_LOCK: Mutex<()> = Mutex::new(());
static PDF_CACHE: OnceLock<Mutex<VecDeque<PdfCacheEntry>>> = OnceLock::new();
static THUMB_PDF_CACHE: OnceLock<Mutex<VecDeque<PdfCacheEntry>>> = OnceLock::new();
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static CACHE_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

type PdfCacheKey = CanonicalFileKey;

struct PdfCacheEntry {
    key: PdfCacheKey,
    pdf_path: PathBuf,
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Returns `true` when LibreOffice (`soffice`/`libreoffice`) is discovered on the host system and
/// the process is not running inside a Flatpak/Snap sandbox.
pub fn is_libreoffice_available() -> bool {
    if is_sandboxed() {
        return false;
    }
    find_soffice().is_some()
}

/// Converts an Office document at `path` to a cached `.pdf` file on disk via LibreOffice.
///
/// Prefers the persistent LibreOfficeKit worker subprocess (`Priority::High`) when registered and
/// falls back to headless `soffice --convert-to pdf` under `SOFFICE_EXEC_LOCK` with a reusable
/// process-scoped `UserInstallation` profile. Maintains a 32-entry LRU disk cache keyed by
/// `(canonical_path, file_len, modified_nanos)` so thumbnail generation and preview page flipping
/// share the same converted PDF without re-invoking LibreOffice.
///
/// Returns `Ok(None)` when LibreOffice is unavailable or the process is sandboxed.
pub fn convert_to_pdf_path(path: &Path) -> Result<Option<PathBuf>, DocumentError> {
    if is_sandboxed() {
        return Ok(None);
    }
    let Some(soffice) = find_soffice() else {
        return Ok(None);
    };

    let (canonical, key) = canonical_key_with_limit(path)?;

    if let Some(cached) = cache_get(&key) {
        return Ok(Some(cached));
    }

    let work_guard = create_unique_temp_dir()?;
    let staged_input = stage_input_file(&canonical, &work_guard.0)?;
    let lok_out = work_guard.0.join("lok-out.pdf");

    match lok_worker::convert_to_pdf(&staged_input, &lok_out, None, lok_worker::Priority::High) {
        lok_worker::LokOutcome::Ok => match persist_cached_pdf(&lok_out, "doc") {
            Ok(cached_pdf) => {
                cache_insert(key, cached_pdf.clone());
                return Ok(Some(cached_pdf));
            }
            Err(err) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %err,
                    "LOK produced invalid PDF output; falling back to soffice CLI"
                );
            }
        },
        lok_worker::LokOutcome::Fallback(reason) => {
            tracing::debug!(
                path = %path.display(),
                reason = %reason,
                "LOK conversion fell back to soffice CLI"
            );
        }
    }

    // Serialize headless LibreOffice invocations because concurrent runs sharing the persistent
    // profile/UNO locks would fail or deadlock.
    let _exec_guard = SOFFICE_EXEC_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if let Some(cached) = cache_get(&key) {
        return Ok(Some(cached));
    }

    let pdf_path = run_soffice_conversion_on_staged(&soffice, &staged_input, &work_guard.0)?;
    cache_insert(key, pdf_path.clone());
    Ok(Some(pdf_path))
}

/// Converts page 1 of an Office document at `path` to a cached `.pdf` file for thumbnail
/// rendering.
///
/// Reuses an existing full-document entry in `PDF_CACHE` or a 64-entry `THUMB_PDF_CACHE` hit
/// before requesting a low-priority `PageRange="1"` conversion from the LibreOfficeKit worker.
/// Falls back to [`convert_to_pdf_path`] when LOK is unavailable or fails.
pub fn convert_first_page_to_pdf_path(path: &Path) -> Result<Option<PathBuf>, DocumentError> {
    if is_sandboxed() {
        return Ok(None);
    }
    if find_soffice().is_none() {
        return Ok(None);
    }

    let (canonical, key) = canonical_key_with_limit(path)?;

    if let Some(cached_full) = cache_get(&key) {
        return Ok(Some(cached_full));
    }
    if let Some(cached_thumb) = thumb_cache_get(&key) {
        return Ok(Some(cached_thumb));
    }

    {
        let work_guard = create_unique_temp_dir()?;
        let staged_input = stage_input_file(&canonical, &work_guard.0)?;
        let lok_out = work_guard.0.join("lok-thumb.pdf");

        match lok_worker::convert_to_pdf(
            &staged_input,
            &lok_out,
            Some("1"),
            lok_worker::Priority::Low,
        ) {
            lok_worker::LokOutcome::Ok => match persist_cached_pdf(&lok_out, "thumb") {
                Ok(cached_pdf) => {
                    thumb_cache_insert(key, cached_pdf.clone());
                    return Ok(Some(cached_pdf));
                }
                Err(err) => {
                    tracing::debug!(
                        path = %path.display(),
                        error = %err,
                        "LOK produced invalid first-page PDF; falling back to full conversion"
                    );
                }
            },
            lok_worker::LokOutcome::Fallback(reason) => {
                tracing::debug!(
                    path = %path.display(),
                    reason = %reason,
                    "LOK first-page conversion fell back to full soffice conversion"
                );
            }
        }
    }

    convert_to_pdf_path(path)
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

pub(super) fn find_soffice() -> Option<PathBuf> {
    SOFFICE_PATH.get_or_init(find_soffice_uncached).clone()
}

fn find_soffice_uncached() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let names: &[&str] = &["soffice.exe"];
    #[cfg(not(target_os = "windows"))]
    let names: &[&str] = &["soffice", "libreoffice"];

    if let Some(found) = find_executable_on_path(names) {
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

fn persistent_profile_dir() -> Result<PathBuf, DocumentError> {
    let dir = SOFFICE_PROFILE_DIR.get_or_init(|| {
        let path =
            std::env::temp_dir().join(format!("media-sort-soffice-profile-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&path);
        path
    });
    if !dir.is_dir() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(dir.clone())
}

fn persistent_cache_dir() -> Result<PathBuf, DocumentError> {
    let dir = SOFFICE_CACHE_DIR.get_or_init(|| {
        let path =
            std::env::temp_dir().join(format!("media-sort-soffice-cache-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&path);
        path
    });
    if !dir.is_dir() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(dir.clone())
}

fn pdf_cache() -> &'static Mutex<VecDeque<PdfCacheEntry>> {
    PDF_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(PDF_CACHE_CAPACITY)))
}

fn thumb_pdf_cache() -> &'static Mutex<VecDeque<PdfCacheEntry>> {
    THUMB_PDF_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(THUMB_PDF_CACHE_CAPACITY)))
}

fn cache_get(key: &PdfCacheKey) -> Option<PathBuf> {
    let mut cache = pdf_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache_get_from(&mut cache, key)
}

fn thumb_cache_get(key: &PdfCacheKey) -> Option<PathBuf> {
    let mut cache = thumb_pdf_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache_get_from(&mut cache, key)
}

fn cache_get_from(cache: &mut VecDeque<PdfCacheEntry>, key: &PdfCacheKey) -> Option<PathBuf> {
    let pos = cache.iter().position(|entry| &entry.key == key)?;
    let entry = cache.remove(pos)?;
    if entry.pdf_path.is_file() {
        let path = entry.pdf_path.clone();
        cache.push_back(entry);
        Some(path)
    } else {
        None
    }
}

fn cache_insert(key: PdfCacheKey, pdf_path: PathBuf) {
    let mut cache = pdf_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache_insert_into(&mut cache, key, pdf_path, PDF_CACHE_CAPACITY);
}

fn thumb_cache_insert(key: PdfCacheKey, pdf_path: PathBuf) {
    let mut cache = thumb_pdf_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache_insert_into(&mut cache, key, pdf_path, THUMB_PDF_CACHE_CAPACITY);
}

fn canonical_key_with_limit(path: &Path) -> Result<(PathBuf, PdfCacheKey), DocumentError> {
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

fn persist_cached_pdf(produced_pdf: &Path, prefix: &str) -> Result<PathBuf, DocumentError> {
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

    let cache_dir = persistent_cache_dir()?;
    let seq = CACHE_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let cached_pdf = cache_dir.join(format!("{prefix}-{seq}.pdf"));
    if std::fs::rename(produced_pdf, &cached_pdf).is_err() {
        std::fs::copy(produced_pdf, &cached_pdf)?;
    }
    Ok(cached_pdf)
}

fn cache_insert_into(
    cache: &mut VecDeque<PdfCacheEntry>,
    key: PdfCacheKey,
    pdf_path: PathBuf,
    capacity: usize,
) {
    if let Some(pos) = cache.iter().position(|entry| entry.key == key)
        && let Some(old) = cache.remove(pos)
        && old.pdf_path != pdf_path
    {
        let _ = std::fs::remove_file(&old.pdf_path);
    }
    while cache.len() >= capacity {
        if let Some(evicted) = cache.pop_front() {
            let _ = std::fs::remove_file(&evicted.pdf_path);
        }
    }
    cache.push_back(PdfCacheEntry { key, pdf_path });
}

fn run_soffice_conversion_on_staged(
    soffice: &Path,
    staged_input: &Path,
    work_dir: &Path,
) -> Result<PathBuf, DocumentError> {
    let profile_dir = persistent_profile_dir()?;

    let out_dir = work_dir.join("out");
    std::fs::create_dir(&out_dir)?;

    if path_starts_with_dash(staged_input)
        || path_starts_with_dash(&out_dir)
        || path_starts_with_dash(&profile_dir)
    {
        return Err(DocumentError::Message(
            "Refusing to pass '-' prefixed path to LibreOffice".to_string(),
        ));
    }

    // Remove any stale lock left by a previously killed soffice invocation (safe under
    // SOFFICE_EXEC_LOCK because the profile directory is process-scoped).
    let _ = std::fs::remove_file(profile_dir.join(".parentlock"));

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

    let output = match run_command_capped(
        &mut cmd,
        SOFFICE_TIMEOUT,
        MAX_STDIO_CAPTURE_BYTES,
        MAX_STDIO_CAPTURE_BYTES,
        "LibreOffice",
    ) {
        Ok(out) => out,
        Err(err) => {
            let _ = std::fs::remove_file(profile_dir.join(".parentlock"));
            return Err(err);
        }
    };

    if !output.status.success() {
        let _ = std::fs::remove_file(profile_dir.join(".parentlock"));
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
            DocumentError::Message("LibreOffice completed without producing a PDF file".to_string())
        })?
    };

    persist_cached_pdf(&produced_pdf, "doc")
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

fn create_unique_temp_dir() -> Result<TempDirGuard, DocumentError> {
    let base = std::env::temp_dir();
    std::fs::create_dir_all(&base)?;
    let pid = std::process::id();
    for _ in 0..32 {
        let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = base.join(format!("media-sort-soffice-{pid}-{seq}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(TempDirGuard(candidate)),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(DocumentError::Io(err)),
        }
    }
    Err(DocumentError::Message(
        "Failed to create unique temporary directory for LibreOffice".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_cache_lru_evicts_oldest_and_deletes_file() {
        let guard = create_unique_temp_dir().expect("create temp test dir");
        let mut cache = VecDeque::new();

        let make_key = |idx: usize| PdfCacheKey {
            path: guard.0.join(format!("src-{idx}.docx")),
            file_len: 100 + idx as u64,
            modified_nanos: 1,
        };

        let mut paths = Vec::new();
        for idx in 0..3 {
            let p = guard.0.join(format!("cached-{idx}.pdf"));
            std::fs::write(&p, b"%PDF-1.4\n").expect("write dummy pdf");
            cache_insert_into(&mut cache, make_key(idx), p.clone(), 2);
            paths.push(p);
        }

        // Capacity is 2, so index 0 was evicted and its file removed.
        assert!(!paths[0].exists());
        assert!(paths[1].exists());
        assert!(paths[2].exists());
        assert!(cache_get_from(&mut cache, &make_key(0)).is_none());
        assert_eq!(
            cache_get_from(&mut cache, &make_key(1)),
            Some(paths[1].clone())
        );

        // Inserting key 3 now evicts key 2 (since key 1 was just touched).
        let p3 = guard.0.join("cached-3.pdf");
        std::fs::write(&p3, b"%PDF-1.4\n").expect("write dummy pdf 3");
        cache_insert_into(&mut cache, make_key(3), p3.clone(), 2);
        assert!(paths[1].exists());
        assert!(!paths[2].exists());
        assert!(p3.exists());
    }
}
