use media_sort_core::media_type::MediaType;
use std::path::PathBuf;
use std::sync::LazyLock;

use tracing;

type ThumbnailResult = Result<(u32, u32, Vec<u8>), ThumbnailError>;
type VideoThumbnailRequest = (PathBuf, std::sync::mpsc::Sender<ThumbnailResult>);

/// Failure of a thumbnail generation request.
///
/// [`Decode`](Self::Decode) is a real file problem (corrupt data, unsupported
/// codec) and gets recorded in `media_errors` as a persistent "failed" badge.
/// [`Transient`](Self::Transient) is a queue/worker-pool problem (queue
/// timeout, dead pool, ffmpeg/mpv request timeout) — the file itself is
/// fine, so it is NOT recorded; the card just stays without a thumbnail and
/// the next `load_visible_thumbnails` pass re-queues it.
#[derive(Debug, Clone)]
pub enum ThumbnailError {
    Decode(String),
    Transient(String),
}

impl ThumbnailError {
    fn transient(message: impl Into<String>) -> Self {
        Self::Transient(message.into())
    }
}

/// Bounded work-queue capacity per pool. A scroll-storm through a folder of
/// video files used to feed unbounded `mpsc::channel` queues (and if every
/// mpv worker failed to start, requesters blocked forever on `recv()`);
/// `sync_channel` applies backpressure and the response timeouts below
/// un-hang requesters when a pool is dead.
const THUMBNAIL_QUEUE_CAPACITY: usize = 256;
const FFMPEG_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MPV_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// How long a producer may block on the bounded worker-pool queue before
/// giving up. A scroll-storm can fill the 256-slot queue; without this cap
/// a request would block indefinitely while every worker is busy.
const THUMBNAIL_QUEUE_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Bounded send with a deadline: blocks up to `timeout`, then gives up
/// (returning `Err(value)`). `SyncSender::send_timeout` is not stable on
/// the MSRV toolchain (`std_internals`), so this is a `try_send` poll loop.
/// Callers must NOT hold the pool's `Mutex` across this call — clone the
/// sender out of the lock first.
fn send_with_timeout<T>(
    sender: &std::sync::mpsc::SyncSender<T>,
    mut value: T,
    timeout: std::time::Duration,
) -> Result<(), T> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match sender.try_send(value) {
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(full_value)) => {
                if std::time::Instant::now() >= deadline {
                    return Err(full_value);
                }
                value = full_value;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(full_value)) => {
                return Err(full_value);
            }
        }
    }
}

static VIDEO_THUMBNAIL_WORKER: LazyLock<
    std::sync::Mutex<std::sync::mpsc::SyncSender<VideoThumbnailRequest>>,
> = LazyLock::new(|| {
    let (tx, rx) = std::sync::mpsc::sync_channel::<VideoThumbnailRequest>(THUMBNAIL_QUEUE_CAPACITY);
    let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
    let num_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 4);

    for i in 0..num_workers {
        let rx = rx.clone();
        std::thread::spawn(move || {
            let mut player = match iced_mpv::MpvContext::new_thumbnail_player() {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("Video thumbnail worker {i}: failed to create MpvContext: {e}");
                    return;
                }
            };

            loop {
                let request = {
                    let guard = rx.lock().unwrap();
                    guard.recv().ok()
                };
                let Some((path, response)) = request else {
                    break;
                };
                let result = generate_video_thumbnail_frame(&mut player, &path);
                let _ = response.send(result);
            }
        });
    }
    std::sync::Mutex::new(tx)
});

// Bounded ffmpeg worker pool — sized larger (clamp 2..16) than the mpv pool
// (clamp 2..4) because ffmpeg is a short-lived lightweight subprocess (~70 ms,
// ~40 MB RSS, no persistent state) while each MpvContext holds GPU resources
// and a libmpv handle. cap=16 lets a typical visible set (~20–30 video cards)
// render in 1–2 batches on most machines while still bounding pathological
// scroll-storm fan-out. Without this cap, scrolling through a folder of N
// video files can spawn N simultaneous ffmpeg processes (default tokio
// blocking pool ceiling is 512). See `benches/perf_candidates.rs` Group G.
static FFMPEG_THUMBNAIL_WORKER: LazyLock<
    std::sync::Mutex<std::sync::mpsc::SyncSender<VideoThumbnailRequest>>,
