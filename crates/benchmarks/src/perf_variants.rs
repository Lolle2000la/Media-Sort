//! Performance-improvement candidate variants for `benches/perf_candidates.rs`.
//! Each "baseline" mirrors a current production code path; each "_optimized"
//! (or named) variant proposes a concrete change. The bench file compares them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::GenericImageView;
use media_sort_core::media_type::MediaType;
use media_sort_core::models::MediaEntry;

// ──────────────────────────────────────────────────────────────────────
// Candidate A: cache the resolved ffmpeg binary path
// ──────────────────────────────────────────────────────────────────────

static FFMPEG_CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();

#[cfg(target_os = "windows")]
const FFMPEG_EXE_NAME: &str = "ffmpeg.exe";
#[cfg(not(target_os = "windows"))]
const FFMPEG_EXE_NAME: &str = "ffmpeg";

fn verify_ffmpeg_uncached(ffmpeg_path: &Path) -> bool {
    std::process::Command::new(ffmpeg_path)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// Inlines the pre-cache production logic (PATH-scan + `-version` verify
// spawn on every call). Production `find_ffmpeg()` now caches, so this
// baseline must remain self-contained to keep measuring the historical
// uncached cost.
pub fn ffmpeg_path_baseline() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(FFMPEG_EXE_NAME);
        if candidate.exists() && verify_ffmpeg_uncached(&candidate) {
            return Some(candidate);
        }
    }

    let path_candidate = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            let p = dir.join(FFMPEG_EXE_NAME);
            p.exists().then_some(p)
        })
    });

    if let Some(ref p) = path_candidate
        && verify_ffmpeg_uncached(p)
    {
        return path_candidate;
    }

    None
}

pub fn ffmpeg_path_cached() -> Option<PathBuf> {
    FFMPEG_CACHED
        .get_or_init(media_sort_backend::media::ffmpeg_pipe::find_ffmpeg)
        .clone()
}

pub fn extract_frame_baseline(
    path: &Path,
    max_w: u32,
    max_h: u32,
) -> Result<media_sort_backend::media::DecodedImage, String> {
    let ffmpeg = ffmpeg_path_baseline().ok_or_else(|| "ffmpeg not found".to_string())?;
    extract_frame_with_path(&ffmpeg, path, max_w, max_h)
}

pub fn extract_frame_cached_ffmpeg(
    path: &Path,
    max_w: u32,
    max_h: u32,
) -> Result<media_sort_backend::media::DecodedImage, String> {
    let ffmpeg = ffmpeg_path_cached().ok_or_else(|| "ffmpeg not found".to_string())?;
    extract_frame_with_path(&ffmpeg, path, max_w, max_h)
}

fn extract_frame_with_path(
    ffmpeg: &Path,
    path: &Path,
    max_w: u32,
    max_h: u32,
) -> Result<media_sort_backend::media::DecodedImage, String> {
    let vf = format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,setsar=1",
        max_w, max_h
    );
    let output = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &vf,
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("ffmpeg spawn: {e}"))?;
    let stderr_tail = || {
        let s = String::from_utf8_lossy(&output.stderr);
        s.trim().chars().take(200).collect::<String>()
    };
    if !output.status.success() {
        return Err(format!(
            "ffmpeg exited with {}: {}",
            output.status,
            stderr_tail()
        ));
    }
    if output.stdout.is_empty() {
        return Err(format!("ffmpeg produced empty output: {}", stderr_tail()));
    }
    let img = image::load_from_memory_with_format(&output.stdout, image::ImageFormat::Png)
        .map_err(|e| format!("png decode: {e}"))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(media_sort_backend::media::DecodedImage::new(
        w,
        h,
        rgba.into_raw(),
    ))
}

// ──────────────────────────────────────────────────────────────────────
// Candidate B: resize_rgba without the src.to_vec() copy, and with a
// thread_local Resizer reused across calls.
// ──────────────────────────────────────────────────────────────────────

