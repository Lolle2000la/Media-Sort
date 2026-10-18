use crate::rotation::{Rotation, detect_video_rotation};
use libmpv_sys::*;
use std::ffi::{CStr, CString};
use std::fmt;
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

const MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME: mpv_render_param_type = 12;
const MPV_RENDER_PARAM_SKIP_RENDERING: mpv_render_param_type = 13;
const MPV_RENDER_PARAM_SW_SIZE: mpv_render_param_type = 17;
const MPV_RENDER_PARAM_SW_FORMAT: mpv_render_param_type = 18;
const MPV_RENDER_PARAM_SW_STRIDE: mpv_render_param_type = 19;
const MPV_RENDER_PARAM_SW_POINTER: mpv_render_param_type = 20;

/// Default copy-back hardware decoder probe list when hardware decoding is
/// enabled.
///
/// We deliberately list the platform copy-back APIs (`nvdec-copy`, `vaapi-copy`,
/// `d3d11va-copy`, `dxva2-copy`, `videotoolbox-copy`) instead of `auto-copy`:
/// on Linux with mpv 0.41+ and FFmpeg 8.1+, `auto-copy` probes `vulkan-copy`
/// first (`h264-vulkan-copy`, `hevc-vulkan-copy`, `av1-vulkan-copy`), which
/// logs `av_log callback called with bad parameters (NULL AVClass)`, stalls
/// ~600 ms per load, and segfaults (`SIGSEGV`) on decoder teardown when files
/// are switched rapidly inside the Flatpak runtime. Explicit platform copy
/// decoders use NVDEC on NVIDIA and VAAPI on AMD/Intel Mesa without touching
/// `vulkan-copy`. Override at runtime with `MEDIA_SORT_MPV_HWDEC`.
const DEFAULT_HWDEC: &str = "nvdec-copy,vaapi-copy,d3d11va-copy,dxva2-copy,videotoolbox-copy";

/// Errors produced by [`MpvContext`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MpvError {
    /// `mpv_create` returned a null handle.
    CreateFailed,
    /// `mpv_initialize` failed with the given libmpv error code.
    Initialize(i32),
    /// `mpv_render_context_create` failed with the given libmpv error code.
    RenderContextCreate(i32),
    /// `mpv_render_context_render` failed with the given libmpv error code.
    RenderFrame(i32),
    /// `mpv_command` failed with the given libmpv error code.
    Command(i32),
    /// `mpv_set_property`/`mpv_get_property` failed with the given libmpv error code.
    Property(i32),
    /// The caller-provided output buffer was too small for the requested frame.
    BufferTooSmall { required: usize, available: usize },
    /// The path cannot be converted to a C string (non-UTF-8).
    InvalidPath,
    /// [`MpvContext::capture_frame`] did not produce a frame within its timeout.
    CaptureTimeout,
    /// A C-string conversion failed unexpectedly.
    InvalidCString,
    /// Any other failure with a human-readable message.
    Other(String),
}