> = LazyLock::new(|| {
    let (tx, rx) = std::sync::mpsc::sync_channel::<VideoThumbnailRequest>(THUMBNAIL_QUEUE_CAPACITY);
    let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
    let num_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);

    for i in 0..num_workers {
        let rx = rx.clone();
        std::thread::spawn(move || {
            loop {
                let request = {
                    let guard = rx.lock().unwrap();
                    guard.recv().ok()
                };
                let Some((path, response)) = request else {
                    break;
                };
                let result =
                    match media_sort_backend::media::ffmpeg_pipe::extract_frame(&path, 128, 128) {
                        Ok(decoded) => Ok(decoded.into_parts()),
                        Err(e) => Err(ThumbnailError::Decode(e)),
                    };
                if response.send(result).is_err() {
                    tracing::warn!(
                        "FFMPEG thumbnail worker {i}: response channel closed for {}",
                        path.display()
                    );
                }
            }
        });
    }
    std::sync::Mutex::new(tx)
});

fn generate_video_thumbnail_frame(
    player: &mut iced_mpv::MpvContext,
    path: &std::path::Path,
) -> ThumbnailResult {
    player
        .capture_frame(path, 128, 128, std::time::Duration::from_millis(1000))
        .map_err(|e| ThumbnailError::Decode(e.to_string()))
}

/// Whether the path is a GIF file (case-insensitive extension check,
/// mirroring `state::detect_media_type`'s lowercased-extension handling).
///
/// Animated GIFs are classified `MediaType::Video` at scan time so the
/// *preview pane* plays them through the video pipeline — but a grid
/// thumbnail is a static first-frame RGBA handle exactly like any image's,
/// and the in-process image pipeline decodes that first frame without
/// spawning an ffmpeg subprocess. Routing GIFs through the video pool used
/// to cost an ffmpeg spawn per GIF card: on Windows every console-subprocess
/// spawn from the console-less GUI binary pops up a console window (see
/// `ffmpeg_pipe::ffmpeg_command`), and even elsewhere the ~70 ms subprocess
/// startup per card is pure overhead.
fn is_gif_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
}

