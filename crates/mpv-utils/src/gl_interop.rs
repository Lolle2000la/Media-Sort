//! OpenGL-to-`wgpu` zero-copy interop context setup and GPU handle export for `libmpv`.
//!
//! Provides [`create_gl_render_context`] (the mpv OpenGL render-context
//! creation used by the zero-copy worker path), [`render_gl_fbo`] and
//! [`SharedGpuFrameHandle`] — the OS-level memory handle handed to the wgpu
//! side.
//!
//! The Linux producer path lives in [`gl_context`](crate::gl_context) (EGL +
//! GBM); this module only bridges mpv's render API to those resources.

use libmpv_sys::*;
use std::ffi::{CString, c_char, c_void};

/// An OS-level GPU memory handle backing an OpenGL Framebuffer Object (FBO)
/// rendered by `libmpv`.
///
/// The handle owns its resources (the DMA-BUF fd on Linux) and closes them on
/// drop; the consumer transfers ownership via [`SharedGpuFrameHandle::into_fd`]
/// when importing into wgpu.
#[derive(Debug, Clone)]
pub enum SharedGpuFrameHandle {
    /// Linux DMA-BUF exported file descriptor (`VK_EXT_external_memory_dma_buf`).
    /// The buffer is `width×height` RGBA8 (`DRM_FORMAT_ABGR8888`).
    #[cfg(target_os = "linux")]
    DmaBuf(DmaBufHandle),
    /// Fallback indicator when hardware zero-copy interop is unavailable.
    SoftwareFallback,
}

#[cfg(target_os = "linux")]
impl SharedGpuFrameHandle {
    /// The DMA-BUF file descriptor for the [`SharedGpuFrameHandle::DmaBuf`]
    /// variant.
    pub fn as_fd(&self) -> Option<std::os::unix::io::RawFd> {
        match self {
            Self::DmaBuf(h) => Some(h.fd),
            Self::SoftwareFallback => None,
        }
    }

    /// Consumes the handle and returns its raw DMA-BUF fd. The caller owns
    /// the fd afterwards (it is not closed by the handle).
    pub fn into_fd(self) -> Option<std::os::unix::io::RawFd> {
        match self {
            Self::DmaBuf(h) => Some(h.into_fd()),
            Self::SoftwareFallback => None,
        }
    }
}

/// A Linux DMA-BUF frame handle that closes its fd on drop.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct DmaBufHandle {
    fd: std::os::unix::io::RawFd,
    width: u32,
    height: u32,
    stride: u32,
    offset: u32,
}

#[cfg(target_os = "linux")]
impl Clone for DmaBufHandle {
    /// Duplicates the fd so every clone owns its own descriptor.
    fn clone(&self) -> Self {
        // SAFETY: fd is a valid descriptor; dup() is thread-safe.
        let dup = unsafe { libc::dup(self.fd) };
        assert!(dup >= 0, "dup() failed while cloning a dma-buf handle");
        Self {
            fd: dup,
            width: self.width,
            height: self.height,
            stride: self.stride,
            offset: self.offset,
        }
    }
}

#[cfg(target_os = "linux")]
impl DmaBufHandle {
    pub fn new(
        fd: std::os::unix::io::RawFd,
        width: u32,
        height: u32,
        stride: u32,
        offset: u32,
    ) -> Self {
        Self {
            fd,
            width,
            height,
            stride,
            offset,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> u32 {
        self.stride
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn into_fd(self) -> std::os::unix::io::RawFd {
        let fd = self.fd;
        std::mem::forget(self);
        fd
    }
}

#[cfg(target_os = "linux")]
impl Drop for DmaBufHandle {
    fn drop(&mut self) {
        // SAFETY: fd is a valid file descriptor owned by this handle.
        unsafe { libc::close(self.fd) };
    }
}

unsafe impl Send for SharedGpuFrameHandle {}
unsafe impl Sync for SharedGpuFrameHandle {}

/// Creates a `libmpv` render context initialized for OpenGL rendering (`MPV_RENDER_API_TYPE_OPENGL`).
///
/// # Safety
///
/// `mpv` must be a valid, initialized `mpv_handle`. `get_proc_addr` must be a valid C function
/// pointer for resolving OpenGL extension function addresses in the current OpenGL context.
pub unsafe fn create_gl_render_context(
    mpv: *mut mpv_handle,
    get_proc_addr: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    get_proc_addr_ctx: *mut c_void,
) -> Result<*mut mpv_render_context, crate::MpvError> {
    if mpv.is_null() {
        return Err(crate::MpvError::CreateFailed);
    }
    let mut gl_init_params = mpv_opengl_init_params {
        get_proc_address: Some(get_proc_addr),
        get_proc_address_ctx: get_proc_addr_ctx,
        extra_exts: std::ptr::null(),
    };

    let api_type = CString::new("opengl").map_err(|_| crate::MpvError::InvalidCString)?;
    let mut params = [
        mpv_render_param {
            type_: mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
            data: api_type.as_ptr() as *mut _,
        },
        mpv_render_param {
            type_: mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
            data: &mut gl_init_params as *mut _ as *mut _,
        },
        mpv_render_param {
            type_: 0,
            data: std::ptr::null_mut(),
        },
    ];

    let mut render_ctx: *mut mpv_render_context = std::ptr::null_mut();
    let err = unsafe { mpv_render_context_create(&mut render_ctx, mpv, params.as_mut_ptr()) };
    if err < 0 {
        Err(crate::MpvError::RenderContextCreate(err))
    } else {
        Ok(render_ctx)
    }
}

/// Offscreen OpenGL FBO render configuration for `libmpv`.
#[derive(Debug, Clone, Copy)]
pub struct GlFboTarget {
    pub fbo_id: u32,
    pub width: u32,
    pub height: u32,
    pub internal_format: u32,
}

/// Renders a single frame from `libmpv` into an offscreen OpenGL FBO target.
///
/// # Safety
///
/// `render_ctx` must be a valid `mpv_render_context` created with OpenGL API type.
/// The calling thread must have the target OpenGL context bound as current.
pub unsafe fn render_gl_fbo(
    render_ctx: *mut mpv_render_context,
    target: GlFboTarget,
) -> Result<(), crate::MpvError> {
    if render_ctx.is_null() {
        return Err(crate::MpvError::NullRenderContext);
    }
    let mut fbo = mpv_opengl_fbo {
        fbo: target.fbo_id as i32,
        w: target.width as i32,
        h: target.height as i32,
        internal_format: target.internal_format as i32,
    };

    let mut params = [
        mpv_render_param {
            type_: mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
            data: &mut fbo as *mut _ as *mut c_void,
        },
        mpv_render_param {
            type_: 0,
            data: std::ptr::null_mut(),
        },
    ];

    let err = unsafe { mpv_render_context_render(render_ctx, params.as_mut_ptr()) };
    if err < 0 {
        Err(crate::MpvError::RenderFrame(err))
    } else {
        Ok(())
    }
}
