use crate::mpv_context::MpvContext;
use crate::rotation::Rotation;
use libmpv_sys::*;
use std::ffi::c_char;
use std::os::raw::c_void;
use std::path::PathBuf;

/// Configuration for the background playback worker ([`start_video_worker`]).
#[derive(Debug, Clone)]
pub struct PlayerConfig {
    /// Maximum rendered frame width in pixels. Larger videos are scaled down
    /// to fit this box; the render size is the video's aspect-fitted size.
    /// Only applied when [`Self::hardware_decoding`] is `false`.
    pub max_frame_width: u32,
    /// Maximum rendered frame height in pixels.
    /// Only applied when [`Self::hardware_decoding`] is `false`.
    pub max_frame_height: u32,
    /// Whether the worker enables mpv hardware decoding (`hwdec=auto-copy`).
    /// When `true`, frames are rendered at the video's native resolution
    /// (mpv defaults — no downscaling). When `false`, software decoding
    /// (`hwdec=no`) is used and frames are capped at
    /// [`Self::max_frame_width`]/[`Self::max_frame_height`].
    pub hardware_decoding: bool,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            max_frame_width: 960,
            max_frame_height: 540,
            hardware_decoding: true,
        }
    }
}

/// Commands sent to the background video worker.
#[derive(Debug, Clone)]
pub enum VideoCommand {
    /// Load a file into the player and start playback.
    Load(PathBuf),
    Play,
    Pause,
    TogglePause,
    /// Relative seek (seconds, may be negative).
    Seek(f64),
    /// Absolute seek (seconds from the start).
    SeekAbsolute(f64),
    SetMute(bool),
    SetVolume(f64),
    Stop,
    /// Pause and release the current file handle so OS-level operations
    /// (rename/move/delete) are not blocked.
    Deactivate,
}

/// Events emitted by the background video worker.
#[derive(Debug, Clone)]
pub enum VideoEvent {
    /// A freshly rendered frame.
    ///
    /// The pixel data is **always unrotated**: mpv renders with
    /// `video-rotate=no`, and the video's rotation is reported separately in
    /// `rotation`. The crate's iced shader applies `rotation` automatically
    /// at draw time; consumers that want the raw pixels simply ignore the
    /// field.
    FrameReady {
        path: PathBuf,
        width: u32,
        height: u32,
        rotation: Rotation,
        rgba: std::sync::Arc<Vec<u8>>,
    },
    PlaybackProgress {
        position: f64,
        duration: f64,
    },
    Muted(bool),
    Volume(f64),
    Paused(bool),
    LoadFailed {
        path: PathBuf,
        error: String,
    },
}