impl fmt::Display for MpvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateFailed => write!(f, "failed to create mpv instance"),
            Self::Initialize(code) => write!(f, "failed to initialize mpv (code {code})"),
            Self::RenderContextCreate(code) => {
                write!(f, "failed to create render context (code {code})")
            }
            Self::RenderFrame(code) => write!(f, "failed to render frame (code {code})"),
            Self::Command(code) => write!(f, "mpv command failed (code {code})"),
            Self::Property(code) => write!(f, "mpv property access failed (code {code})"),
            Self::BufferTooSmall {
                required,
                available,
            } => write!(
                f,
                "buffer too small: {available} bytes, need {required} bytes"
            ),
            Self::InvalidPath => write!(f, "path is not valid UTF-8"),
            Self::CaptureTimeout => write!(f, "timed out waiting for a video frame"),
            Self::InvalidCString => write!(f, "string contains a null byte"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for MpvError {}

/// A software-rendered libmpv instance.
///
/// Wraps a raw `mpv_handle` plus a software (`sw`) render context that renders
/// frames into caller-provided CPU memory via [`MpvContext::render_frame`].
/// Use [`MpvContext::new`] for video playback and
/// [`MpvContext::new_thumbnail_player`] for cheap thumbnail extraction.
///
/// The handle is Send and Sync; callers must serialize access (a single
/// [`worker`](crate::worker) loop or one thread per context) because libmpv
/// render contexts are not safe for concurrent access.
pub struct MpvContext {
    pub(crate) handle: *mut mpv_handle,
    pub(crate) render_ctx: *mut mpv_render_context,
    callback_context_raw: *mut c_void,
}

impl MpvContext {
    /// Creates a fully configured playback context (video + audio enabled)
    /// with hardware decoding enabled.
    pub fn new() -> Result<Self, MpvError> {
        Self::create(false, true)
    }

    /// Creates a fully configured playback context (video + audio enabled).
    /// `hardware_decoding` controls mpv's `hwdec` option: `true` enables the
    /// platform copy-back hardware decoder list, `false` forces software
    /// decoding (`hwdec=no`).
    pub fn new_with_hardware_decoding(hardware_decoding: bool) -> Result<Self, MpvError> {
        Self::create(false, hardware_decoding)
    }

    /// Creates a minimal context tuned for thumbnail extraction: audio,
    /// subtitles, and hardware decoding are disabled and seeking is allowed to
    /// be approximate, which makes single-frame capture fast without opening
    /// competing GPU hardware decode contexts across thumbnail worker threads.
    pub fn new_thumbnail_player() -> Result<Self, MpvError> {
        Self::create(true, false)
    }

    fn create(thumbnail_mode: bool, hardware_decoding: bool) -> Result<Self, MpvError> {
        unsafe {
            let handle = mpv_create();
            if handle.is_null() {
                return Err(MpvError::CreateFailed);
            }

            // Common options for both modes.
            Self::set_option(handle, c"vo", "libmpv");
            Self::set_option(handle, c"keep-open", "yes");
            // libmpv does not log to the terminal for a library client, and the
            // event draining in `drain_log_messages` cannot run while a
            // synchronous libmpv call is blocked — which is exactly the failure
            // that matters ("no frames", with nothing to explain it). When
            // verbose logging is requested, let mpv write to stderr directly as
            // well, so a hang still leaves its last messages behind.
            if verbose_logging_requested() {
                Self::set_option(handle, c"terminal", "yes");
                Self::set_option(handle, c"msg-level", "all=info");
            }
            if hardware_decoding {
                // `MEDIA_SORT_MPV_HWDEC` overrides the decoder, for diagnosing a
                // hardware path that misbehaves on one machine or in a sandbox
                // (e.g. forcing `nvdec-copy`, or `no`); the value is handed to
                // mpv verbatim, so it also accepts a probe list.
                let hwdec = std::env::var("MEDIA_SORT_MPV_HWDEC")
                    .unwrap_or_else(|_| DEFAULT_HWDEC.to_string());
                Self::set_option(handle, c"hwdec", &hwdec);
            } else {
                Self::set_option(handle, c"hwdec", "no");
            }
            Self::set_option(handle, c"sub-auto", "no");
            Self::set_option(handle, c"audio-file-auto", "no");
            Self::set_option(handle, c"cache", "no");
            // Rotation is applied manually via rotate_rgba after rendering so
            // the raw unrotated frame can be uploaded to the GPU cheaply.
            Self::set_option(handle, c"video-rotate", "no");
            Self::set_option(handle, c"force-window", "no");
            Self::set_option(handle, c"input-default-bindings", "no");

            if thumbnail_mode {
                Self::set_option(handle, c"aid", "no");
                Self::set_option(handle, c"sid", "no");
                Self::set_option(handle, c"hr-seek", "no");
                Self::set_option(handle, c"video-sync", "desync");
            } else {
                Self::set_option(handle, c"loop-file", "inf");
                Self::set_option(handle, c"framedrop", "vo");
                Self::set_option(handle, c"video-sync", "audio");
                Self::set_option(handle, c"video-timing-offset", "0");
            }

            let err = mpv_initialize(handle);
            if err < 0 {
                mpv_terminate_destroy(handle);
                return Err(MpvError::Initialize(err));
            }

            // Ask libmpv for its own log messages, which are otherwise dropped
            // on the floor. Inside a Flatpak there is no mpv CLI to fall back
            // on, so a decoder or driver problem — which hwdec mpv picked, and
            // why it failed — would be invisible. `drain_log_messages` forwards
            // them to `tracing`.
            mpv_request_log_messages(handle, c"info".as_ptr());

            let api_type = CString::new("sw").map_err(|_| MpvError::InvalidCString)?;
            let mut params = [
                mpv_render_param {
                    type_: mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                    data: api_type.as_ptr() as *mut c_void,
                },
                mpv_render_param {
                    type_: 0,
                    data: ptr::null_mut(),
                },
            ];

            let mut render_ctx: *mut mpv_render_context = ptr::null_mut();
            let err = mpv_render_context_create(&mut render_ctx, handle, params.as_mut_ptr());
            if err < 0 {
                mpv_terminate_destroy(handle);
                return Err(MpvError::RenderContextCreate(err));
            }

            Ok(Self {
                handle,
                render_ctx,
                callback_context_raw: ptr::null_mut(),
            })
        }
    }

    /// Best-effort option setter; errors are ignored because the option set is
    /// static and expected to be accepted by any libmpv version in use.
    fn set_option(handle: *mut mpv_handle, key: &'static std::ffi::CStr, value: &str) {
        unsafe {
            let value = CString::new(value).expect("static string contains no null bytes");
            mpv_set_option_string(handle, key.as_ptr(), value.as_ptr());
        }
    }

    /// Registers a wakeup callback that `try_send`s `()` on `sender` whenever
    /// the render context has new work (e.g. a frame) to process.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self.render_ctx` is a valid render context
    /// and that the sender remains usable for the lifetime of the context.
    pub(crate) unsafe fn register_callback(&mut self, sender: tokio::sync::mpsc::Sender<()>) {
        let sender_box = Box::new(sender);
        self.callback_context_raw = Box::into_raw(sender_box) as *mut c_void;

        unsafe {
            mpv_render_context_set_update_callback(
                self.render_ctx,
                Some(mpv_wakeup_callback),
                self.callback_context_raw,
            );
        }
    }

    /// Returns `true` if the render context has at least one pending frame.
    ///
    /// IMPORTANT: When this returns `true`, the caller **must** follow up with
    /// either [`Self::render_frame`] or [`Self::skip_frame`]. In libmpv's
    /// `vo_libmpv`, `mpv_render_context_update` clears the update flag without
    /// releasing `ctx->next_frame`, leaving `vo_libmpv`'s `draw_frame` blocked
    /// in `pthread_cond_timedwait` until `mpv_render_context_render` runs.
    pub fn has_frame_ready(&self) -> bool {
        let flags = unsafe { mpv_render_context_update(self.render_ctx) };
        (flags & mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64) != 0
    }

    /// Consumes a pending frame in `vo_libmpv` without rendering pixels.
    ///
    /// In libmpv's `vo_libmpv`, `draw_frame` sets `ctx->next_frame` and blocks
    /// in `pthread_cond_timedwait` until `mpv_render_context_render` clears
    /// `ctx->next_frame`. Calling `mpv_render_context_render` with
    /// `MPV_RENDER_PARAM_SKIP_RENDERING = 1` and
    /// `MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME = 0` immediately clears
    /// `ctx->next_frame` and wakes the VO thread with zero pixel work.
    pub fn skip_frame(&self) {
        unsafe {
            let mut skip: c_int = 1;
            let mut block: c_int = 0;
            let mut params = [
                mpv_render_param {
                    type_: MPV_RENDER_PARAM_SKIP_RENDERING,
                    data: &mut skip as *mut _ as *mut c_void,
                },
                mpv_render_param {
                    type_: MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                    data: &mut block as *mut _ as *mut c_void,
                },
                mpv_render_param {
                    type_: 0,
                    data: ptr::null_mut(),
                },
            ];
            let _ = mpv_render_context_render(self.render_ctx, params.as_mut_ptr());
        }
    }

    /// Forwards mpv's pending log messages to `tracing`, draining mpv's client
    /// event queue.
    ///
    /// mpv delivers its log messages as *events*, so a client that never waits
    /// for events both loses every diagnostic and lets the queue grow for as
    /// long as a file plays. That is why this is called from the worker's
    /// periodic tick and from the thumbnail capture loop, and why it must run on
    /// the thread that owns the handle. It never blocks (zero timeout), so it
    /// cannot stall frame delivery.
    pub(crate) fn drain_log_messages(&self) -> Option<String> {
        let mut load_error = None;
        unsafe {
            loop {
                let event = mpv_wait_event(self.handle, 0.0);
                if event.is_null() {
                    break;
                }
                let event = &*event;
                if event.event_id == mpv_event_id_MPV_EVENT_NONE {
                    break;
                }
                if event.event_id == mpv_event_id_MPV_EVENT_LOG_MESSAGE && !event.data.is_null() {
                    forward_log_message(&*event.data.cast::<mpv_event_log_message>());
                } else if event.event_id == mpv_event_id_MPV_EVENT_END_FILE && !event.data.is_null()
                {
                    let end_file = &*event.data.cast::<mpv_event_end_file>();
                    if end_file.reason == mpv_end_file_reason_MPV_END_FILE_REASON_ERROR as c_int
                        && end_file.error < 0
                    {
                        let err_str = cstr_lossy(mpv_error_string(end_file.error));
                        load_error = Some(if err_str.is_empty() {
                            format!("mpv playback error (code {})", end_file.error)
                        } else {
                            err_str
                        });
                    }
                }
            }
        }
        load_error
    }

    /// The path of the currently loaded file, if any.
    pub fn get_current_path(&self) -> Option<String> {
        unsafe {
            let mut path_ptr: *mut c_char = ptr::null_mut();
            let err = mpv_get_property(
                self.handle,
                c"path".as_ptr(),
                mpv_format_MPV_FORMAT_STRING,
                &mut path_ptr as *mut _ as *mut c_void,
            );
            if err >= 0 && !path_ptr.is_null() {
                let s = CStr::from_ptr(path_ptr).to_string_lossy().into_owned();
                mpv_free(path_ptr as *mut c_void);
                Some(s)
            } else {
                None
            }
        }
    }

    /// Loads a file into the player. Returns an error when the path is not
    /// valid UTF-8 or mpv rejects the file.
    pub fn load_file(&mut self, path: &Path) -> Result<(), MpvError> {
        self.drain_render_context();
        unsafe {
            let path_str = CString::new(path.to_str().ok_or(MpvError::InvalidPath)?)
                .map_err(|_| MpvError::InvalidCString)?;
            let mut cmd: [*const c_char; 3] =
                [c"loadfile".as_ptr(), path_str.as_ptr(), ptr::null()];
            let err = mpv_command(self.handle, cmd.as_mut_ptr());
            if err < 0 {
                return Err(MpvError::Command(err));
            }
            Ok(())
        }
    }

    /// Loads a file asynchronously without blocking the render thread.
    ///
    /// `mpv_command_async` is explicitly safe to call from libmpv render API
    /// threads (`mpv/client.h`), whereas synchronous `mpv_command` can deadlock
    /// if the mpv core waits on `vo_libmpv` while `vo_libmpv` waits on
    /// `mpv_render_context_render`.
    pub(crate) fn load_file_async(&mut self, path: &Path) -> Result<(), MpvError> {
        self.drain_render_context();
        unsafe {
            let path_str = CString::new(path.to_str().ok_or(MpvError::InvalidPath)?)
                .map_err(|_| MpvError::InvalidCString)?;
            let mut cmd: [*const c_char; 3] =
                [c"loadfile".as_ptr(), path_str.as_ptr(), ptr::null()];
            let err = mpv_command_async(self.handle, 0, cmd.as_mut_ptr());
            if err < 0 {
                return Err(MpvError::Command(err));
            }
            Ok(())
        }
    }

    /// Sends a `stop` command to release the current file and flush internal
    /// caches. Drains any pending frame first so `vo_libmpv` is not blocked in
    /// `draw_frame` while the core processes `stop`.
    pub fn stop(&mut self) {
        self.drain_render_context();
        unsafe {
            let mut cmd: [*const c_char; 2] = [c"stop".as_ptr(), ptr::null()];
            mpv_command(self.handle, cmd.as_mut_ptr());
        }
        self.drain_render_context();
    }

    /// Drains all pending render-context frames without rendering pixels,
    /// waking `vo_libmpv` if it is waiting in `draw_frame`.
    pub fn drain_render_context(&self) {
        for _ in 0..128 {
            if !self.has_frame_ready() {
                break;
            }
            self.skip_frame();
        }
    }

    /// Returns `true` when the video output chain is fully initialized and
    /// ready to produce frames. Must be checked before calling
    /// [`MpvContext::render_frame`] to avoid `mp_image_crop` assertions during
    /// the transient initialization window between `load_file` and the first
    /// fully-formed frame.
    pub fn is_video_ready(&self) -> bool {
        unsafe {
            let mut ptr: *mut c_char = ptr::null_mut();
            let err = mpv_get_property(
                self.handle,
                c"video-out-params".as_ptr(),
                mpv_format_MPV_FORMAT_STRING,
                &mut ptr as *mut _ as *mut c_void,
            );
            if err >= 0 && !ptr.is_null() {
                mpv_free(ptr as *mut c_void);
                true
            } else {
                false
            }
        }
    }

    /// The width and height of the decoded video, in display pixels. Returns
    /// `(0, 0)` when no video is loaded.
    pub fn get_video_size(&self) -> (i64, i64) {
        unsafe {
            let mut width: i64 = 0;
            let mut height: i64 = 0;
            mpv_get_property(
                self.handle,
                c"video-params/w".as_ptr(),
                mpv_format_MPV_FORMAT_INT64,
                &mut width as *mut _ as *mut c_void,
            );
            mpv_get_property(
                self.handle,
                c"video-params/h".as_ptr(),
                mpv_format_MPV_FORMAT_INT64,
                &mut height as *mut _ as *mut c_void,
            );
            (width, height)
        }
    }

    /// The duration of the currently loaded media in seconds, or `0.0` when
    /// nothing is loaded.
    pub fn get_duration(&self) -> f64 {
        unsafe {
            let mut dur: f64 = 0.0;
            mpv_get_property(
                self.handle,
                c"duration".as_ptr(),
                mpv_format_MPV_FORMAT_DOUBLE,
                &mut dur as *mut _ as *mut c_void,
            );
            dur
        }
    }

    /// The effective rotation of the currently loaded video.
    ///
    /// Prefers the file-based detection in [`detect_video_rotation`] (mp4
    /// `tkhd` matrix / EXIF / mp4ameta) and falls back to
    /// [`Self::probe_rotation_properties`]. Returns [`Rotation::R0`] when
    /// nothing is loaded or no rotation is known.
    pub fn get_video_rotation(&self) -> Rotation {
        if let Some(path_str) = self.get_current_path()
            && let Some(rot) = detect_video_rotation(Path::new(&path_str))
            && rot != Rotation::R0
        {
            return rot;
        }

        self.probe_rotation_properties()
    }

    /// Probes mpv's already-parsed rotation state through properties only —
    /// no file I/O.
    ///
    /// Checks `video-params/rotate`, `video-out-params/rotate`, the
    /// track-list demux/user rotation properties and metadata tags, in that
    /// order. Unlike [`detect_video_rotation`] this never touches the file,
    /// so it is safe to call repeatedly (e.g. the worker's delayed one-shot
    /// recheck, since mpv may populate these properties only after
    /// `video-out-params` first appears). Returns [`Rotation::R0`] when no
    /// property carries a rotation.
    pub(crate) fn probe_rotation_properties(&self) -> Rotation {
        unsafe {
            // 1. Check video-params/rotate
            let mut rotate: i64 = 0;
            let mut err = mpv_get_property(
                self.handle,
                c"video-params/rotate".as_ptr(),
                mpv_format_MPV_FORMAT_INT64,
                &mut rotate as *mut _ as *mut c_void,
            );
            if err >= 0 && rotate != 0 {
                return Rotation::from_degrees(rotate);
            }

            // 2. Check video-out-params/rotate
            err = mpv_get_property(
                self.handle,
                c"video-out-params/rotate".as_ptr(),
                mpv_format_MPV_FORMAT_INT64,
                &mut rotate as *mut _ as *mut c_void,
            );
            if err >= 0 && rotate != 0 {
                return Rotation::from_degrees(rotate);
            }

            // 3. Check track-list/0/demux-rotation
            err = mpv_get_property(
                self.handle,
                c"track-list/0/demux-rotation".as_ptr(),
                mpv_format_MPV_FORMAT_INT64,
                &mut rotate as *mut _ as *mut c_void,
            );
            if err >= 0 && rotate != 0 {
                return Rotation::from_degrees(rotate);
            }

            // 4. Check track-list/0/user-rotation
            err = mpv_get_property(
                self.handle,
                c"track-list/0/user-rotation".as_ptr(),
                mpv_format_MPV_FORMAT_INT64,
                &mut rotate as *mut _ as *mut c_void,
            );
            if err >= 0 && rotate != 0 {
                return Rotation::from_degrees(rotate);
            }

            // 5. Check metadata string tags
            let meta_keys = [
                c"metadata/by-key/rotate".as_ptr(),
                c"metadata/by-key/ROTATE".as_ptr(),
                c"metadata/by-key/orientation".as_ptr(),
                c"metadata/by-key/ORIENTATION".as_ptr(),
                c"metadata/by-key/com.apple.quicktime.orientation".as_ptr(),
            ];

            for key in meta_keys {
                let mut str_ptr: *mut c_char = ptr::null_mut();
                err = mpv_get_property(
                    self.handle,
                    key,
                    mpv_format_MPV_FORMAT_STRING,
                    &mut str_ptr as *mut _ as *mut c_void,
                );
                if err >= 0 && !str_ptr.is_null() {
                    let s = CStr::from_ptr(str_ptr).to_string_lossy();
                    let parsed = s.trim().parse::<i64>().unwrap_or(0);
                    mpv_free(str_ptr as *mut c_void);
                    if parsed != 0 {
                        return Rotation::from_degrees(parsed);
                    }
                }
            }

            Rotation::R0
        }
    }

    /// Renders the current video frame into `buffer` as raw RGBA.
    ///
    /// `buffer` must be at least `width * height * 4` bytes long.
    pub fn render_frame(&self, width: i32, height: i32, buffer: &mut [u8]) -> Result<(), MpvError> {
        if width <= 0 || height <= 0 {
            return Err(MpvError::Other(format!(
                "invalid frame size {width}x{height}"
            )));
        }
        // Checked size math: an unchecked i32/usize product would panic in
        // debug builds and wrap in release, turning hostile dims into a
        // giant `required` that vacuously passes the length check.
        let Some(required) = crate::worker::rgba_frame_size(width as u32, height as u32) else {
            return Err(MpvError::Other(format!(
                "frame size {width}x{height} overflows the buffer size math"
            )));
        };
        if buffer.len() < required {
            return Err(MpvError::BufferTooSmall {
                required,
                available: buffer.len(),
            });
        }
        // Same checked math for the row stride (an unchecked `width * 4` in
        // i32 would panic for widths above i32::MAX / 4).
        let mut stride = (width as u64)
            .checked_mul(4)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| {
                MpvError::Other(format!("frame stride for width {width} overflows usize"))
            })?;
        unsafe {
            let format = CString::new("rgba").map_err(|_| MpvError::InvalidCString)?;
            let mut size: [c_int; 2] = [width, height];
            let mut block: c_int = 0;

            let mut params = [
                mpv_render_param {
                    type_: MPV_RENDER_PARAM_SW_SIZE,
                    data: size.as_mut_ptr() as *mut c_void,
                },
                mpv_render_param {
                    type_: MPV_RENDER_PARAM_SW_FORMAT,
                    data: format.as_ptr() as *mut c_void,
                },
                mpv_render_param {
                    type_: MPV_RENDER_PARAM_SW_STRIDE,
                    data: &mut stride as *mut _ as *mut c_void,
                },
                mpv_render_param {
                    type_: MPV_RENDER_PARAM_SW_POINTER,
                    data: buffer.as_mut_ptr() as *mut c_void,
                },
                mpv_render_param {
                    type_: MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                    data: &mut block as *mut _ as *mut c_void,
                },
                mpv_render_param {
                    type_: 0,
                    data: ptr::null_mut(),
                },
            ];

            let err = mpv_render_context_render(self.render_ctx, params.as_mut_ptr());
            if err < 0 {
                return Err(MpvError::RenderFrame(err));
            }
            Ok(())
        }
    }

    /// Returns `true` when playback is currently unpaused.
    pub fn is_playing(&self) -> bool {
        unsafe {
            let mut paused: c_int = 0;
            mpv_get_property(
                self.handle,
                c"pause".as_ptr(),
                mpv_format_MPV_FORMAT_FLAG,
                &mut paused as *mut _ as *mut c_void,
            );
            paused == 0
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.drain_render_context();
        unsafe {
            let val: c_int = if paused { 1 } else { 0 };
            mpv_set_property(
                self.handle,
                c"pause".as_ptr(),
                mpv_format_MPV_FORMAT_FLAG,
                &val as *const _ as *mut c_void,
            );
        }
    }

    pub(crate) fn set_paused_async(&mut self, paused: bool) {
        unsafe {
            let mut val: c_int = if paused { 1 } else { 0 };
            mpv_set_property_async(
                self.handle,
                0,
                c"pause".as_ptr(),
                mpv_format_MPV_FORMAT_FLAG,
                &mut val as *mut _ as *mut c_void,
            );
        }
    }

    pub fn toggle_pause(&mut self) {
        self.drain_render_context();
        unsafe {
            let mut cmd: [*const c_char; 3] = [c"cycle".as_ptr(), c"pause".as_ptr(), ptr::null()];
            mpv_command(self.handle, cmd.as_mut_ptr());
        }
    }

    pub(crate) fn toggle_pause_async(&mut self) {
        unsafe {
            let mut cmd: [*const c_char; 3] = [c"cycle".as_ptr(), c"pause".as_ptr(), ptr::null()];
            mpv_command_async(self.handle, 0, cmd.as_mut_ptr());
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        self.drain_render_context();
        unsafe {
            let sec_str = CString::new(seconds.to_string())
                .expect("floating point number string contains no null bytes");
            let mut cmd: [*const c_char; 4] = [
                c"seek".as_ptr(),
                sec_str.as_ptr(),
                c"relative".as_ptr(),
                ptr::null(),
            ];
            mpv_command(self.handle, cmd.as_mut_ptr());
        }
    }

    pub(crate) fn seek_async(&mut self, seconds: f64) {
        self.drain_render_context();
        unsafe {
            let sec_str = CString::new(seconds.to_string())
                .expect("floating point number string contains no null bytes");
            let mut cmd: [*const c_char; 4] = [
                c"seek".as_ptr(),
                sec_str.as_ptr(),
                c"relative".as_ptr(),
                ptr::null(),
            ];
            mpv_command_async(self.handle, 0, cmd.as_mut_ptr());
        }
    }

    pub fn seek_absolute(&mut self, seconds: f64) {
        self.drain_render_context();
        unsafe {
            let sec_str = CString::new(seconds.to_string())
                .expect("floating point number string contains no null bytes");
            let mut cmd: [*const c_char; 5] = [
                c"seek".as_ptr(),
                sec_str.as_ptr(),
                c"absolute".as_ptr(),
                c"exact".as_ptr(),
                ptr::null(),
            ];
            mpv_command(self.handle, cmd.as_mut_ptr());
        }
    }

    pub(crate) fn seek_absolute_async(&mut self, seconds: f64) {
        self.drain_render_context();
        unsafe {
            let sec_str = CString::new(seconds.to_string())
                .expect("floating point number string contains no null bytes");
            let mut cmd: [*const c_char; 5] = [
                c"seek".as_ptr(),
                sec_str.as_ptr(),
                c"absolute".as_ptr(),
                c"exact".as_ptr(),
                ptr::null(),
            ];
            mpv_command_async(self.handle, 0, cmd.as_mut_ptr());
        }
    }

    pub(crate) fn stop_async(&mut self) {
        self.drain_render_context();
        unsafe {
            let mut cmd: [*const c_char; 2] = [c"stop".as_ptr(), ptr::null()];
            mpv_command_async(self.handle, 0, cmd.as_mut_ptr());
        }
    }

    pub fn set_volume(&mut self, volume: f64) {
        unsafe {
            mpv_set_property(
                self.handle,
                c"volume".as_ptr(),
                mpv_format_MPV_FORMAT_DOUBLE,
                &volume as *const _ as *mut c_void,
            );
        }
    }

    pub(crate) fn set_volume_async(&mut self, mut volume: f64) {
        unsafe {
            mpv_set_property_async(
                self.handle,
                0,
                c"volume".as_ptr(),
                mpv_format_MPV_FORMAT_DOUBLE,
                &mut volume as *mut _ as *mut c_void,
            );
        }
    }

    pub fn set_mute(&mut self, mute: bool) {
        unsafe {
            let val: c_int = if mute { 1 } else { 0 };
            mpv_set_property(
                self.handle,
                c"mute".as_ptr(),
                mpv_format_MPV_FORMAT_FLAG,
                &val as *const _ as *mut c_void,
            );
        }
    }

    pub(crate) fn set_mute_async(&mut self, mute: bool) {
        unsafe {
            let mut val: c_int = if mute { 1 } else { 0 };
            mpv_set_property_async(
                self.handle,
                0,
                c"mute".as_ptr(),
                mpv_format_MPV_FORMAT_FLAG,
                &mut val as *mut _ as *mut c_void,
            );
        }
    }

    pub fn get_volume(&self) -> f64 {
        unsafe {
            let mut vol: f64 = 0.0;
            mpv_get_property(
                self.handle,
                c"volume".as_ptr(),
                mpv_format_MPV_FORMAT_DOUBLE,
                &mut vol as *mut _ as *mut c_void,
            );
            vol
        }
    }

    pub fn get_mute(&self) -> bool {
        unsafe {
            let mut mute: c_int = 0;
            mpv_get_property(
                self.handle,
                c"mute".as_ptr(),
                mpv_format_MPV_FORMAT_FLAG,
                &mut mute as *mut _ as *mut c_void,
            );
            mute != 0
        }
    }

    /// Captures a single (rotated) frame of `path` as raw RGBA, fitted into a
    /// `max_w`×`max_h` box. This is a convenience for thumbnail generation: it
    /// loads the file, pauses playback, polls for the first ready frame (up to
    /// `timeout`), renders it, rotates it per the video's metadata, and stops
    /// the player.
    ///
    /// The returned dimensions are the *rotated* dimensions, and the pixel
    /// data is already orientation-corrected.
    pub fn capture_frame(
        &mut self,
        path: &Path,
        max_w: u32,
        max_h: u32,
        timeout: Duration,
    ) -> Result<(u32, u32, Vec<u8>), MpvError> {
        self.stop();
        self.load_file(path)?;
        self.set_paused(true);

        let start = Instant::now();
        let target_canonical = path.canonicalize().ok();

        while start.elapsed() < timeout {
            // Surfaces the reason a capture is failing (an unsupported codec, a
            // hardware decoder that cannot be opened) instead of a bare timeout.
            self.drain_log_messages();

            if self.has_frame_ready() {
                if let Some(current_p_str) = self.get_current_path() {
                    let current_p = PathBuf::from(current_p_str);
                    let paths_match = current_p == path
                        || target_canonical.as_ref().is_some_and(|tc| {
                            current_p == *tc || current_p.canonicalize().ok().as_ref() == Some(tc)
                        });

                    if paths_match && self.is_video_ready() {
                        let (w, h) = self.get_video_size();
                        if w > 0 && h > 0 {
                            let rotation = self.get_video_rotation();
                            let (eff_w, eff_h) = if rotation.is_swapped() {
                                (h, w)
                            } else {
                                (w, h)
                            };

                            let scale = (max_w as f64 / eff_w as f64)
                                .min(max_h as f64 / eff_h as f64)
                                .min(1.0);
                            let render_w = ((w as f64 * scale) as i32) & !1;
                            let render_h = ((h as f64 * scale) as i32) & !1;

                            if render_w > 0 && render_h > 0 {
                                // Checked size math: hostile video-params dims
                                // would overflow the i32 product; skip the frame
                                // instead of allocating a giant buffer.
                                let Some(buf_size) = crate::worker::rgba_frame_size(
                                    render_w as u32,
                                    render_h as u32,
                                ) else {
                                    self.skip_frame();
                                    continue;
                                };
                                let mut buffer = vec![0u8; buf_size];
                                if self.render_frame(render_w, render_h, &mut buffer).is_ok() {
                                    let (final_w, final_h, final_rgba) = crate::rotate_rgba(
                                        render_w as u32,
                                        render_h as u32,
                                        &buffer,
                                        rotation,
                                    );
                                    self.stop();
                                    return Ok((final_w, final_h, final_rgba));
                                }
                                // Consume the frame even though rendering failed,
                                // so `vo_libmpv` is not left waiting for a render
                                // that will not come.
                                self.skip_frame();
                                continue;
                            }
                        }
                    }
                }
                // Release `ctx->next_frame` whenever a ready frame was not
                // rendered above so `vo_libmpv` does not stall in `draw_frame`.
                self.skip_frame();
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        self.stop();
        Err(MpvError::CaptureTimeout)
    }

    /// Queries the set of file extensions compiled into the underlying FFmpeg
    /// layer of `libmpv`. Returns an empty set if the context cannot be
    /// created or the `demuxer-lavf-list` property cannot be read.
    pub fn query_supported_extensions() -> std::collections::HashSet<String> {
        let mut extensions = std::collections::HashSet::new();
        unsafe {
            let handle = mpv_create();
            if handle.is_null() {
                return extensions;
            }
            Self::set_option(handle, c"vo", "null");
            Self::set_option(handle, c"ao", "null");
            Self::set_option(handle, c"hwdec", "no");
            Self::set_option(handle, c"input-default-bindings", "no");
            if mpv_initialize(handle) >= 0 {
                let mut ptr: *mut c_char = ptr::null_mut();
                let err = mpv_get_property(
                    handle,
                    c"demuxer-lavf-list".as_ptr(),
                    mpv_format_MPV_FORMAT_STRING,
                    &mut ptr as *mut _ as *mut c_void,
                );
                if err >= 0 && !ptr.is_null() {
                    let list_str = CStr::from_ptr(ptr).to_string_lossy();
                    extensions.extend(
                        list_str
                            .lines()
                            .flat_map(|line| line.trim().split(','))
                            .map(|ext| ext.trim().to_lowercase())
                            .filter(|ext| !ext.is_empty()),
                    );
                    mpv_free(ptr as *mut c_void);
                }
            }
            mpv_terminate_destroy(handle);
        }
        extensions
    }
}

impl Drop for MpvContext {
    fn drop(&mut self) {
        self.drain_render_context();
        unsafe {
            let mut cmd: [*const c_char; 2] = [c"stop".as_ptr(), ptr::null()];
            mpv_command(self.handle, cmd.as_mut_ptr());

            mpv_render_context_set_update_callback(self.render_ctx, None, ptr::null_mut());

            // Free the render context BEFORE releasing the wakeup sender:
            // mpv_render_context_free waits for in-flight render work, so a
            // callback already running on mpv's render thread has finished
            // by the time the Box is freed (freeing it first would leave a
            // small use-after-free window for that in-flight invocation).
            mpv_render_context_free(self.render_ctx);

            if !self.callback_context_raw.is_null() {
                let _sender_box =
                    Box::from_raw(self.callback_context_raw as *mut tokio::sync::mpsc::Sender<()>);
            }

            mpv_terminate_destroy(self.handle);
        }
    }
}

/// # Safety
///
/// `cb_ctx` must be a valid pointer to a `tokio::sync::mpsc::Sender<()>` that
/// was previously registered via `mpv_render_context_set_update_callback`.
pub(crate) unsafe extern "C" fn mpv_wakeup_callback(cb_ctx: *mut c_void) {
    let sender = cb_ctx as *const tokio::sync::mpsc::Sender<()>;
    if let Some(tx) = unsafe { sender.as_ref() } {
        let _ = tx.try_send(());
    }
}

/// Whether mpv should also log to the terminal: `RUST_LOG` asking for
/// `debug`/`trace` — the same convention the app's tracing filter uses — or an
/// explicit `MEDIA_SORT_MPV_TERMINAL`.
fn verbose_logging_requested() -> bool {
    let rust_log = std::env::var("RUST_LOG").unwrap_or_default();
    rust_log.contains("debug")
        || rust_log.contains("trace")
        || std::env::var_os("MEDIA_SORT_MPV_TERMINAL").is_some()
}

/// Forwards one mpv log message to `tracing`.
///
/// mpv's levels map down one step: `fatal`/`error` and `warn` are visible by
/// default, while `info` — the per-file detail that names the hwdec mpv picked,
/// the decoder that opened the file, and why an attempt failed — stays behind
/// `RUST_LOG=debug` (or `RUST_LOG=mpv=debug`) so a normal run is not flooded.
fn forward_log_message(msg: &mpv_event_log_message) {
    let text = cstr_lossy(msg.text);
    let text = text.trim_end();
    if text.is_empty() {
        return;
    }
    let prefix = cstr_lossy(msg.prefix);
    let prefix = prefix.trim();
    match cstr_lossy(msg.level).as_str() {
        "fatal" | "error" => tracing::error!(target: "mpv", "[{prefix}] {text}"),
        "warn" => tracing::warn!(target: "mpv", "[{prefix}] {text}"),
        "info" => tracing::debug!(target: "mpv", "[{prefix}] {text}"),
        _ => tracing::trace!(target: "mpv", "[{prefix}] {text}"),
    }
}

/// Reads a libmpv string, tolerating a null pointer (which must not become a
/// dereference) and invalid UTF-8 (mpv message text can quote arbitrary file
/// content).
fn cstr_lossy(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

// libmpv's core is internally synchronized, so it is sound to move the
// context to the thread that owns it (the worker task, the thumbnail worker
// threads). `Send` alone is sound because a moved context is used from
// exactly one thread at a time.
unsafe impl Send for MpvContext {}
// NOTE: there is deliberately no `Sync` impl. The libmpv render API
// (`mpv_render_context_update`/`mpv_render_context_render`, reachable via the
// `&self` methods `has_frame_ready`, `render_frame`, `drain_render_context`)
// must only be called from a single thread, and `Sync` would make it sound to
// share `&MpvContext` across threads without any serialization.

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/MockState"
        ))
        .join(name)
    }

    /// Wall-clock budget for the load and frame waits below.
    ///
    /// Deliberately generous: these tests assert that a frame is *eventually*
    /// produced, not how quick the machine is. nextest runs each test in its
    /// own process, in parallel, and `benchmarks`'
    /// `fixture_gen::tests::test_generate_and_decode_all_format_fixtures`
    /// saturates every core for a minute or two (`rayon` over 15 formats), so
    /// a tight budget here measures the runner's CPU weather instead of the
    /// code -- on a contended 4-vCPU runner the capture below took 5.4s while
    /// taking 0.4s on the same commit elsewhere. Production calls the same
    /// `capture_frame` with a 1000ms budget and retries on `CaptureTimeout`,
    /// so a latency assertion has no place in this test.
    const PLAYER_TIMEOUT: Duration = Duration::from_secs(60);

    fn thumbnail_player() -> Option<MpvContext> {
        match MpvContext::new_thumbnail_player() {
            Ok(player) => Some(player),
            Err(e) => {
                eprintln!("SKIP: MpvContext::new_thumbnail_player() failed: {e}");
                None
            }
        }
    }

    #[test]
    fn test_capture_frame_returns_valid_thumbnail() {
        let Some(mut player) = thumbnail_player() else {
            return;
        };
        let (w, h, rgba) = player
            .capture_frame(&fixture("mock 3.mp4"), 128, 128, PLAYER_TIMEOUT)
            .expect("capture_frame should produce a frame within the timeout");
        assert!(w > 0 && h > 0, "captured frame must have non-zero size");
        assert!(
            w <= 128 && h <= 128,
            "captured frame must fit the 128x128 box"
        );
        assert_eq!(
            rgba.len(),
            (w * h * 4) as usize,
            "rgba len must match w*h*4"
        );
    }

    /// Polls for a renderable frame and renders it into a 128x128-fit box.
    /// Mirrors the production frame pump: wait for a pending frame after the
    /// video output is ready, then render + rotate.
    fn wait_and_render_frame(
        player: &mut MpvContext,
        deadline: Instant,
        label: &str,
    ) -> (u32, u32, Vec<u8>) {
        while Instant::now() < deadline {
            if player.has_frame_ready() {
                if player.is_video_ready() {
                    let (w, h) = player.get_video_size();
                    if w > 0 && h > 0 {
                        let rotation = player.get_video_rotation();
                        let (eff_w, eff_h) = if rotation.is_swapped() {
                            (h, w)
                        } else {
                            (w, h)
                        };
                        let scale = (128.0f64 / eff_w as f64)
                            .min(128.0f64 / eff_h as f64)
                            .min(1.0);
                        let render_w = ((w as f64 * scale) as i32) & !1;
                        let render_h = ((h as f64 * scale) as i32) & !1;
                        if render_w > 0 && render_h > 0 {
                            let mut buffer = vec![0u8; (render_w * render_h * 4) as usize];
                            if player.render_frame(render_w, render_h, &mut buffer).is_ok() {
                                let (final_w, final_h, rgba) = crate::rotate_rgba(
                                    render_w as u32,
                                    render_h as u32,
                                    &buffer,
                                    rotation,
                                );
                                assert!(final_w > 0 && final_h > 0);
                                assert!(final_w <= 128 && final_h <= 128);
                                assert_eq!(rgba.len(), (final_w * final_h * 4) as usize);
                                return (final_w, final_h, rgba);
                            }
                            continue;
                        }
                    }
                }
                player.skip_frame();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for a renderable frame ({label})");
    }

    #[test]
    fn test_seek_then_render_frame() {
        let Some(mut player) = thumbnail_player() else {
            return;
        };
        let path = fixture("mock 3.mp4");

        player.stop();
        player
            .load_file(&path)
            .expect("load_file should succeed for the fixture");
        player.set_paused(true);

        // Wait until the file is loaded and its duration is known.
        let duration = {
            let start = Instant::now();
            loop {
                assert!(
                    start.elapsed() < PLAYER_TIMEOUT,
                    "timed out waiting for the video to load"
                );
                let (w, h) = player.get_video_size();
                let dur = player.get_duration();
                if w > 0 && h > 0 && dur > 0.0 {
                    break dur;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };

        // Render the first frame before seeking: this activates the video
        // output pipeline (on macOS no frame becomes pending until the first
        // render call, so a seek on a never-rendered context stalls).
        wait_and_render_frame(&mut player, Instant::now() + PLAYER_TIMEOUT, "before seek");

        player.seek(duration * 0.1);

        // The seek must not break rendering: poll for the next frame and
        // render it.
        wait_and_render_frame(&mut player, Instant::now() + PLAYER_TIMEOUT, "after seek");

        player.stop();
    }

    /// Hostile dimensions must be rejected by `render_frame` as errors, not
    /// panics: the i32/usize size math is checked (an unchecked
    /// `i32::MAX * i32::MAX * 4` would panic in debug and wrap in release).
    #[test]
    fn test_render_frame_hostile_dims_return_error_not_panic() {
        let Some(player) = thumbnail_player() else {
            return;
        };
        let mut buffer = vec![0u8; 1024];
        assert!(
            player
                .render_frame(i32::MAX, i32::MAX, &mut buffer)
                .is_err(),
            "huge dims must fail with an error, not panic or succeed"
        );
        assert!(
            matches!(
                player.render_frame(0, 100, &mut buffer),
                Err(MpvError::Other(_))
            ),
            "zero dims must be rejected"
        );
        assert!(
            matches!(
                player.render_frame(100, 100, &mut buffer),
                Err(MpvError::BufferTooSmall { .. })
            ),
            "a too-small buffer must keep reporting BufferTooSmall"
        );
    }
}