pub fn resize_rgba_baseline(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, String> {
    if dst_w == 0 || dst_h == 0 {
        return Ok(Vec::new());
    }
    let src_img = image::RgbaImage::from_raw(src_w, src_h, src.to_vec())
        .ok_or_else(|| "failed to create RgbaImage from raw data".to_string())?;
    let mut dst_img = Image::new(dst_w, dst_h, PixelType::U8x4);
    let mut resizer = Resizer::new();
    resizer
        .resize(
            &src_img,
            &mut dst_img,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .map_err(|e| format!("{e}"))?;
    Ok(dst_img.into_vec())
}

pub fn resize_rgba_owned_no_copy(
    src: Vec<u8>,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, String> {
    if dst_w == 0 || dst_h == 0 {
        return Ok(Vec::new());
    }
    let src_img =
        Image::from_vec_u8(src_w, src_h, src, PixelType::U8x4).map_err(|e| format!("{e}"))?;
    let mut dst_img = Image::new(dst_w, dst_h, PixelType::U8x4);
    let mut resizer = Resizer::new();
    resizer
        .resize(
            &src_img,
            &mut dst_img,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .map_err(|e| format!("{e}"))?;
    Ok(dst_img.into_vec())
}

pub fn resize_rgba_thread_local_resizer(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, String> {
    if dst_w == 0 || dst_h == 0 {
        return Ok(Vec::new());
    }
    thread_local! {
        static RESIZER: std::cell::RefCell<Resizer> = std::cell::RefCell::new(Resizer::new());
    }
    let src_img = image::RgbaImage::from_raw(src_w, src_h, src.to_vec())
        .ok_or_else(|| "failed to create RgbaImage from raw data".to_string())?;
    let mut dst_img = Image::new(dst_w, dst_h, PixelType::U8x4);
    RESIZER.with_borrow_mut(|resizer| {
        resizer
            .resize(
                &src_img,
                &mut dst_img,
                &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
            )
            .map_err(|e| format!("{e}"))
    })?;
    Ok(dst_img.into_vec())
}

// ──────────────────────────────────────────────────────────────────────
// Candidate C: read image dimensions from headers only, without a full
// pixel decode (production decode_image_dimensions calls load_image).
// ──────────────────────────────────────────────────────────────────────

pub fn dims_baseline_full_decode(path: &Path) -> Result<(u32, u32), image::ImageError> {
    let img = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?;
    Ok(img.dimensions())
}

pub fn dims_header_only(path: &Path) -> Result<(u32, u32), image::ImageError> {
    image::ImageReader::open(path)?
        .with_guessed_format()?
        .into_dimensions()
}

pub fn dims_turbojpeg_header(path: &Path) -> Result<(u32, u32), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let mut decompressor = turbojpeg::Decompressor::new().map_err(|e| format!("init: {e}"))?;
    let header = decompressor
        .read_header(&bytes)
        .map_err(|e| format!("header: {e}"))?;
    Ok((header.width as u32, header.height as u32))
}

// ──────────────────────────────────────────────────────────────────────
// Candidate D: detect_media_type via a static HashMap lookup and an
// ASCII-lowercase stack buffer (no String allocation), instead of three
// linear slice scans with a per-call to_lowercase String.
// ──────────────────────────────────────────────────────────────────────

static EXT_MAP: OnceLock<HashMap<&'static str, MediaType>> = OnceLock::new();

fn ext_map() -> &'static HashMap<&'static str, MediaType> {
    EXT_MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for ty in MediaType::iter() {
            for ext in ty.extensions() {
                m.insert(*ext, ty);
            }
        }
        m
    })
}

fn ascii_lowercase_stack<'b>(ext: &str, buf: &'b mut [u8; 32]) -> Option<&'b str> {
    let bytes = ext.as_bytes();
    if bytes.len() > buf.len() {
        return None;
    }
    for (i, &b) in bytes.iter().enumerate() {
        buf[i] = b.to_ascii_lowercase();
    }
    Some(std::str::from_utf8(&buf[..bytes.len()]).expect("ascii-only"))
}

pub fn detect_media_type_baseline(path: &Path, _animate_gifs: bool) -> MediaType {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let ext = ext.to_lowercase();
    MediaType::iter()
        .find(|ty| ty.extensions().contains(&ext.as_str()))
        .unwrap_or(MediaType::Image)
}