/// Rotates raw RGBA bytes by the given [`Rotation`].
///
/// Returns `(new_width, new_height, new_rgba_bytes)`. `src` must hold a
/// complete `src_w × src_h × 4` frame for **every** rotation, including
/// [`Rotation::R0`]: the front guard rejects an undersized source before any
/// rotation is applied, so R0 is not a pure passthrough and yields an empty
/// buffer too. Degenerate or hostile dimensions (zero, or large enough to
/// overflow the u32 size math) also yield an empty buffer instead of a panic.
pub fn rotate_rgba(src_w: u32, src_h: u32, src: &[u8], rotation: Rotation) -> (u32, u32, Vec<u8>) {
    use rayon::prelude::*;
    let (dst_w, dst_h) = match rotation {
        Rotation::R90 | Rotation::R270 => (src_h, src_w),
        Rotation::R0 | Rotation::R180 => (src_w, src_h),
    };
    // Zero dimensions would make `par_chunks_exact_mut` panic on a zero
    // chunk size; hostile dimensions would overflow the u32 math below.
    if dst_w == 0 || dst_h == 0 {
        return (0, 0, Vec::new());
    }
    // A caller passing dimensions much larger than the source slice would
    // otherwise allocate dst_w*dst_h*4 bytes of zeros. Require the source
    // to actually hold a full frame before allocating.
    let Some(src_size) = (src_w as u64)
        .checked_mul(src_h as u64)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok())
    else {
        return (0, 0, Vec::new());
    };
    if src.len() < src_size {
        return (0, 0, Vec::new());
    }
    // The common no-rotation case must not pay for the dst allocation,
    // which is only needed for the in-place rotated copies below. Slice to
    // the validated frame size so a source buffer with an unused tail (e.g.
    // a pooled buffer) does not leak into the result.
    if rotation == Rotation::R0 {
        return (src_w, src_h, src[..src_size].to_vec());
    }
    let Some(dst_size) = (dst_w as u64)
        .checked_mul(dst_h as u64)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok())
    else {
        return (0, 0, Vec::new());
    };
    let mut dst = vec![0u8; dst_size];
    let dst_stride = (dst_w as usize) * 4;

    // `src_idx` uses saturating math: with hostile dims the u32 product can
    // overflow; the `<= src.len()` guard below then drops the read.
    macro_rules! src_idx {
        ($y:expr, $x:expr) => {
            ($y as usize)
                .saturating_mul(src_w as usize)
                .saturating_add($x as usize)
                .saturating_mul(4)
        };
    }

    match rotation {
        Rotation::R90 => {
            dst.par_chunks_exact_mut(dst_stride)
                .enumerate()
                .for_each(|(dst_y, row)| {
                    let src_x = dst_y as u32;
                    row.as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .enumerate()
                        .for_each(|(dst_x, pixel)| {
                            let src_y = src_h - 1 - dst_x as u32;
                            let src_idx = src_idx!(src_y, src_x);
                            if src_idx.saturating_add(4) <= src.len() {
                                pixel.copy_from_slice(&src[src_idx..src_idx + 4]);
                            }
                        });
                });

            (dst_w, dst_h, dst)
        }
        Rotation::R180 => {
            dst.par_chunks_exact_mut(dst_stride)
                .enumerate()
                .for_each(|(dst_y, row)| {
                    let src_y = src_h - 1 - dst_y as u32;
                    row.as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .enumerate()
                        .for_each(|(dst_x, pixel)| {
                            let src_x = src_w - 1 - dst_x as u32;
                            let src_idx = src_idx!(src_y, src_x);
                            if src_idx.saturating_add(4) <= src.len() {
                                pixel.copy_from_slice(&src[src_idx..src_idx + 4]);
                            }
                        });
                });

            (dst_w, dst_h, dst)
        }
        Rotation::R270 => {
            dst.par_chunks_exact_mut(dst_stride)
                .enumerate()
                .for_each(|(dst_y, row)| {
                    let src_x = src_w - 1 - dst_y as u32;
                    row.as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .enumerate()
                        .for_each(|(dst_x, pixel)| {
                            let src_y = dst_x as u32;
                            let src_idx = src_idx!(src_y, src_x);
                            if src_idx.saturating_add(4) <= src.len() {
                                pixel.copy_from_slice(&src[src_idx..src_idx + 4]);
                            }
                        });
                });

            (dst_w, dst_h, dst)
        }
        Rotation::R0 => unreachable!("handled by the early return above"),
    }
}

/// Computes the byte size of a `w × h` RGBA frame (`w * h * 4`) with checked
/// math. Returns `None` when the product overflows `usize`.
///
/// All allocation sites in the render path use this helper: an unchecked
/// `i32`/`u32` product would panic in debug builds and wrap in release, and a
/// wrapped value `as usize` becomes a giant length that drives a huge
/// allocation via `Vec::resize`/`vec![0u8; len]`.
pub(crate) fn rgba_frame_size(w: u32, h: u32) -> Option<usize> {
    (w as u64)
        .checked_mul(h as u64)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok())
}

/// The render scale for a video of effective (rotation-corrected) size
/// `eff_w × eff_h` under `config`.
///
/// With hardware decoding enabled, frames are rendered at native resolution
/// (mpv defaults): scale `1.0`. With hardware decoding disabled, the frame is
/// aspect-fitted into the configured `max_frame_width × max_frame_height`
/// box, never upscaled (`scale <= 1.0`).
pub(crate) fn render_scale(eff_w: i64, eff_h: i64, config: &PlayerConfig) -> f64 {
    if config.hardware_decoding {
        return 1.0;
    }
    (config.max_frame_width as f64 / eff_w as f64)
        .min(config.max_frame_height as f64 / eff_h as f64)
        .min(1.0)
}