/// Generate a thumbnail for the given file. Takes the entry's cached
/// `media_type` and `animated` (the latter `Some(_)` for GIFs, populated
/// at scan time) so the production code path no longer re-runs
/// `detect_media_type(path, false)` here — that call internally re-opens
/// / re-decodes GIF files when the extension is `.gif` to decide whether
/// they should be reclassified as `Image`. With the cached args threaded
/// through, the GIF "is_animated" check happens exactly once per scan.
///
/// `animated` is unused at dispatch today (the `media_type` arg already
/// carries the animate_gifs-driven classification) but is threaded through
/// so future callers can use it (e.g. a metadata-panel "Animated" field)
/// without reopening `is_animated_gif`.
/// The `Ok(None)` outcome means the file has no thumbnail image to show —
/// currently only an audio file without embedded cover art. That is a normal,
/// playable file, not a failure, so it is surfaced as a no-op
/// (`MediaMessage::ThumbnailCancelled`) rather than `ThumbnailError::Decode`.
pub fn generate_thumbnail(
    path: &std::path::Path,
    media_type: MediaType,
    _animated: Option<bool>,
) -> Result<Option<(u32, u32, Vec<u8>)>, ThumbnailError> {
    if media_type == MediaType::Audio {
        return media_sort_backend::media::thumbnail::generate_thumbnail(path, 128, 128)
            .map(|opt| opt.map(|d| d.into_parts()))
            .map_err(|e| ThumbnailError::Decode(format!("Audio cover thumbnail error: {e}")));
    }

    if media_type == MediaType::Video && !is_gif_path(path) {
        // ffmpeg is bundled in release packages and is ~2.4× faster than
        // the mpv poll loop. Route through a bounded worker pool
        // (`FFMPEG_THUMBNAIL_WORKER`, `available_parallelism().clamp(2, 16)`)
        // so a scroll-storm can't fan out N simultaneous ffmpeg
        // subprocesses. On ffmpeg miss / extract failure, fall back to the
        // existing mpv worker pool — same semantics as before, just with a
        // bounded upper bound on concurrency.
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        // Clone the sender and drop the mutex guard BEFORE the bounded
        // send: holding the pool's mutex across a blocked send would stall
        // every other thumbnail producer behind this request's backpressure.
        let sender = FFMPEG_THUMBNAIL_WORKER
            .lock()
            .expect("FFMPEG_THUMBNAIL_WORKER lock is not poisoned")
            .clone();
        if send_with_timeout(
            &sender,
            (path.to_path_buf(), response_tx),
            THUMBNAIL_QUEUE_SEND_TIMEOUT,
        )
        .is_err()
        {
            // Queue stayed full for 5 s or the pool is gone — the file
            // itself is fine. Report as transient so `ThumbnailFailed`
            // does NOT record it in `media_errors` (that would permanently
            // badge the card and block all retries for the session).
            return Err(ThumbnailError::transient(
                "failed to queue ffmpeg video thumbnail request",
            ));
        }
        match response_rx.recv_timeout(FFMPEG_RESPONSE_TIMEOUT) {
            Ok(Ok(parts)) => return Ok(Some(parts)),
            Ok(Err(_)) | Err(_) => {
                // ffmpeg rejected the file, timed out on it, or its pool
                // is gone (disconnected workers). The mpv pool may still
                // extract a frame, so try it before giving up.
                let (mpv_tx, mpv_rx) = std::sync::mpsc::channel();
                let sender = VIDEO_THUMBNAIL_WORKER
                    .lock()
                    .expect("VIDEO_THUMBNAIL_WORKER lock is not poisoned")
                    .clone();
                if send_with_timeout(
                    &sender,
                    (path.to_path_buf(), mpv_tx),
                    THUMBNAIL_QUEUE_SEND_TIMEOUT,
                )
                .is_err()
                {
                    return Err(ThumbnailError::transient(
                        "failed to queue video thumbnail request",
                    ));
                }
                match mpv_rx.recv_timeout(MPV_RESPONSE_TIMEOUT) {
                    Ok(Ok(parts)) => return Ok(Some(parts)),
                    Ok(Err(e)) => return Err(e),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        return Err(ThumbnailError::transient(format!(
                            "video thumbnail request timed out after {MPV_RESPONSE_TIMEOUT:?}"
                        )));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        // Pool gone: a queue failure, not a decode failure —
                        // retry later.
                        return Err(ThumbnailError::transient(
                            "video thumbnail worker pool disconnected",
                        ));
                    }
                }
            }
        }
    }

    if path.extension().and_then(|e| e.to_str()) == Some("ico") {
        return generate_ico_thumbnail(path).map(Some);
    }

    media_sort_backend::media::thumbnail::generate_thumbnail(path, 128, 128)
        .map(|opt| opt.map(|d| d.into_parts()))
        .map_err(|e| ThumbnailError::Decode(format!("Image decoding failed: {e}")))
}

