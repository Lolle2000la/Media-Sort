use std::path::Path;
use std::sync::mpsc::SyncSender;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use super::pool::{
    KeepPredicate, ThumbnailRequest, ThumbnailResult, await_response, spawn_worker_pool,
    submit_request,
};
use super::{THUMBNAIL_SIZE, ThumbnailError};

const FFMPEG_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const MPV_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);

static VIDEO_THUMBNAIL_WORKER: LazyLock<Mutex<SyncSender<ThumbnailRequest>>> =
    LazyLock::new(|| {
        let num_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 4);

        spawn_worker_pool(
            "Video thumbnail",
            num_workers,
            |i| match iced_mpv::MpvContext::new_thumbnail_player() {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::error!("Video thumbnail worker {i}: failed to create MpvContext: {e}");
                    None
                }
            },
            generate_video_thumbnail_frame,
        )
    });

// Bounded ffmpeg worker pool — sized larger (clamp 2..16) than the mpv pool
// (clamp 2..4) because ffmpeg is a short-lived lightweight subprocess (~70 ms,
// ~40 MB RSS, no persistent state) while each MpvContext holds GPU resources
// and a libmpv handle. cap=16 lets a typical visible set (~20–30 video cards)
// render in 1–2 batches on most machines while still bounding pathological
// scroll-storm fan-out.
static FFMPEG_THUMBNAIL_WORKER: LazyLock<Mutex<SyncSender<ThumbnailRequest>>> =
    LazyLock::new(|| {
        let num_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 16);

        spawn_worker_pool(
            "FFMPEG thumbnail",
            num_workers,
            |_| Some(()),
            |_, path| match media_sort_backend::media::ffmpeg_pipe::extract_frame(
                path,
                THUMBNAIL_SIZE,
                THUMBNAIL_SIZE,
            ) {
                Ok(decoded) => Ok(Some(decoded.into_parts())),
                Err(e) => Err(ThumbnailError::Decode(e)),
            },
        )
    });

fn generate_video_thumbnail_frame(
    player: &mut iced_mpv::MpvContext,
    path: &Path,
) -> ThumbnailResult {
    player
        .capture_frame(
            path,
            THUMBNAIL_SIZE,
            THUMBNAIL_SIZE,
            Duration::from_millis(1000),
        )
        .map(Some)
        .map_err(|e| ThumbnailError::Decode(e.to_string()))
}

/// Whether the path is a GIF file (case-insensitive extension check).
///
/// Animated GIFs are classified `MediaType::Video` at scan time so the
/// *preview pane* plays them through the video pipeline — but a grid
/// thumbnail is a static first-frame RGBA handle exactly like any image's,
/// and the in-process image pipeline decodes that first frame without
/// spawning an ffmpeg subprocess.
pub fn is_gif_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
}

/// Generate a thumbnail for a video file using the ffmpeg pool first,
/// falling back to the libmpv pool if ffmpeg fails, times out, or its pool
/// is unavailable.
pub fn generate_video_thumbnail(
    path: &Path,
    should_keep: Option<KeepPredicate>,
) -> ThumbnailResult {
    let response_rx = submit_request(
        &FFMPEG_THUMBNAIL_WORKER,
        path,
        should_keep.clone(),
        "ffmpeg video",
    )?;
    match await_response(response_rx, FFMPEG_RESPONSE_TIMEOUT, "ffmpeg video") {
        Ok(Some(parts)) => Ok(Some(parts)),
        Ok(None) => Ok(None),
        Err(_) => {
            if should_keep.as_ref().is_some_and(|keep| !keep(path)) {
                return Ok(None);
            }
            let mpv_rx = submit_request(&VIDEO_THUMBNAIL_WORKER, path, should_keep, "video")?;
            await_response(mpv_rx, MPV_RESPONSE_TIMEOUT, "video")
        }
    }
}