/// Spawns the background video worker on the current tokio runtime with the
/// default [`PlayerConfig`].
///
/// The worker owns an [`MpvContext`], applies incoming [`VideoCommand`]s and
/// emits [`VideoEvent`]s (rendered frames, playback progress, ...) on
/// `event_tx`. The worker exits when the command channel is closed.
pub fn start_video_worker(
    cmd_rx: tokio::sync::mpsc::Receiver<VideoCommand>,
    event_tx: tokio::sync::mpsc::Sender<VideoEvent>,
) {
    start_video_worker_with(cmd_rx, event_tx, PlayerConfig::default());
}

/// Like [`start_video_worker`], with a custom [`PlayerConfig`].
pub fn start_video_worker_with(
    cmd_rx: tokio::sync::mpsc::Receiver<VideoCommand>,
    event_tx: tokio::sync::mpsc::Sender<VideoEvent>,
    config: PlayerConfig,
) {
    tokio::spawn(run_video_worker(cmd_rx, event_tx, config));
}

/// The worker loop. See [`start_video_worker`] for the public entry point.
async fn run_video_worker(
    mut cmd_rx: tokio::sync::mpsc::Receiver<VideoCommand>,
    event_tx: tokio::sync::mpsc::Sender<VideoEvent>,
    config: PlayerConfig,
) {
    let mut player = match MpvContext::new_with_hardware_decoding(config.hardware_decoding) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to create MpvContext: {e}");
            return;
        }
    };

    let (wakeup_tx, mut wakeup_rx) = tokio::sync::mpsc::channel(64);
    // SAFETY: player is a newly created, valid MpvContext instance, and the
    // callback context is dropped when player is dropped at the end of this loop/worker.
    unsafe {
        player.register_callback(wakeup_tx);
    }

    // Frame buffers are pooled and reused while iced still holds them: a pool
    // slot is reusable only when the worker is its sole owner (strong_count ==
    // 1). The pool payload stays `Arc<Vec<u8>>` rather than `Arc<[u8]>`
    // precisely because the pool must resize in place via `Arc::get_mut`; an
    // immutable slice payload would force a fresh allocation per frame. The
    // initial size is only an allocation hint (per-frame `resize` sets the
    // real size), so a max-frame-size config that overflows usize degrades to
    // empty pool buffers instead of panicking. With hardware decoding the
    // frames are native-sized (mpv defaults), so the cap is not even a useful
    // hint and the pool starts empty.
    let max_buffer_size = if config.hardware_decoding {
        0
    } else {
        match rgba_frame_size(config.max_frame_width, config.max_frame_height) {
            Some(n) => n,
            None => {
                tracing::warn!(
                    "PlayerConfig max frame size {}x{} overflows usize; starting with empty pool buffers",
                    config.max_frame_width,
                    config.max_frame_height
                );
                0
            }
        }
    };
    let mut pool = [
        std::sync::Arc::new(vec![0u8; max_buffer_size]),
        std::sync::Arc::new(vec![0u8; max_buffer_size]),
        std::sync::Arc::new(vec![0u8; max_buffer_size]),
    ];

    let mut current_video_path = PathBuf::new();
    let mut canonical_video_path: Option<PathBuf> = None;
    let mut cached_video_params: Option<(i32, i32, Rotation)> = None;
    // Load-scoped one-shot deadline for the late rotation recheck (see the
    // progress tick below). Reset alongside `cached_video_params`.
    let mut rotation_recheck_deadline: Option<std::time::Instant> = None;
    // The mpv-reported `path` string that passed the initial paths_match
    // check for this load. The recheck compares against this string instead
    // of re-running `canonicalize()` (which is file I/O); mpv's `path`
    // property is stable within a load, so string equality is sufficient.
    let mut rotation_matched_path: Option<PathBuf> = None;
    let mut last_position = -1.0;
    let mut last_muted = false;
    let mut last_volume = -1.0;
    let mut last_paused = false;
    let mut is_active = false;

    // Diagnostics for "the video does not play": reported every 5 s from the
    // progress tick, which runs whether or not a frame is ever produced.
    let mut load_started: Option<std::time::Instant> = None;
    let mut frames_rendered: u32 = 0;
    let mut frames_delivered: u32 = 0;
    let mut frames_dropped: u32 = 0;
    let mut report_at = std::time::Instant::now();

    let mut progress_interval = tokio::time::interval(std::time::Duration::from_millis(100));

    loop {
        if event_tx.is_closed() && is_active {
            player.stop();
            is_active = false;
        }

        tokio::select! {
            cmd_opt = cmd_rx.recv() => {
                let Some(mut cmd) = cmd_opt else {
                    player.stop();
                    break;
                };
                while let Ok(newer_cmd) = cmd_rx.try_recv() {
                    cmd = newer_cmd;
                }
                match cmd {
                    VideoCommand::Load(path) => {
                        player.stop();
                        match player.load_file(&path) {
                            Ok(()) => {
                                player.set_paused(false);
                                player.drain_render_context();
                                is_active = true;
                                load_started = Some(std::time::Instant::now());
                                frames_rendered = 0;
                                frames_delivered = 0;
                                frames_dropped = 0;
                                report_at = std::time::Instant::now();
                                canonical_video_path = path.canonicalize().ok();
                                current_video_path = path;
                                cached_video_params = None;
                                rotation_recheck_deadline = None;
                                rotation_matched_path = None;
                            }
                            Err(err) => {
                                let _ = event_tx
                                    .send(VideoEvent::LoadFailed {
                                        path: path.clone(),
                                        error: err.to_string(),
                                    })
                                    .await;
                            }
                        }
                    }
                    VideoCommand::Play => {
                        player.set_paused(false);
                    }
                    VideoCommand::Pause => {
                        player.set_paused(true);
                    }
                    VideoCommand::TogglePause => {
                        player.toggle_pause();
                    }
                    VideoCommand::Seek(sec) => {
                        player.seek(sec);
                    }
                    VideoCommand::SeekAbsolute(sec) => {
                        player.seek_absolute(sec);
                    }
                    VideoCommand::SetMute(m) => {
                        player.set_mute(m);
                    }
                    VideoCommand::SetVolume(v) => {
                        player.set_volume(v);
                    }
                    VideoCommand::Stop => {
                        player.set_paused(true);
                        player.seek_absolute(0.0);
                        cached_video_params = None;
                        rotation_recheck_deadline = None;
                        rotation_matched_path = None;
                    }
                    VideoCommand::Deactivate => {
                        player.set_paused(true);
                        cached_video_params = None;
                        rotation_recheck_deadline = None;
                        rotation_matched_path = None;
                        // SAFETY: send "stop" command to release the current file handle and
                        // flush internal mpv caches, preventing file locks that would block
                        // rename/move/delete operations on the last-played video.
                        unsafe {
                            let mut cmd: [*const c_char; 2] =
                                [c"stop".as_ptr(), std::ptr::null()];
                            mpv_command(player.handle, cmd.as_mut_ptr());
                        }
                        is_active = false;
                    }
                }
            }

            _ = wakeup_rx.recv() => {
                // Drain any extra wakeup notifications in the channel for this tick
                while wakeup_rx.try_recv().is_ok() {}

                if is_active {
                    loop {
                        let flags = unsafe {
                            mpv_render_context_update(player.render_ctx)
                        };

                        if (flags & mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64) == 0 {
                            break;
                        }

                        if cached_video_params.is_none()
                            && let Some(current_p_str) = player.get_current_path()
                        {
                            let current_p = PathBuf::from(current_p_str);
                            let paths_match = current_p == current_video_path
                                || canonical_video_path.as_ref().is_some_and(|cp| current_p == *cp || current_p.canonicalize().ok().as_ref() == Some(cp));

                            if paths_match && player.is_video_ready() {
                                let (w, h) = player.get_video_size();
                                if w > 0 && h > 0 {
                                    let rotation = player.get_video_rotation();
                                    let (eff_w, eff_h) = if rotation.is_swapped() {
                                        (h, w)
                                    } else {
                                        (w, h)
                                    };

                                    let scale = render_scale(eff_w, eff_h, &config);
                                    let render_unrot_w = ((w as f64 * scale) as i32) & !1;
                                    let render_unrot_h = ((h as f64 * scale) as i32) & !1;

                                    if render_unrot_w > 0 && render_unrot_h > 0 {
                                        cached_video_params =
                                            Some((render_unrot_w, render_unrot_h, rotation));
                                        rotation_matched_path = Some(current_p.clone());
                                        // Schedule the one-shot late-rotation
                                        // recheck (handled in the progress
                                        // tick): mpv can populate
                                        // video-params/rotate or the
                                        // track-list demux-rotation AFTER
                                        // video-out-params first appears, so a
                                        // rotation probed at first-frame time
                                        // may still be stale.
                                        rotation_recheck_deadline = Some(
                                            std::time::Instant::now()
                                                + std::time::Duration::from_secs(2),
                                        );
                                    }
                                }
                            }
                        }

                        // Rotation is detected once per load above, plus a
                        // single property-only recheck ~2 s later (see the
                        // progress tick): a file's rotation cannot change
                        // during playback, and re-detection must not be
                        // repeated per tick — `get_video_rotation` re-opens
                        // and re-parses the container (read_mp4_tkhd_rotation),
                        // which a crafted file could turn into CPU burn.

                        if let Some((render_unrot_w, render_unrot_h, rotation)) = cached_video_params {
                            // Checked size math: hostile dims (e.g. an mpv
                            // video-params reporting a giant size) would
                            // overflow the i32 product — panicking in debug,
                            // wrapping in release, and a wrapped negative
                            // `as usize` becomes a giant length that drives
                            // `resize`/`vec![0u8; _]` into a huge allocation.
                            // Drop the frame like the FrameReady validation
                            // drops inconsistent frames.
                            let Some(unrot_size) =
                                rgba_frame_size(render_unrot_w as u32, render_unrot_h as u32)
                            else {
                                tracing::warn!(
                                    "render size {render_unrot_w}x{render_unrot_h} overflows usize; skipping frame"
                                );
                                break;
                            };

                            // Find a free buffer in the pool (where we are the sole owner)
                            let free_buffer = pool
                                .iter_mut()
                                .find(|buf| std::sync::Arc::strong_count(buf) == 1);

                            if let Some(arc_buf) = free_buffer
                                && let Some(target_vec) = std::sync::Arc::get_mut(arc_buf)
                            {
                                target_vec.resize(unrot_size, 0);
                                if player.render_frame(render_unrot_w, render_unrot_h, target_vec).is_ok() {
                                    frames_rendered += 1;
                                    if event_tx
                                        .try_send(VideoEvent::FrameReady {
                                            path: current_video_path.clone(),
                                            width: render_unrot_w as u32,
                                            height: render_unrot_h as u32,
                                            rotation,
                                            rgba: arc_buf.clone(),
                                        })
                                        .is_ok()
                                    {
                                        frames_delivered += 1;
                                        if let Some(started) = load_started.take() {
                                            tracing::debug!(
                                                width = render_unrot_w,
                                                height = render_unrot_h,
                                                first_frame_ms =
                                                    started.elapsed().as_millis() as u64,
                                                "first video frame delivered"
                                            );
                                        }
                                    } else {
                                        // The GUI is not keeping up: mpv renders
                                        // into our buffers, the queue is full,
                                        // so the frame is dropped instead of
                                        // stalling playback.
                                        frames_dropped += 1;
                                    }
                                }
                            } else {
                                // If buffer pool is currently fully occupied by iced view,
                                // we render into a temporary scratch buffer to advance mpv's state machine.
                                let mut dummy = vec![0u8; unrot_size];
                                let _ = player.render_frame(render_unrot_w, render_unrot_h, &mut dummy);
                            }
                        } else {
                            break;
                        }
                    }
                }
            }

            _ = progress_interval.tick() => {
                // mpv's own diagnostics (chosen hwdec, decoder, failures) come
                // through its event queue and are only visible if something
                // drains it — this tick is the one point that runs whether or
                // not frames are flowing.
                player.drain_log_messages();

                if is_active {
                    // One-shot late-rotation recheck. The initial detection
                    // runs at first-frame time, but mpv can populate
                    // `video-params/rotate` / `track-list/*/demux-rotation`
                    // only AFTER `video-out-params` first appears — a file
                    // whose rotation is not yet known then is reported as R0
                    // for the whole load. The recheck fires exactly once,
                    // ~2 s into the load, and does ZERO file I/O: the
                    // property probe reads mpv's already-parsed state, and
                    // the same-load guard is a pure string comparison
                    // against the path that already passed the initial
                    // detection (mpv's `path` property is stable within a
                    // load — a file switch without a Load command changes
                    // the string and skips the probe). It must never fall
                    // back to the file-based `get_video_rotation`, which
                    // re-opens and re-parses the container — repeating that
                    // would let a crafted file burn CPU per recheck.
                    if let Some(deadline) = rotation_recheck_deadline
                        && std::time::Instant::now() >= deadline
                    {
                        rotation_recheck_deadline = None;
                        if let Some((w, h, rot)) = cached_video_params
                            && let Some(current_p_str) = player.get_current_path()
                            && let Some(matched) = rotation_matched_path.as_deref()
                            && matched.as_os_str() == std::ffi::OsStr::new(&current_p_str)
                            && player.is_video_ready()
                        {
                            let prop_rot = player.probe_rotation_properties();
                            // R0 here means "no property known": never
                            // downgrade the file-parse result.
                            if prop_rot != Rotation::R0 && prop_rot != rot {
                                cached_video_params = Some((w, h, prop_rot));
                                tracing::debug!(
                                    "rotation recheck corrected {rot:?} -> {prop_rot:?}"
                                );
                            }
                        }
                    }

                    // Every 5 s while a file is loaded, report what the frame
                    // path actually did. A stalled video is either "nothing was
                    // ever rendered" (`video_size` stays none, so mpv never
                    // reported a usable size) or "rendered but the GUI is not
                    // consuming" (`dropped` climbing while `delivered` is not).
                    if report_at.elapsed() >= std::time::Duration::from_secs(5) {
                        tracing::debug!(
                            video_size = cached_video_params
                                .map(|(w, h, _)| format!("{w}x{h}"))
                                .unwrap_or_else(|| "none".to_string()),
                            rendered = frames_rendered,
                            delivered = frames_delivered,
                            dropped = frames_dropped,
                            "video frame report"
                        );
                        frames_rendered = 0;
                        frames_delivered = 0;
                        frames_dropped = 0;
                        report_at = std::time::Instant::now();
                    }

                    let mut pos: f64 = 0.0;
                    unsafe {
                        mpv_get_property(
                            player.handle,
                            c"time-pos".as_ptr(),
                            mpv_format_MPV_FORMAT_DOUBLE,
                            &mut pos as *mut _ as *mut c_void,
                        );
                    }
                    let dur = player.get_duration();
                    if pos != last_position {
                        // Stateful/periodic events are dropped under
                        // backpressure (like FrameReady): the worker must
                        // never stall command handling and frame pumping on a
                        // slow consumer, and the next tick resends the value.
                        let _ = event_tx.try_send(VideoEvent::PlaybackProgress {
                            position: pos,
                            duration: dur,
                        });
                        last_position = pos;
                    }

                    let mute = player.get_mute();
                    if mute != last_muted {
                        let _ = event_tx.try_send(VideoEvent::Muted(mute));
                        last_muted = mute;
                    }
                    let vol = player.get_volume();
                    if vol != last_volume {
                        let _ = event_tx.try_send(VideoEvent::Volume(vol));
                        last_volume = vol;
                    }
                    let paused = !player.is_playing();
                    if paused != last_paused {
                        let _ = event_tx.try_send(VideoEvent::Paused(paused));
                        last_paused = paused;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rotate_rgba_0() {
        let src = vec![255, 0, 0, 255, 0, 255, 0, 255]; // 2x1: Red, Green
        let (w, h, dst) = rotate_rgba(2, 1, &src, Rotation::R0);
        assert_eq!((w, h), (2, 1));
        assert_eq!(dst, src);
    }

    #[test]
    fn test_rotate_rgba_90() {
        let src = vec![255, 0, 0, 255, 0, 255, 0, 255]; // 2x1: Red, Green
        let (w, h, dst) = rotate_rgba(2, 1, &src, Rotation::R90);
        assert_eq!((w, h), (1, 2));
        // 90 deg CW: (0,0) Red -> (0,0); (1,0) Green -> (0,1)
        assert_eq!(&dst[0..4], &[255, 0, 0, 255]);
        assert_eq!(&dst[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn test_rotate_rgba_180() {
        let src = vec![255, 0, 0, 255, 0, 255, 0, 255]; // 2x1: Red, Green
        let (w, h, dst) = rotate_rgba(2, 1, &src, Rotation::R180);
        assert_eq!((w, h), (2, 1));
        // 180 deg: Green, Red
        assert_eq!(&dst[0..4], &[0, 255, 0, 255]);
        assert_eq!(&dst[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn test_rotate_rgba_270() {
        let src = vec![255, 0, 0, 255, 0, 255, 0, 255]; // 2x1: Red, Green
        let (w, h, dst) = rotate_rgba(2, 1, &src, Rotation::R270);
        assert_eq!((w, h), (1, 2));
        // 270 deg CW: (0,0) Red -> (0,1); (1,0) Green -> (0,0)
        assert_eq!(&dst[0..4], &[0, 255, 0, 255]);
        assert_eq!(&dst[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn test_rotate_rgba_rejects_undersized_src() {
        let src = vec![0u8; 4];
        let (w, h, dst) = rotate_rgba(1024, 1024, &src, Rotation::R90);
        assert_eq!((w, h), (0, 0));
        assert!(dst.is_empty());
    }

    #[test]
    fn test_rotate_rgba_rejects_undersized_src_even_for_r0() {
        // The front guard rejects an undersized source for EVERY rotation,
        // including R0: it is not a pure passthrough.
        let src = vec![0u8; 4];
        let (w, h, dst) = rotate_rgba(1024, 1024, &src, Rotation::R0);
        assert_eq!((w, h), (0, 0));
        assert!(dst.is_empty());
    }

    #[test]
    fn test_rotate_rgba_r0_slices_oversized_src_to_frame() {
        // A source buffer larger than the declared frame (pooled buffer
        // with an unused tail) must yield exactly the w*h*4 frame, not the
        // whole slice.
        let src = vec![7u8; 2 * 2 * 4 + 16];
        let (w, h, dst) = rotate_rgba(2, 2, &src, Rotation::R0);
        assert_eq!((w, h), (2, 2));
        assert_eq!(dst.len(), 2 * 2 * 4);
        assert!(dst.iter().all(|&b| b == 7));
    }

    #[test]
    fn test_rgba_frame_size_computes_valid_sizes() {
        assert_eq!(rgba_frame_size(2, 1), Some(8));
        assert_eq!(rgba_frame_size(960, 540), Some(960 * 540 * 4));
        assert_eq!(rgba_frame_size(10_000, 10_000), Some(400_000_000));
        assert_eq!(rgba_frame_size(0, 0), Some(0));
    }

    #[test]
    fn test_rgba_frame_size_overflow_returns_none() {
        // u32::MAX^2 * 4 overflows u64 -> None on every platform.
        assert_eq!(rgba_frame_size(u32::MAX, u32::MAX), None);
        // The worker render-path scenario: i32::MAX render dims overflow the
        // old i32 intermediate (panic in debug, negative wrap in release).
        // The helper must never wrap: it returns the exact u64 product when
        // it fits usize, and None otherwise.
        let dim = i32::MAX as u32;
        let exact = (dim as u64) * (dim as u64) * 4;
        match rgba_frame_size(dim, dim) {
            Some(n) => assert_eq!(n as u64, exact, "helper must not wrap"),
            None => assert!(exact > usize::MAX as u64),
        }
    }

    #[test]
    fn test_render_scale_hardware_decoding_is_native_size() {
        // With hardware decoding, frames render at mpv default (native)
        // resolution: scale is 1.0 regardless of the video size and the
        // configured cap.
        let config = PlayerConfig::default();
        assert_eq!(render_scale(1920, 1080, &config), 1.0);
        assert_eq!(render_scale(640, 360, &config), 1.0);
        assert_eq!(render_scale(3840, 2160, &config), 1.0);
    }

    #[test]
    fn test_render_scale_software_decoding_caps_frame() {
        let config = PlayerConfig {
            hardware_decoding: false,
            ..PlayerConfig::default()
        };
        // 1920x1080 fits 960x540 at scale 0.5.
        assert_eq!(render_scale(1920, 1080, &config), 0.5);
        // Smaller-than-cap videos are never upscaled.
        assert_eq!(render_scale(640, 360, &config), 1.0);
        // The tighter dimension wins: width here, height here.
        assert_eq!(render_scale(7680, 2160, &config), 0.125);
        assert_eq!(render_scale(1920, 4320, &config), 0.125);
    }
}
