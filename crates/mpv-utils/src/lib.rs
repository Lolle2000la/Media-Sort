//! `mpv-utils` — a dependency-light wrapper around `libmpv` for rendering
//! video frames to CPU memory (software rendering).
//!
//! This crate intentionally has **no** `iced`/wgpu dependency. It contains:
//!
//! - [`MpvContext`]: a safe-ish wrapper around a raw libmpv instance with a
//!   software render context, plus helpers like [`MpvContext::capture_frame`]
//!   for thumbnail extraction.
//! - [`worker`]: the `VideoCommand`/`VideoEvent` channel protocol and the
//!   background `tokio` worker that decouples frame pumping from callers.
//! - [`rotation`]: the [`Rotation`] enum and file-based rotation detection
//!   ([`detect_video_rotation`]).
//! - [`rotate_rgba`]: a pure RGBA rotation utility.
//!
//! The GUI-facing `iced-mpv` crate layers iced subscriptions/widgets on top of
//! this crate; `mpv-utils` is also usable directly from non-GUI code (tests,
//! benchmarks) without pulling in a UI framework.

pub mod gl_context;
pub mod gl_interop;
pub mod mpv_context;
pub mod rotation;
pub mod worker;

pub use gl_interop::{GlFboTarget, SharedGpuFrameHandle, create_gl_render_context, render_gl_fbo};
pub use mpv_context::{MpvContext, MpvError};
pub use rotation::{Rotation, detect_video_rotation};
pub use worker::{
    PlayerConfig, VideoCommand, VideoEvent, rotate_rgba, start_video_worker,
    start_video_worker_with,
};
