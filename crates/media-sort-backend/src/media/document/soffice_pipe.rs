use std::collections::VecDeque;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use super::{DocumentError, MAX_PDF_FILE_BYTES, is_sandboxed, read_file_capped};

const PDF_CACHE_CAPACITY: usize = 5;
const SOFFICE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STDIO_CAPTURE_BYTES: u64 = 64 * 1024;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static SOFFICE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static SOFFICE_EXEC_LOCK: Mutex<()> = Mutex::new(());
static PDF_CACHE: OnceLock<Mutex<VecDeque<PdfCacheEntry>>> = OnceLock::new();
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, PartialEq, Eq)]
struct PdfCacheKey {
    path: PathBuf,
    file_len: u64,
    modified_nanos: u64,
}

struct PdfCacheEntry {
    key: PdfCacheKey,
    pdf_bytes: Vec<u8>,
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

/// Converts an Office document at `path` to PDF bytes via headless LibreOffice, caching recently
/// converted PDFs by `(canonical_path, file_len, modified_time)` so page flipping does not
/// re-invoke `soffice`.
///
/// Returns `Ok(None)` when LibreOffice is unavailable or the process is sandboxed.
pub fn convert_to_pdf_bytes(path: &Path) -> Result<Option<Vec<u8>>, DocumentError> {
    if is_sandboxed() {
        return Ok(None);
    }
    let Some(soffice) = find_soffice() else {
        return Ok(None);
    };

    let canonical = std::fs::canonicalize(path)?;
    let meta = std::fs::metadata(&canonical)?;
    let file_len = meta.len();
    if file_len > MAX_PDF_FILE_BYTES {
        return Err(DocumentError::Message(format!(
            "Document exceeds LibreOffice conversion limit ({file_len} > {MAX_PDF_FILE_BYTES} bytes)"
        )));
    }
    let modified_nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let key = PdfCacheKey {
        path: canonical.clone(),
        file_len,
        modified_nanos,
    };

    if let Some(cached) = cache_get(&key) {
        return Ok(Some(cached));
    }

    // Serialize headless LibreOffice invocations because concurrent runs sharing or racing on
    // profile/UNO locks can fail or deadlock.
    let _exec_guard = SOFFICE_EXEC_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if let Some(cached) = cache_get(&key) {
        return Ok(Some(cached));
    }

    let pdf_bytes = run_soffice_conversion(&soffice, &canonical)?;
    cache_insert(key, pdf_bytes.clone());
    Ok(Some(pdf_bytes))
}

fn find_soffice() -> Option<PathBuf> {
    SOFFICE_PATH.get_or_init(find_soffice_uncached).clone()
}

fn find_soffice_uncached() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let names: &[&str] = &["soffice.exe"];
    #[cfg(not(target_os = "windows"))]
    let names: &[&str] = &["soffice", "libreoffice"];

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
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

fn pdf_cache() -> &'static Mutex<VecDeque<PdfCacheEntry>> {
    PDF_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(PDF_CACHE_CAPACITY)))
}

fn cache_get(key: &PdfCacheKey) -> Option<Vec<u8>> {
    let mut cache = pdf_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pos = cache.iter().position(|entry| &entry.key == key)?;
    let entry = cache.remove(pos)?;
    let bytes = entry.pdf_bytes.clone();
    cache.push_back(entry);
    Some(bytes)
}

fn cache_insert(key: PdfCacheKey, pdf_bytes: Vec<u8>) {
    let mut cache = pdf_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(pos) = cache.iter().position(|entry| entry.key == key) {
        cache.remove(pos);
    }
    while cache.len() >= PDF_CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back(PdfCacheEntry { key, pdf_bytes });
}