fn generate_ico_thumbnail(path: &std::path::Path) -> ThumbnailResult {
    let file = std::fs::File::open(path)
        .map_err(|e| ThumbnailError::Decode(format!("Failed to open ICO file: {e}")))?;
    let icon_dir = ico::IconDir::read(file)
        .map_err(|e| ThumbnailError::Decode(format!("Failed to parse ICO structure: {e}")))?;

    let entry = icon_dir
        .entries()
        .iter()
        .filter(|e| e.width() <= 128 && e.height() <= 128)
        .max_by_key(|e| e.width())
        .or_else(|| icon_dir.entries().iter().max_by_key(|e| e.width()))
        .ok_or_else(|| {
            ThumbnailError::Decode("No valid image entries found in ICO file".to_string())
        })?;

    let decoded = entry
        .decode()
        .map_err(|e| ThumbnailError::Decode(format!("Failed to decode ICO entry: {e}")))?;
    let width = decoded.width();
    let height = decoded.height();
    let rgba = decoded.rgba_data().to_vec();

    Ok((width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_gif_path_case_insensitive() {
        assert!(is_gif_path(std::path::Path::new("/x/a.gif")));
        assert!(is_gif_path(std::path::Path::new("/x/a.GIF")));
        assert!(is_gif_path(std::path::Path::new("/x/a.Gif")));
        assert!(!is_gif_path(std::path::Path::new("/x/a.png")));
        assert!(!is_gif_path(std::path::Path::new("/x/a.gifv")));
        assert!(!is_gif_path(std::path::Path::new("/x/agif")));
        assert!(!is_gif_path(std::path::Path::new("/x/gif")));
    }

    #[test]
    fn test_generate_thumbnail_animated_gif_classified_as_video() {
        let dir = std::env::temp_dir().join("mediasort_test_animated_gif_thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("animated.gif");

        // Two-frame GIF => animated => classified `MediaType::Video` at scan
        // time (with `animate_gifs` on).
        let img1 = image::RgbaImage::from_pixel(200, 50, image::Rgba([255, 0, 0, 255]));
        let img2 = image::RgbaImage::from_pixel(200, 50, image::Rgba([0, 255, 0, 255]));
        let file = std::fs::File::create(&path).unwrap();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(file);
            encoder
                .encode(&img1, 200, 50, image::ExtendedColorType::Rgba8)
                .unwrap();
            encoder
                .encode(&img2, 200, 50, image::ExtendedColorType::Rgba8)
                .unwrap();
        }

        assert_eq!(
            media_sort_backend::media::image_decoder::is_animated_gif(&path),
            Some(true)
        );

        // The thumbnail must come from the in-process image pipeline (first
        // frame), NOT the ffmpeg/mpv video pools — this works even on a
        // machine with neither ffmpeg nor libmpv available.
        let result = generate_thumbnail(&path, MediaType::Video, Some(true));
        assert!(result.is_ok());
        let (w, h, rgba) = result.unwrap().expect("expected a thumbnail");
        // 200x50 into the 128x128 box, aspect preserved: min-ratio scaling
        // (128/200) yields 128x32.
        assert_eq!((w, h), (128, 32));
        assert_eq!(rgba.len(), (128 * 32 * 4) as usize);
        // The first frame is the red one.
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_thumbnail_valid_image() {
        let dir = std::env::temp_dir().join("mediasort_test_thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.png");

        let img = image::RgbaImage::from_pixel(32, 32, image::Rgba([255, 0, 0, 255]));
        img.save(&path).unwrap();

        let result = generate_thumbnail(&path, MediaType::Image, None);
        assert!(result.is_ok());
        let (w, h, rgba) = result.unwrap().expect("expected a thumbnail");
        assert!(w > 0 && h > 0);
        assert!(!rgba.is_empty());
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_thumbnail_ico() {
        let dir = std::env::temp_dir().join("mediasort_test_ico");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.ico");

        let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
        icon_dir.add_entry(
            ico::IconDirEntry::encode_as_png(&ico::IconImage::from_rgba_data(
                32,
                32,
                vec![0u8; 32 * 32 * 4],
            ))
            .unwrap(),
        );
        let mut file = std::fs::File::create(&path).unwrap();
        icon_dir.write(&mut file).unwrap();

        let result = generate_thumbnail(&path, MediaType::Image, None);
        assert!(result.is_ok());
        let (w, h, rgba) = result.unwrap().expect("expected a thumbnail");
        assert_eq!((w, h), (32, 32));
        assert!(!rgba.is_empty());
        assert_eq!(rgba.len(), (32 * 32 * 4) as usize);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_thumbnail_nonexistent() {
        let result = generate_thumbnail(
            &std::path::PathBuf::from("/nonexistent/image_xyz.jpg"),
            MediaType::Image,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_thumbnail_exif_orientation() {
        let dir = std::env::temp_dir().join("mediasort_test_exif_thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jpg");

        let img = image::RgbImage::from_pixel(100, 50, image::Rgb([255, 0, 0]));
        let mut jpeg_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
        img.write_to(&mut cursor, image::ImageFormat::Jpeg).unwrap();

        let exif_app1: &[u8] = &[
            0xFF, 0xE1, // APP1 marker
            0x00, 0x22, // Length of segment (34 bytes)
            0x45, 0x78, 0x69, 0x66, 0x00, 0x00, // "Exif\0\0"
            0x49, 0x49, 0x2A, 0x00, // TIFF header "II", 0x002A
            0x08, 0x00, 0x00, 0x00, // Offset to 0th IFD (8)
            0x01, 0x00, // Number of fields (1)
            0x12, 0x01, // Tag: 0x0112 (Orientation)
            0x03, 0x00, // Type: SHORT (3)
            0x01, 0x00, 0x00, 0x00, // Count: 1
            0x06, 0x00, 0x00, 0x00, // Value: 6 (rotate 90 CW)
            0x00, 0x00, 0x00, 0x00, // Offset to next IFD (0)
        ];

        let mut oriented_jpeg = Vec::new();
        oriented_jpeg.extend_from_slice(&jpeg_bytes[..2]);
        oriented_jpeg.extend_from_slice(exif_app1);
        oriented_jpeg.extend_from_slice(&jpeg_bytes[2..]);

        std::fs::write(&path, &oriented_jpeg).unwrap();

        let result = generate_thumbnail(&path, MediaType::Image, None);
        assert!(result.is_ok());
        let (w, h, rgba) = result.unwrap().expect("expected a thumbnail");
        assert!(
            h > w,
            "Expected thumbnail height {h} > width {w} due to EXIF orientation 6 (90 deg CW)"
        );
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Write a minimal, tag-less PCM WAV file (canonical 44-byte RIFF header
    /// plus silence) so the "audio without embedded cover art" path can be
    /// exercised without a bundled fixture or an ffmpeg dependency.
    fn write_silent_wav(path: &std::path::Path) {
        let sample_rate: u32 = 8000;
        let channels: u16 = 1;
        let bits: u16 = 8;
        let frames: u32 = 800; // 0.1 s
        let data_len = frames * u32::from(bits / 8) * u32::from(channels);
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits / 8);
        let block_align = channels * (bits / 8);

        let mut buf = Vec::with_capacity(44 + data_len as usize);
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(36 + data_len).to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits.to_le_bytes());
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_len.to_le_bytes());
        buf.resize(44 + data_len as usize, 128u8); // silence
        std::fs::write(path, &buf).unwrap();
    }

    #[test]
    fn test_generate_thumbnail_audio_without_cover_is_not_a_failure() {
        // Regression: a tag-less audio file used to fail thumbnail generation
        // with "the file extension `.wav` was not recognized as an image
        // format", which recorded it in `media_errors` and rendered the
        // preview pane's "file not supported" box. No cover art is not a
        // decode failure: the entry must report "no thumbnail" and stay usable.
        let dir = std::env::temp_dir().join("mediasort_test_audio_no_cover");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("silent.wav");
        write_silent_wav(&path);

        match generate_thumbnail(&path, MediaType::Audio, None) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("tag-less WAV has no cover art; expected no thumbnail"),
            Err(e) => panic!("tag-less WAV must not be a thumbnail failure: {e:?}"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