pub fn detect_media_type_hashset(path: &Path, _animate_gifs: bool) -> MediaType {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return MediaType::Image,
    };
    let mut buf = [0u8; 32];
    if let Some(lower) = ascii_lowercase_stack(ext, &mut buf) {
        return ext_map().get(lower).copied().unwrap_or(MediaType::Image);
    }
    ext_map()
        .get(ext.to_lowercase().as_str())
        .copied()
        .unwrap_or(MediaType::Image)
}

// ──────────────────────────────────────────────────────────────────────
// Candidate E: filtered_entries memoization (and pre-lowercased names).
// ──────────────────────────────────────────────────────────────────────

pub fn filtered_entries_baseline<'a>(
    entries: &'a [MediaEntry],
    query: &str,
) -> Vec<&'a MediaEntry> {
    if query.is_empty() {
        return entries.iter().collect();
    }
    let query_lower = query.to_lowercase();
    entries
        .iter()
        .filter(|e| e.file_name.to_lowercase().contains(&query_lower))
        .collect()
}

pub fn filtered_entries_precomputed_lower<'a>(
    entries: &'a [MediaEntry],
    lower_names: &[String],
    query_lower: &str,
) -> Vec<&'a MediaEntry> {
    assert_eq!(entries.len(), lower_names.len());
    if query_lower.is_empty() {
        return entries.iter().collect();
    }
    entries
        .iter()
        .zip(lower_names.iter())
        .filter(|(_, name_lower)| name_lower.contains(query_lower))
        .map(|(e, _)| e)
        .collect()
}

pub fn precompute_lower_names(entries: &[MediaEntry]) -> Vec<String> {
    entries.iter().map(|e| e.file_name.to_lowercase()).collect()
}

// ──────────────────────────────────────────────────────────────────────
// Candidate F: settings.save — current (TOML serialize + fs::write every
// call) vs. serialization alone vs. a no-op dirty-flag check.
// ──────────────────────────────────────────────────────────────────────

pub fn settings_save_baseline(
    store: &mut media_sort_core::settings::store::SettingsStore,
) -> Result<(), media_sort_core::settings::store::SettingsError> {
    store.save()
}

pub fn settings_save_serialize_only(
    store: &media_sort_core::settings::store::SettingsStore,
) -> Result<String, media_sort_core::settings::store::SettingsError> {
    Ok(toml::to_string_pretty(store)?)
}

pub fn settings_save_noop_if_clean(dirty: bool) -> Result<(), &'static str> {
    if !dirty {
        return Ok(());
    }
    Err("dirty")
}

// ──────────────────────────────────────────────────────────────────────
// Candidate G: video thumbnail queue concurrency — bounded semaphore
// protects against unbounded ffmpeg subprocess fan-out under scroll
// storms. Production today spawns one `spawn_blocking`-backed
// `extract_frame` per video thumbnail with no concurrency cap; the
// proposed change mirrors the existing mpv worker pool (2–4 worker
// threads) on the ffmpeg side via a small semaphore or worker count.
// ──────────────────────────────────────────────────────────────────────

pub struct Semaphore {
    state: Mutex<usize>,
    cond: Condvar,
}

impl Semaphore {
    pub fn new(permits: usize) -> Self {
        Self {
            state: Mutex::new(permits),
            cond: Condvar::new(),
        }
    }

    pub fn acquire(&self) -> PermitGuard<'_> {
        let mut guard = self.state.lock().expect("semaphore state lock");
        while *guard == 0 {
            guard = self.cond.wait(guard).expect("semaphore cond wait");
        }
        *guard -= 1;
        PermitGuard { sem: self }
    }

    fn release(&self) {
        let mut guard = self.state.lock().expect("semaphore state lock");
        *guard += 1;
        self.cond.notify_one();
    }
}

pub struct PermitGuard<'a> {
    sem: &'a Semaphore,
}

impl Drop for PermitGuard<'_> {
    fn drop(&mut self) {
        self.sem.release();
    }
}