fn run_soffice_conversion(soffice: &Path, canonical_src: &Path) -> Result<Vec<u8>, DocumentError> {
    let work_guard = create_unique_temp_dir()?;
    let work_dir = &work_guard.0;

    let profile_dir = work_dir.join("profile");
    let out_dir = work_dir.join("out");
    std::fs::create_dir(&profile_dir)?;
    std::fs::create_dir(&out_dir)?;

    // Copy into the isolated work dir under a fixed safe filename (`input.<ext>`). This prevents
    // leading `-` option injection in `soffice` arguments and keeps LibreOffice `.~lock.*` files
    // out of the user's sorted directory.
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

    {
        let src_file = File::open(canonical_src)?;
        let mut dst_file = File::create(&staged_input)?;
        let mut limited = src_file.take(MAX_PDF_FILE_BYTES.saturating_add(1));
        let copied = std::io::copy(&mut limited, &mut dst_file)?;
        if copied > MAX_PDF_FILE_BYTES {
            return Err(DocumentError::Message(format!(
                "Document exceeds LibreOffice size cap ({MAX_PDF_FILE_BYTES} bytes)"
            )));
        }
    }

    if path_starts_with_dash(&staged_input) || path_starts_with_dash(&out_dir) {
        return Err(DocumentError::Message(
            "Refusing to pass '-' prefixed path to LibreOffice".to_string(),
        ));
    }

    let profile_url = path_to_file_url(&profile_dir)?;

    let mut cmd = Command::new(soffice);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
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
        .arg(&staged_input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout_thread = child.stdout.take().map(spawn_capped_drain);
    let stderr_thread = child.stderr.take().map(spawn_capped_drain);

    let deadline = Instant::now() + SOFFICE_TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DocumentError::Message(format!(
                    "LibreOffice conversion timed out after {}s",
                    SOFFICE_TIMEOUT.as_secs()
                )));
            }
            None => std::thread::sleep(Duration::from_millis(15)),
        }
    };

    let stderr_bytes = stderr_thread
        .and_then(|t| t.join().ok())
        .unwrap_or_default();
    if let Some(t) = stdout_thread {
        let _ = t.join();
    }

    if !status.success() {
        let msg = String::from_utf8_lossy(&stderr_bytes);
        let detail = msg.trim();
        return Err(DocumentError::Message(if detail.is_empty() {
            format!("LibreOffice exited with status {status}")
        } else {
            format!("LibreOffice exited with status {status}: {detail}")
        }));
    }

    let expected_pdf = out_dir.join("input.pdf");
    let pdf_path = if expected_pdf.is_file() {
        expected_pdf
    } else {
        find_first_pdf_in_dir(&out_dir)?.ok_or_else(|| {
            DocumentError::Message("LibreOffice completed without producing a PDF file".to_string())
        })?
    };

    read_file_capped(&pdf_path, MAX_PDF_FILE_BYTES)
}

fn spawn_capped_drain<R: Read + Send + 'static>(reader: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.take(MAX_STDIO_CAPTURE_BYTES).read_to_end(&mut buf);
        buf
    })
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

fn path_starts_with_dash(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().first() == Some(&b'-')
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str().encode_wide().next() == Some(u16::from(b'-'))
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.to_str().is_some_and(|s| s.starts_with('-'))
    }
}

fn path_to_file_url(path: &Path) -> Result<String, DocumentError> {
    let canon = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let raw = canon.to_string_lossy();
        Ok(format_file_url(&raw, true))
    }
    #[cfg(not(windows))]
    {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            Ok(format_unix_file_url(canon.as_os_str().as_bytes()))
        }
        #[cfg(not(unix))]
        {
            let raw = canon.to_string_lossy();
            Ok(format_file_url(&raw, false))
        }
    }
}

#[cfg(unix)]
fn format_unix_file_url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity("file://".len() + bytes.len());
    out.push_str("file://");
    percent_encode_bytes(bytes, false, &mut out);
    out
}

#[cfg(any(not(unix), test))]
fn format_file_url(raw: &str, is_windows: bool) -> String {
    let mut out = String::with_capacity("file:///".len() + raw.len());
    if is_windows {
        if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
            out.push_str("file://");
            let normalized = unc.replace('\\', "/");
            percent_encode_bytes(normalized.as_bytes(), false, &mut out);
            return out;
        }
        let stripped = raw.strip_prefix(r"\\?\").unwrap_or(raw);
        let normalized = stripped.replace('\\', "/");
        if normalized.starts_with("//") {
            out.push_str("file:");
        } else if normalized.starts_with('/') {
            out.push_str("file://");
        } else {
            out.push_str("file:///");
        }
        percent_encode_bytes(normalized.as_bytes(), true, &mut out);
    } else {
        out.push_str("file://");
        if !raw.starts_with('/') {
            out.push('/');
        }
        percent_encode_bytes(raw.as_bytes(), false, &mut out);
    }
    out
}

fn percent_encode_bytes(bytes: &[u8], allow_colon: bool, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in bytes {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~')
            || (allow_colon && b == b':');
        if keep {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[(b >> 4) as usize]));
            out.push(char::from(HEX[(b & 0x0F) as usize]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_and_windows_file_urls() {
        assert_eq!(
            format_file_url("/tmp/my dir/profile", false),
            "file:///tmp/my%20dir/profile"
        );
        assert_eq!(
            format_file_url(r"\\?\C:\Users\John Doe\Temp\profile", true),
            "file:///C:/Users/John%20Doe/Temp/profile"
        );
        assert_eq!(
            format_file_url(r"\\?\UNC\server\share\profile", true),
            "file://server/share/profile"
        );
    }

    #[test]
    fn detects_leading_dash_paths() {
        assert!(path_starts_with_dash(Path::new("-flag.docx")));
        assert!(!path_starts_with_dash(Path::new("/tmp/-flag.docx")));
    }
}
