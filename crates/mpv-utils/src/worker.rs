use crate::mpv_context::MpvContext;
use crate::rotation::Rotation;
use libmpv_sys::*;
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
    /// Whether the worker enables mpv hardware decoding. `true` configures
    /// `hwdec` with the platform copy-back decoder list (see
    /// `mpv_context::DEFAULT_HWDEC`; a pre-0.36 libmpv gets the single
    /// `auto-copy` decoder, and `MEDIA_SORT_MPV_HWDEC` overrides either).
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

/// Counts the frames the worker had to skip because every pooled buffer was
/// still held downstream.
///
/// Test-only: it exists so
/// `test_video_worker_rapid_switch_and_pool_starvation_recovery` can prove that
/// the pool-starvation branch actually ran. The pre-fix implementation rendered
/// into a throwaway buffer instead of skipping, so it never reached this branch
/// and the test passed without exercising it.
#[cfg(test)]
static FRAMES_STARVED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

/// Coalesce a batch of queued [`VideoCommand`]s without dropping a [`VideoCommand::Load`]
/// when followed by playback or volume/mute commands.
///
/// - If the batch contains one or more lifecycle-reset commands (`Load` or `Deactivate`),
///   any earlier file-specific commands (`Load`, `Deactivate`, `Stop`, `Play`, `Pause`,
///   `TogglePause`, `Seek`, `SeekAbsolute`) are superseded by the last lifecycle reset,
///   while player-wide `SetMute`/`SetVolume` updates before that reset are preserved.
/// - Consecutive `SeekAbsolute`, `SetMute`, or `SetVolume` commands collapse to the
///   latest value so slider drags do not flood mpv's command queue.
fn coalesce_command_batch(batch: Vec<VideoCommand>) -> Vec<VideoCommand> {
    let last_lifecycle_idx = batch
        .iter()
        .rposition(|cmd| matches!(cmd, VideoCommand::Load(_) | VideoCommand::Deactivate));

    let mut out: Vec<VideoCommand> = Vec::with_capacity(batch.len());
    for (idx, cmd) in batch.into_iter().enumerate() {
        if let Some(reset_idx) = last_lifecycle_idx
            && idx < reset_idx
            && !matches!(cmd, VideoCommand::SetMute(_) | VideoCommand::SetVolume(_))
        {
            continue;
        }

        match (&mut out.last_mut(), &cmd) {
            (Some(VideoCommand::SeekAbsolute(_)), VideoCommand::SeekAbsolute(_))
            | (Some(VideoCommand::SetMute(_)), VideoCommand::SetMute(_))
            | (Some(VideoCommand::SetVolume(_)), VideoCommand::SetVolume(_)) => {
                *out.last_mut().unwrap() = cmd;
            }
            _ => out.push(cmd),
        }
    }
    out
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
    // Per-load: whether any frame was ever rendered for the file loaded by the
    // current `Load`. Deliberately NOT reset by the 5 s telemetry window below
    // (which resets the counters it reports), because this is what keeps a
    // stale `MPV_EVENT_END_FILE` error from the previous file — or a transient
    // decoder warning — from being reported as a failure of a file that
    // actually renders.
    let mut has_rendered_frame_for_load = false;
    // At most one `LoadFailed` per load: mpv's log queue can hand over several
    // messages, and the consumer only needs to know the file did not play.
    let mut load_error_reported = false;

    // Diagnostics for "the video does not play": reported every 5 s from the
    // progress tick, which runs whether or not a frame is ever produced.
    let mut load_started: Option<std::time::Instant> = None;
    let mut frames_rendered: u32 = 0;
    let mut frames_delivered: u32 = 0;
    let mut frames_dropped: u32 = 0;
    let mut frames_starved: u32 = 0;
    let mut render_micros: u128 = 0;
    let mut render_micros_max: u128 = 0;
    let mut report_at = std::time::Instant::now();

    let mut progress_interval = tokio::time::interval(std::time::Duration::from_millis(100));

    loop {
        if event_tx.is_closed() && is_active {
            player.stop_async();
            is_active = false;
        }

        tokio::select! {
            cmd_opt = cmd_rx.recv() => {
                let Some(first_cmd) = cmd_opt else {
                    player.stop();
                    break;
                };
                let mut raw_batch = vec![first_cmd];
                while let Ok(newer_cmd) = cmd_rx.try_recv() {
                    raw_batch.push(newer_cmd);
                }
                for cmd in coalesce_command_batch(raw_batch) {
                    match cmd {
                        VideoCommand::Load(path) => {
                            // Discard anything the *previous* file left in mpv's
                            // log/event queue before the new load starts: a
                            // pending `MPV_EVENT_END_FILE` from the old file
                            // would otherwise be drained by the progress tick
                            // after this load and attributed to `path`, showing
                            // "file not supported" for a file that plays fine.
                            let _ = player.drain_log_messages();
                            // `loadfile` replaces any currently loaded file directly.
                            // Using `load_file_async` + `set_paused_async` avoids
                            // blocking the render thread on synchronous `stop` +
                            // `loadfile` calls while `vo_libmpv` is active.
                            match player.load_file_async(&path) {
                                Ok(()) => {
                                    player.set_paused_async(false);
                                    player.drain_render_context();
                                    is_active = true;
                                    has_rendered_frame_for_load = false;
                                    load_error_reported = false;
                                    load_started = Some(std::time::Instant::now());
                                    frames_rendered = 0;
                                    frames_delivered = 0;
                                    frames_dropped = 0;
                                    frames_starved = 0;
                                    render_micros = 0;
                                    render_micros_max = 0;
                                    report_at = std::time::Instant::now();
                                    canonical_video_path = path.canonicalize().ok();
                                    current_video_path = path;
                                    cached_video_params = None;
                                    rotation_recheck_deadline = None;
                                    rotation_matched_path = None;
                                }
                                Err(err) => {
                                    load_error_reported = true;
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
                            player.set_paused_async(false);
                        }
                        VideoCommand::Pause => {
                            player.set_paused_async(true);
                        }
                        VideoCommand::TogglePause => {
                            player.toggle_pause_async();
                        }
                        VideoCommand::Seek(sec) => {
                            player.seek_async(sec);
                        }
                        VideoCommand::SeekAbsolute(sec) => {
                            player.seek_absolute_async(sec);
                        }
                        VideoCommand::SetMute(m) => {
                            player.set_mute_async(m);
                        }
                        VideoCommand::SetVolume(v) => {
                            player.set_volume_async(v);
                        }
                        VideoCommand::Stop => {
                            player.set_paused_async(true);
                            player.seek_absolute_async(0.0);
                            cached_video_params = None;
                            rotation_recheck_deadline = None;
                            rotation_matched_path = None;
                            has_rendered_frame_for_load = false;
                            load_error_reported = false;
                        }
                        VideoCommand::Deactivate => {
                            player.set_paused_async(true);
                            cached_video_params = None;
                            rotation_recheck_deadline = None;
                            rotation_matched_path = None;
                            has_rendered_frame_for_load = false;
                            load_error_reported = false;
                            // Release the current file handle and flush internal mpv
                            // caches so rename/move/delete operations on the
                            // last-played video are not blocked.
                            player.stop_async();
                            is_active = false;
                        }
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
                                player.skip_frame();
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
                                let render_started = std::time::Instant::now();
                                let rendered = player
                                    .render_frame(render_unrot_w, render_unrot_h, target_vec)
                                    .is_ok();
                                let render_elapsed = render_started.elapsed().as_micros();
                                render_micros += render_elapsed;
                                render_micros_max = render_micros_max.max(render_elapsed);
                                if rendered {
                                    frames_rendered += 1;
                                    has_rendered_frame_for_load = true;
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
                                // Every pool buffer is still held downstream (by
                                // the GUI, or by frames sitting in the event
                                // queue), so there is nothing to deliver into.
                                //
                                // Skip the frame in `vo_libmpv` (`MPV_RENDER_PARAM_SKIP_RENDERING`)
                                // so `draw_frame` wakes up immediately without
                                // converting or copying pixels; delivery resumes
                                // as soon as the consumer releases a buffer.
                                player.skip_frame();
                                frames_starved += 1;
                                #[cfg(test)]
                                FRAMES_STARVED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        } else {
                            // `vo_libmpv` is waiting in `draw_frame` for this
                            // frame to be consumed even though `video-out-params`
                            // is not ready yet; skip it so the VO thread can
                            // advance instead of timing out.
                            player.skip_frame();
                            break;
                        }
                    }
                } else {
                    player.drain_render_context();
                }
            }

            _ = progress_interval.tick() => {
                // mpv's own diagnostics (chosen hwdec, decoder, failures) come
                // through its event queue and are only visible if something
                // drains it — this tick is the one point that runs whether or
                // not frames are flowing.
                if let Some(error) = player.drain_log_messages()
                    && is_active
                    && !has_rendered_frame_for_load
                    && !load_error_reported
                    && !current_video_path.as_os_str().is_empty()
                {
                    load_error_reported = true;
                    let _ = event_tx.try_send(VideoEvent::LoadFailed {
                        path: current_video_path.clone(),
                        error,
                    });
                }

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
                            starved = frames_starved,
                            render_avg_ms = if frames_rendered > 0 {
                                render_micros as f64 / frames_rendered as f64 / 1000.0
                            } else {
                                0.0
                            },
                            render_max_ms = render_micros_max as f64 / 1000.0,
                            "video frame report"
                        );
                        frames_rendered = 0;
                        frames_delivered = 0;
                        frames_dropped = 0;
                        frames_starved = 0;
                        render_micros = 0;
                        render_micros_max = 0;
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

    #[test]
    fn test_coalesce_command_batch_preserves_load_and_settings() {
        let batch = vec![
            VideoCommand::SetVolume(40.0),
            VideoCommand::SetVolume(75.0),
            VideoCommand::Load(PathBuf::from("first.mp4")),
            VideoCommand::Seek(1.0),
            VideoCommand::Pause,
            VideoCommand::SetMute(true),
            VideoCommand::Load(PathBuf::from("second.mp4")),
            VideoCommand::SeekAbsolute(2.0),
            VideoCommand::SeekAbsolute(5.0),
            VideoCommand::Play,
        ];
        let coalesced = coalesce_command_batch(batch);
        assert_eq!(coalesced.len(), 5);
        assert!(matches!(coalesced[0], VideoCommand::SetVolume(v) if (v - 75.0).abs() < 1e-6));
        assert!(matches!(coalesced[1], VideoCommand::SetMute(true)));
        assert!(
            matches!(&coalesced[2], VideoCommand::Load(p) if p == std::path::Path::new("second.mp4"))
        );
        assert!(matches!(coalesced[3], VideoCommand::SeekAbsolute(s) if (s - 5.0).abs() < 1e-6));
        assert!(matches!(coalesced[4], VideoCommand::Play));
    }

    #[tokio::test]
    async fn test_video_worker_rapid_switch_and_pool_starvation_recovery() {
        // Skip where libmpv (or a usable software VO) is unavailable, like the
        // other video tests: the worker would fail to start and the waits below
        // would only run into their deadline.
        if crate::MpvContext::new().is_err() {
            return;
        }

        let fixture = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/MockState/mock 3.mp4"
        ));
        if !fixture.exists() {
            return;
        }

        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(32);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(32);
        start_video_worker(cmd_rx, event_tx);

        // Rapidly issue Load/Seek/Pause/Play commands to exercise async
        // command dispatch while vo_libmpv is initializing and rendering.
        for _ in 0..5 {
            cmd_tx
                .send(VideoCommand::Load(fixture.clone()))
                .await
                .unwrap();
            cmd_tx.send(VideoCommand::Seek(0.2)).await.unwrap();
            cmd_tx.send(VideoCommand::TogglePause).await.unwrap();
            cmd_tx.send(VideoCommand::Play).await.unwrap();
        }

        // Hold all 3 frame buffers simultaneously to trigger pool starvation
        // (`frames_starved` -> `player.skip_frame()`), then drop them and
        // confirm frame delivery resumes immediately without vo_libmpv hanging.
        let mut held_buffers = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while held_buffers.len() < 3 {
            let ev = tokio::time::timeout_at(deadline, event_rx.recv())
                .await
                .expect("timed out waiting for initial frames")
                .expect("worker event channel closed");
            if let VideoEvent::FrameReady {
                rgba,
                width,
                height,
                ..
            } = ev
            {
                assert!(width > 0 && height > 0);
                held_buffers.push(rgba);
            }
        }

        // Wait until the exhaustion branch has actually run at least once. This
        // is what distinguishes the real pool from the pre-fix "render into a
        // throwaway buffer" code, which kept frames flowing instead of skipping
        // them. No fourth `FrameReady` may arrive in the meantime.
        FRAMES_STARVED.store(0, std::sync::atomic::Ordering::Relaxed);
        let starve_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while FRAMES_STARVED.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            assert!(
                tokio::time::Instant::now() < starve_deadline,
                "no frame was skipped while all three pool buffers were held: the pool-starvation branch never ran"
            );
            match tokio::time::timeout_at(starve_deadline, event_rx.recv()).await {
                Ok(Some(VideoEvent::FrameReady { .. })) => panic!(
                    "a fourth FrameReady arrived while all three pool buffers were held: frame delivery is not bounded by the pool"
                ),
                Ok(Some(_)) => {}
                Ok(None) => panic!("worker event channel closed"),
                // The deadline itself is asserted at the top of the loop.
                Err(_) => {}
            }
        }

        // All three pool slots are downstream now, so the worker must skip
        // frames instead of handing out a fourth buffer — and it must keep
        // servicing commands while it does (it is not stuck in `draw_frame`).
        cmd_tx.send(VideoCommand::SetMute(true)).await.unwrap();
        let responsive_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut saw_muted = false;
        while !saw_muted {
            let ev = tokio::time::timeout_at(responsive_deadline, event_rx.recv())
                .await
                .expect("worker stopped servicing commands while every pool buffer was held")
                .expect("worker event channel closed");
            match ev {
                VideoEvent::FrameReady { .. } => panic!(
                    "a fourth FrameReady arrived while all three pool buffers were held: frame delivery is not bounded by the pool"
                ),
                VideoEvent::Muted(muted) => saw_muted = muted,
                _ => {}
            }
        }

        // Releasing a single buffer must let delivery resume, so this waits on
        // a fresh deadline rather than the original frame deadline.
        held_buffers.pop();
        let resume_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let ev = tokio::time::timeout_at(resume_deadline, event_rx.recv())
                .await
                .expect("timed out waiting for frame after pool starvation")
                .expect("worker event channel closed");
            if matches!(ev, VideoEvent::FrameReady { .. }) {
                break;
            }
        }

        cmd_tx.send(VideoCommand::Deactivate).await.unwrap();
    }
}