/// Submit `n` concurrent `extract_frame` operations against `mp4`, returning
/// `(wall_time, peak_concurrent_processes)`. `cap = None` mirrors the current
/// production spawn-blocking-per-image path (no bound on concurrent ffmpeg
/// subprocesses); `cap = Some(c)` wraps each invocation in a permit from a
/// `c`-wide semaphore so peak concurrency never exceeds `c`.
pub fn extract_frame_concurrent_burst(
    mp4: &Path,
    n: usize,
    cap: Option<usize>,
) -> (Duration, usize) {
    let peak = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let semaphore: Option<Arc<Semaphore>> = cap.map(|c| Arc::new(Semaphore::new(c)));

    let start = Instant::now();
    let handles: Vec<_> = (0..n)
        .map(|_| {
            let mp4 = mp4.to_path_buf();
            let peak = peak.clone();
            let active = active.clone();
            let sem = semaphore.clone();
            std::thread::spawn(move || {
                let _guard = sem.as_ref().map(|s| s.acquire());
                let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                let mut p = peak.load(Ordering::Relaxed);
                while cur > p {
                    match peak.compare_exchange(p, cur, Ordering::SeqCst, Ordering::Relaxed) {
                        Ok(_) => break,
                        Err(new_p) => p = new_p,
                    }
                }
                let _ = media_sort_backend::media::ffmpeg_pipe::extract_frame(&mp4, 128, 128);
                active.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .collect();

    for h in handles {
        let _ = h.join();
    }

    (start.elapsed(), peak.load(Ordering::Relaxed))
}

// ──────────────────────────────────────────────────────────────────────
// Candidate H: cache `is_animated_gif` on `MediaEntry` — the production
// path opens the file and decodes two GIF frames per call. With an
// `animated: Option<bool>` field set at scan time, the second call (in
// `prefetch::generate_thumbnail`) becomes an O(1) field read.
// ──────────────────────────────────────────────────────────────────────

pub fn ensure_gif_fixture() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("media-sort-bench-gif-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("bench_static.gif");
    if path.exists() {
        return Some(path);
    }
    let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 0, 0, 255]));
    img.save_with_format(&path, image::ImageFormat::Gif).ok()?;
    Some(path)
}

pub fn is_animated_gif_baseline(path: &Path) -> Option<bool> {
    media_sort_backend::media::image_decoder::is_animated_gif(path)
}

pub fn is_animated_gif_cached(cache: &HashMap<PathBuf, Option<bool>>, path: &Path) -> Option<bool> {
    cache.get(path).copied().flatten()
}

// ──────────────────────────────────────────────────────────────────────
// Helpers (no benches required)
// ──────────────────────────────────────────────────────────────────────

pub fn synthetic_rgba(w: u32, h: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity((w * h * 4) as usize);
    let mut x: u32 = 0;
    for _ in 0..(w * h) {
        let v = (x & 0xFF) as u8;
        buf.extend_from_slice(&[v, 255 - v, (v >> 1).wrapping_add(40), 255]);
        x = x.wrapping_add(7);
    }
    buf
}

pub fn synthetic_media_entries(n: usize) -> Vec<MediaEntry> {
    let exts = [
        "jpg", "png", "jpeg", "gif", "mp4", "mkv", "webm", "bmp", "tiff", "webp", "mp3", "flac",
        "wav", "avi", "mov", "m4v", "aac", "ogg", "opus", "m4a", "wmv", "flv", "qoi", "tga",
        "avif", "txt", "md", "csv",
    ];
    (0..n)
        .map(|i| {
            let ext = exts[i % exts.len()];
            let path = PathBuf::from(format!("/tmp/synthetic/DSC_{:05}.{}", i, ext));
            let file_name = path.file_name().unwrap().to_string_lossy().to_string();
            let media_type = match ext {
                "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "gif" => {
                    MediaType::Video
                }
                "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" | "opus" | "wma" | "aiff" => {
                    MediaType::Audio
                }
                _ if MediaType::Document.extensions().contains(&ext) => MediaType::Document,
                _ => MediaType::Image,
            };
            MediaEntry {
                path,
                media_type,
                file_name,
                animated: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_baseline_and_hashset_agree_on_common_exts() {
        for ext in [
            "jpg",
            "JPG",
            "Png",
            "PNG",
            "mp4",
            "MP4",
            "gif",
            "GIF",
            "mp3",
            "Mp3",
            "webp",
            "tiff",
            "bmp",
            "svg",
            "SVG",
            "svgz",
            "SVGZ",
            "txt",
            "TXT",
            "md",
            "csv",
            "unknown_ext",
            "this_is_not_a_real_extension",
        ] {
            let p = PathBuf::from(format!("file.{}", ext));
            let a = detect_media_type_baseline(&p, true);
            let b = detect_media_type_hashset(&p, true);
            assert_eq!(a, b, "ext={}", ext);
        }
        assert_eq!(
            detect_media_type_hashset(Path::new("notes.txt"), true),
            MediaType::Document
        );
        assert_eq!(
            detect_media_type_hashset(Path::new("readme.md"), true),
            MediaType::Document
        );
        assert_eq!(
            detect_media_type_hashset(Path::new("icon.svg"), true),
            MediaType::Image
        );
        assert_eq!(
            detect_media_type_hashset(Path::new("icon.svgz"), true),
            MediaType::Image
        );
    }

    #[test]
    fn detect_handles_long_extension_via_fallback() {
        let long = "a".repeat(64);
        let p = PathBuf::from(format!("file.{}", long));
        let a = detect_media_type_baseline(&p, true);
        let b = detect_media_type_hashset(&p, true);
        assert_eq!(a, b);
    }

    #[test]
    fn filtered_baseline_and_precomputed_agree() {
        let entries = synthetic_media_entries(200);
        let lower = precompute_lower_names(&entries);
        for query in [
            "",
            "dsc",
            "00123",
            ".JPG",
            "nonexistent",
            "img_",
            "DSC_",
            "5.png",
            "abc",
        ] {
            let a = filtered_entries_baseline(&entries, query);
            let lower_q = query.to_lowercase();
            let b = filtered_entries_precomputed_lower(&entries, &lower, &lower_q);
            let a_names: Vec<_> = a.iter().map(|e| &e.file_name).collect();
            let b_names: Vec<_> = b.iter().map(|e| &e.file_name).collect();
            assert_eq!(a_names, b_names, "query={}", query);
        }
    }

    // Evidence that the Group G benches actually exercise the
    // peak-concurrency difference. Skips gracefully when ffmpeg or the
    // mp4 fixture isn't available. Failing cases print the actual peaks to
    // stderr for diagnostic.
    #[test]
    fn g_burst_peak_concurrency_differs() {
        let mp4 = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/MockState/mock 3.mp4"
        ));
        if !mp4.exists() {
            eprintln!("g_burst test: mock 3.mp4 absent — skipping");
            return;
        }
        if media_sort_backend::media::ffmpeg_pipe::find_ffmpeg().is_none() {
            eprintln!("g_burst test: ffmpeg absent — skipping");
            return;
        }

        let (_, peak_unbounded) = extract_frame_concurrent_burst(mp4, 8, None);
        let (_, peak_bounded_4) = extract_frame_concurrent_burst(mp4, 8, Some(4));
        let (_, peak_bounded_2) = extract_frame_concurrent_burst(mp4, 8, Some(2));

        eprintln!(
            "g_burst peak: unbounded={}, bounded_4={}, bounded_2={}",
            peak_unbounded, peak_bounded_4, peak_bounded_2
        );

        // The bounded semaphore must cap peak concurrency exactly.
        assert_eq!(
            peak_bounded_4, 4,
            "bounded-to-4 must cap peak at 4, got {}",
            peak_bounded_4
        );
        assert_eq!(
            peak_bounded_2, 2,
            "bounded-to-2 must cap peak at 2, got {}",
            peak_bounded_2
        );
        // Unbounded must reach strictly higher peak than the 4-cap variant
        // (otherwise the cap has no effect to evaluate).
        assert!(
            peak_unbounded > peak_bounded_4,
            "unbounded peak ({}) must exceed bounded-4 peak ({})",
            peak_unbounded,
            peak_bounded_4
        );
    }

    // Smoke-test the GIF fixture used by Group H so the bench body doesn't
    // silently skip when the fixture write fails.
    #[test]
    fn h_gif_fixture_writes_and_decodes() {
        let Some(path) = ensure_gif_fixture() else {
            eprintln!("h_gif_fixture test: could not create fixture — skipping");
            return;
        };
        assert!(path.exists(), "fixture should exist after write");
        let animated = is_animated_gif_baseline(&path);
        assert_eq!(animated, Some(false), "single-frame GIF is not animated");
    }
}
