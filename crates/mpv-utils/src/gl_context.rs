//! Offscreen EGL + GBM context management for the zero-copy Linux path.
//!
//! Everything is loaded at runtime via `dlopen` (`libEGL.so.1`, `libgbm.so.1`)
//! so the crate never gains a link-time dependency on the Mesa dev packages:
//! machines without EGL simply fail context creation and the worker falls back
//! to software rendering.
//!
//! The zero-copy pipeline renders into a GBM-allocated DMA-BUF:
//!
//! 1. [`GbmDevice::allocate`] allocates a linear [`GbmBo`] (exposed as a
//!    `RawFd` via [`GbmBo::fd`]).
//! 2. [`GlContext::texture_from_dmabuf`] wraps that fd in an `EGLImage`
//!    (`EGL_EXT_image_dma_buf_import`) and binds it as a GL texture attached
//!    to an FBO; mpv renders into the FBO with `internal_format = GL_RGBA8`.
//! 3. [`GlContext::finish`] (`glFinish`) guarantees the write completed before
//!    the fd is handed to the wgpu side.
//!
//! EGL/GL calls must be serialized: a [`GlContext`] is not `Sync`, and mpv's
//! render API additionally requires the context to be current on the thread
//! calling `mpv_render_context_render`, which the worker guarantees.

#![cfg(target_os = "linux")]

use std::ffi::{CStr, CString};
use std::os::unix::io::RawFd;
use std::sync::Arc;

use libc::{c_char, c_int, c_void};

// ---------------------------------------------------------------------------
// EGL constants
// ---------------------------------------------------------------------------

pub type EGLDisplay = *mut c_void;
pub type EGLContext = *mut c_void;
pub type EGLConfig = *const c_void;
pub type EGLImage = *mut c_void;
pub type EGLenum = u32;
pub type EGLint = i32;
pub type EGLBoolean = u32;

pub const EGL_NO_DISPLAY: EGLDisplay = std::ptr::null_mut();
pub const EGL_NO_CONTEXT: EGLContext = std::ptr::null_mut();
pub const EGL_DEFAULT_DISPLAY: *mut c_void = std::ptr::null_mut();

const EGL_SURFACE_TYPE: EGLint = 0x3033;
const EGL_COLOR_BUFFER_TYPE: EGLint = 0x303f;
const EGL_RED_SIZE: EGLint = 0x3024;
const EGL_ALPHA_SIZE: EGLint = 0x3021;
const EGL_PBUFFER_BIT: EGLint = 0x0001;
const EGL_RGB_BUFFER: EGLint = 0x308e;
const EGL_NONE: EGLint = 0x3038;
const EGL_OPENGL_API: EGLenum = 0x30a2;
const EGL_CONTEXT_MAJOR_VERSION: EGLint = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: EGLint = 0x30fb;

const EGL_LINUX_DMA_BUF_EXT: EGLenum = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: EGLenum = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: EGLenum = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: EGLenum = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: EGLenum = 0x3274;
const EGL_WIDTH: EGLint = 0x3057;
const EGL_HEIGHT: EGLint = 0x3056;

/// `DRM_FORMAT_ABGR8888` (`fourcc_code('A','B','2','4')`, 0x34324241) — the
/// byte order of an `R8G8B8A8_UNORM` wgpu texture viewed as a little-endian
/// u32: pixel bytes R,G,B,A == DRM "ABGR" (R in the most significant byte).
/// This is what `internal_format = GL_RGBA8` FBOs export on Mesa.
pub const DRM_FORMAT_ABGR8888: u32 = 0x3432_4241;
/// `DRM_FORMAT_MOD_LINEAR` — we allocate linear buffers so Vulkan can import
/// them without modifier-tiling surprises.
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

const _: () = assert!(DRM_FORMAT_ABGR8888 == u32::from_le_bytes(*b"AB24"));

// GL constants used by this module.
const GL_TEXTURE_2D: u32 = 0x0de1;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_LINEAR: i32 = 0x2601;
const GL_FRAMEBUFFER: u32 = 0x8d40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8ce0;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8cd5;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_MAJOR_VERSION: u32 = 0x821b;
const GL_UNPACK_ROW_LENGTH: u32 = 0x0cf2;

/// `GL_RGBA8` — the internal format mpv renders into (the same byte order as
/// `DRM_FORMAT_ABGR8888` DMA-BUFs and wgpu's `R8G8B8A8_UNORM`).
pub const GL_RGBA8: u32 = 0x8058;

// ---------------------------------------------------------------------------
// dlopen helpers
// ---------------------------------------------------------------------------

struct DlHandle(*mut c_void);

impl DlHandle {
    fn open(name: &str) -> Result<Self, String> {
        let cname =
            CString::new(name).map_err(|_| "library name contains a NUL byte".to_string())?;
        // SAFETY: dlopen is thread-safe and takes a valid C string.
        let handle = unsafe { libc::dlopen(cname.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            Err(dlerror_string())
        } else {
            Ok(Self(handle))
        }
    }

    fn symbol(&self, name: &str) -> Option<*mut c_void> {
        let cname = CString::new(name).ok()?;
        // SAFETY: dlsym is thread-safe and only reads the C string.
        let sym = unsafe { libc::dlsym(self.0, cname.as_ptr()) };
        if sym.is_null() { None } else { Some(sym) }
    }
}

impl Drop for DlHandle {
    fn drop(&mut self) {
        // SAFETY: dlclose on a handle returned by dlopen.
        unsafe { libc::dlclose(self.0) };
    }
}

fn dlerror_string() -> String {
    // SAFETY: dlerror returns a thread-local NUL-terminated string or NULL.
    let ptr = unsafe { libc::dlerror() };
    if ptr.is_null() {
        "unknown dynamic loader error".to_string()
    } else {
        // SAFETY: ptr is a valid NUL-terminated C string.
        unsafe { CStr::from_ptr(ptr as *const c_char) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Loads a `libc::dlsym`-resolved symbol as a typed function pointer.
unsafe fn load_fn<F>(handle: &DlHandle, name: &str) -> Option<F> {
    let sym = handle.symbol(name)?;
    // SAFETY: the caller guarantees the symbol actually has signature F (a
    // function pointer, pointer-sized). transmute_copy reads size_of::<F>()
    // bytes from a pointer-sized local.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&sym) })
}

/// Loads a symbol through `eglGetProcAddress` (used for core GL functions).
unsafe fn load_gl_fn<F>(
    get_proc_addr: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    name: &str,
) -> Result<F, String> {
    let cname = CString::new(name).map_err(|_| "name contains a NUL byte".to_string())?;
    // SAFETY: eglGetProcAddress reads a NUL-terminated name.
    let sym = unsafe { get_proc_addr(cname.as_ptr()) };
    if sym.is_null() {
        Err(format!("eglGetProcAddress(\"{name}\") returned null"))
    } else {
        // SAFETY: the caller guarantees the symbol has signature F (a
        // function pointer, pointer-sized).
        Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&sym) })
    }
}

// ---------------------------------------------------------------------------
// EGL
// ---------------------------------------------------------------------------

#[allow(clippy::type_complexity)]
struct Egl {
    _lib: DlHandle,
    get_display: unsafe extern "C" fn(*mut c_void) -> EGLDisplay,
    initialize: unsafe extern "C" fn(EGLDisplay, *mut EGLint, *mut EGLint) -> EGLBoolean,
    choose_config: unsafe extern "C" fn(
        EGLDisplay,
        *const EGLint,
        *mut EGLConfig,
        EGLint,
        *mut EGLint,
    ) -> EGLBoolean,
    bind_api: unsafe extern "C" fn(EGLenum) -> EGLBoolean,
    create_context:
        unsafe extern "C" fn(EGLDisplay, EGLConfig, EGLContext, *const EGLint) -> EGLContext,
    make_current:
        unsafe extern "C" fn(EGLDisplay, EGLContext, EGLContext, EGLContext) -> EGLBoolean,
    destroy_context: unsafe extern "C" fn(EGLDisplay, EGLContext) -> EGLBoolean,
    terminate: unsafe extern "C" fn(EGLDisplay) -> EGLBoolean,
    create_pbuffer_surface:
        unsafe extern "C" fn(EGLDisplay, EGLConfig, *const EGLint) -> EGLDisplay,
    destroy_surface: unsafe extern "C" fn(EGLDisplay, EGLDisplay) -> EGLBoolean,
    get_error: unsafe extern "C" fn() -> EGLint,
    get_proc_address: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    create_image: Option<
        unsafe extern "C" fn(
            EGLDisplay,
            EGLContext,
            EGLenum,
            EGLint, // client buffer: the fd for EGL_LINUX_DMA_BUF_EXT
            *const EGLint,
        ) -> EGLImage,
    >,
    destroy_image: Option<unsafe extern "C" fn(EGLDisplay, EGLImage) -> EGLBoolean>,
}

impl Egl {
    fn load() -> Result<Self, String> {
        let lib = DlHandle::open("libEGL.so.1")?;
        // SAFETY: the symbol signatures are fixed by the EGL spec.
        unsafe {
            let get_display = load_fn(&lib, "eglGetDisplay")
                .ok_or_else(|| "libEGL.so.1 missing eglGetDisplay".to_string())?;
            let initialize = load_fn(&lib, "eglInitialize")
                .ok_or_else(|| "libEGL.so.1 missing eglInitialize".to_string())?;
            let choose_config = load_fn(&lib, "eglChooseConfig")
                .ok_or_else(|| "libEGL.so.1 missing eglChooseConfig".to_string())?;
            let bind_api = load_fn(&lib, "eglBindAPI")
                .ok_or_else(|| "libEGL.so.1 missing eglBindAPI".to_string())?;
            let create_context = load_fn(&lib, "eglCreateContext")
                .ok_or_else(|| "libEGL.so.1 missing eglCreateContext".to_string())?;
            let make_current = load_fn(&lib, "eglMakeCurrent")
                .ok_or_else(|| "libEGL.so.1 missing eglMakeCurrent".to_string())?;
            let destroy_context = load_fn(&lib, "eglDestroyContext")
                .ok_or_else(|| "libEGL.so.1 missing eglDestroyContext".to_string())?;
            let terminate = load_fn(&lib, "eglTerminate")
                .ok_or_else(|| "libEGL.so.1 missing eglTerminate".to_string())?;
            let create_pbuffer_surface = load_fn(&lib, "eglCreatePbufferSurface")
                .ok_or_else(|| "libEGL.so.1 missing eglCreatePbufferSurface".to_string())?;
            let destroy_surface = load_fn(&lib, "eglDestroySurface")
                .ok_or_else(|| "libEGL.so.1 missing eglDestroySurface".to_string())?;
            let get_error = load_fn(&lib, "eglGetError")
                .ok_or_else(|| "libEGL.so.1 missing eglGetError".to_string())?;
            let get_proc_address = load_fn(&lib, "eglGetProcAddress")
                .ok_or_else(|| "libEGL.so.1 missing eglGetProcAddress".to_string())?;
            // eglCreateImageKHR / eglDestroyImageKHR are extension functions:
            // they are only reachable through eglGetProcAddress, not dlsym.
            let create_image = load_gl_fn::<
                unsafe extern "C" fn(
                    EGLDisplay,
                    EGLContext,
                    EGLenum,
                    EGLint,
                    *const EGLint,
                ) -> EGLImage,
            >(get_proc_address, "eglCreateImageKHR")
            .ok();
            let destroy_image = load_gl_fn::<
                unsafe extern "C" fn(EGLDisplay, EGLImage) -> EGLBoolean,
            >(get_proc_address, "eglDestroyImageKHR")
            .ok();

            Ok(Self {
                _lib: lib,
                get_display,
                initialize,
                choose_config,
                bind_api,
                create_context,
                make_current,
                destroy_context,
                terminate,
                create_pbuffer_surface,
                destroy_surface,
                get_error,
                get_proc_address,
                create_image,
                destroy_image,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// GBM
// ---------------------------------------------------------------------------
//
// NOTE: libgbm is LINKED (not dlopen'd). Mesa's GBM backend dynamically loads
// the DRI driver inside gbm_bo_create, and that driver's symbol resolution
// only works when libgbm is loaded at process start (as a DT_NEEDED link);
// a late dlopen of libgbm.so.1 makes gbm_bo_create fail with EINVAL. This was
// verified empirically on radeonsi (see `crates/mpv-utils/tests/gl_spike_tests.rs`).
// Build-time requirement: libgbm.so (the `libgbm-dev` package on Debian/Ubuntu);
// the runtime library libgbm.so.1 ships with any Mesa installation.

type GbmDevicePtr = *mut c_void;
type GbmBoPtr = *mut c_void;

#[cfg(target_os = "linux")]
#[link(name = "gbm")]
unsafe extern "C" {
    fn gbm_create_device(fd: c_int) -> GbmDevicePtr;
    fn gbm_device_destroy(dev: GbmDevicePtr);
    fn gbm_device_is_format_supported(dev: GbmDevicePtr, format: u32, usage: u32) -> c_int;
    fn gbm_bo_create(dev: GbmDevicePtr, w: u32, h: u32, format: u32, usage: u32) -> GbmBoPtr;
    fn gbm_bo_create_with_modifiers(
        dev: GbmDevicePtr,
        w: u32,
        h: u32,
        format: u32,
        modifiers: *const u64,
        count: u32,
    ) -> GbmBoPtr;
    fn gbm_bo_get_fd(bo: GbmBoPtr) -> c_int;
    fn gbm_bo_get_stride(bo: GbmBoPtr) -> u32;
    fn gbm_bo_destroy(bo: GbmBoPtr);
}

const GBM_BO_USE_RENDERING: u32 = 1 << 2;
const GBM_FORMAT_ABGR8888: u32 = DRM_FORMAT_ABGR8888;

fn open_render_node() -> Result<RawFd, String> {
    for minor in 128..192 {
        let path = format!("/dev/dri/renderD{minor}");
        // SAFETY: open takes a valid NUL-terminated path.
        let fd = unsafe {
            libc::open(
                CString::new(path.as_str()).unwrap().as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC,
            )
        };
        if fd >= 0 {
            return Ok(fd);
        }
    }
    Err("no usable /dev/dri/renderD* node (is a GPU driver loaded?)".to_string())
}

struct GbmDeviceInner {
    raw: GbmDevicePtr,
    fd: RawFd,
}

// SAFETY: the raw `gbm_device` is used from a single worker thread at a time
// (the same thread that owns the render context), and the Arc clones are only
// shared within that thread's call stack (a bo keeps its device alive). The
// pointer itself is opaque and never dereferenced.
unsafe impl Send for GbmDeviceInner {}
unsafe impl Sync for GbmDeviceInner {}

impl Drop for GbmDeviceInner {
    fn drop(&mut self) {
        // SAFETY: raw is a valid GBM device; destroy it before the fd closes.
        unsafe { gbm_device_destroy(self.raw) };
        // SAFETY: close the render-node fd.
        unsafe { libc::close(self.fd) };
    }
}

/// A GBM device (owns the render-node fd and the `gbm_device`). Cheap to
/// clone; the underlying device lives until the last clone and its bo's drop.
#[derive(Clone)]
pub struct GbmDevice {
    inner: Arc<GbmDeviceInner>,
}

impl GbmDevice {
    pub fn new() -> Result<Self, String> {
        let fd = open_render_node()?;
        // SAFETY: fd is a valid render-node fd.
        let raw = unsafe { gbm_create_device(fd) };
        if raw.is_null() {
            // SAFETY: close a valid fd.
            unsafe { libc::close(fd) };
            return Err("gbm_create_device failed".to_string());
        }
        Ok(Self {
            inner: Arc::new(GbmDeviceInner { raw, fd }),
        })
    }

    pub fn is_format_supported(&self, format: u32, usage: u32) -> bool {
        // SAFETY: self.inner.raw is a valid GBM device.
        unsafe { gbm_device_is_format_supported(self.inner.raw, format, usage) == 1 }
    }

    /// Allocates a linear `width×height` buffer in `DRM_FORMAT_ABGR8888`.
    pub fn allocate(&self, width: u32, height: u32) -> Result<GbmBo, String> {
        let usage = GBM_BO_USE_RENDERING;
        // Prefer the LINEAR modifier explicitly; fall back to the driver
        // default. The allocation itself is the source of truth — if it fails
        // the caller falls back to software rendering.
        let raw = {
            // SAFETY: standard GBM call with a valid device + LINEAR modifier.
            let bo = unsafe {
                gbm_bo_create_with_modifiers(
                    self.inner.raw,
                    width,
                    height,
                    GBM_FORMAT_ABGR8888,
                    &DRM_FORMAT_MOD_LINEAR,
                    1,
                )
            };
            if bo.is_null() {
                // SAFETY: standard GBM call without modifiers.
                unsafe { gbm_bo_create(self.inner.raw, width, height, GBM_FORMAT_ABGR8888, usage) }
            } else {
                bo
            }
        };
        if raw.is_null() {
            return Err("gbm_bo_create(_with_modifiers) failed".to_string());
        }
        Ok(GbmBo {
            _device: self.clone(),
            raw,
            width,
            height,
        })
    }
}

/// A GBM buffer object. Owns the `gbm_bo`; the DMA-BUF fd is exported on
/// demand via [`GbmBo::fd`].
pub struct GbmBo {
    /// Keeps the `gbm_device` alive while this bo exists (bo must be
    /// destroyed before the device).
    _device: GbmDevice,
    raw: GbmBoPtr,
    width: u32,
    height: u32,
}

impl GbmBo {
    /// Exports a **new** DMA-BUF fd for this buffer (caller owns it; close
    /// with `libc::close`). The underlying buffer stays alive until the
    /// `GbmBo` is dropped.
    pub fn fd(&self) -> Result<RawFd, String> {
        // SAFETY: self.raw is a valid GBM bo.
        let fd = unsafe { gbm_bo_get_fd(self.raw) };
        if fd < 0 {
            Err("gbm_bo_get_fd failed".to_string())
        } else {
            Ok(fd)
        }
    }

    pub fn stride(&self) -> u32 {
        // SAFETY: self.raw is a valid GBM bo.
        unsafe { gbm_bo_get_stride(self.raw) }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

impl Drop for GbmBo {
    fn drop(&mut self) {
        // SAFETY: self.raw is a valid GBM bo created on self.device.
        unsafe { gbm_bo_destroy(self.raw) };
    }
}

// SAFETY: the raw gbm_bo is used from a single worker thread at a time; the
// pointer is opaque and never dereferenced. Needed so the worker future is
// Send (a GbmBo is held across awaits in the ring).
unsafe impl Send for GbmBo {}

// ---------------------------------------------------------------------------
// GL function pointers
// ---------------------------------------------------------------------------

#[allow(clippy::type_complexity)]
struct Gl {
    gen_textures: unsafe extern "C" fn(u32, *mut u32),
    bind_texture: unsafe extern "C" fn(u32, u32),
    delete_textures: unsafe extern "C" fn(u32, *const u32),
    tex_parameteri: unsafe extern "C" fn(u32, u32, i32),
    pixel_storei: unsafe extern "C" fn(u32, i32),
    gen_framebuffers: unsafe extern "C" fn(u32, *mut u32),
    bind_framebuffer: unsafe extern "C" fn(u32, u32),
    delete_framebuffers: unsafe extern "C" fn(u32, *const u32),
    framebuffer_texture_2d: unsafe extern "C" fn(u32, u32, u32, u32, i32),
    check_framebuffer_status: unsafe extern "C" fn(u32) -> u32,
    viewport: unsafe extern "C" fn(i32, i32, i32, i32),
    clear_color: unsafe extern "C" fn(f32, f32, f32, f32),
    clear: unsafe extern "C" fn(u32),
    finish: unsafe extern "C" fn(),
    get_integerv: unsafe extern "C" fn(u32, *mut i32),
    egl_image_target_texture_2d: Option<unsafe extern "C" fn(u32, EGLImage)>,
}

impl Gl {
    fn load(egl: &Egl) -> Result<Self, String> {
        let get_proc_addr = egl.get_proc_address;
        // SAFETY: the GL function signatures are fixed by the GL spec.
        let egl_image_target = unsafe {
            load_gl_fn::<unsafe extern "C" fn(u32, EGLImage)>(
                get_proc_addr,
                "glEGLImageTargetTexture2DOES",
            )
            .ok()
        };
        Ok(Self {
            gen_textures: unsafe { load_gl_fn(get_proc_addr, "glGenTextures")? },
            bind_texture: unsafe { load_gl_fn(get_proc_addr, "glBindTexture")? },
            delete_textures: unsafe { load_gl_fn(get_proc_addr, "glDeleteTextures")? },
            tex_parameteri: unsafe { load_gl_fn(get_proc_addr, "glTexParameteri")? },
            pixel_storei: unsafe { load_gl_fn(get_proc_addr, "glPixelStorei")? },
            gen_framebuffers: unsafe { load_gl_fn(get_proc_addr, "glGenFramebuffers")? },
            bind_framebuffer: unsafe { load_gl_fn(get_proc_addr, "glBindFramebuffer")? },
            delete_framebuffers: unsafe { load_gl_fn(get_proc_addr, "glDeleteFramebuffers")? },
            framebuffer_texture_2d: unsafe { load_gl_fn(get_proc_addr, "glFramebufferTexture2D")? },
            check_framebuffer_status: unsafe {
                load_gl_fn(get_proc_addr, "glCheckFramebufferStatus")?
            },
            viewport: unsafe { load_gl_fn(get_proc_addr, "glViewport")? },
            clear_color: unsafe { load_gl_fn(get_proc_addr, "glClearColor")? },
            clear: unsafe { load_gl_fn(get_proc_addr, "glClear")? },
            finish: unsafe { load_gl_fn(get_proc_addr, "glFinish")? },
            get_integerv: unsafe { load_gl_fn(get_proc_addr, "glGetIntegerv")? },
            egl_image_target_texture_2d: egl_image_target,
        })
    }
}

// ---------------------------------------------------------------------------
// GlContext
// ---------------------------------------------------------------------------

/// A single offscreen EGL + GL context. Not `Sync`: all EGL/GL calls must be
/// made from one thread at a time (serialize with [`GlContext::make_current`]).
pub struct GlContext {
    egl: Egl,
    display: EGLDisplay,
    context: EGLContext,
    config: EGLConfig,
    /// A 1×1 pbuffer used as the current draw/read surface: surfaceless
    /// contexts (`EGL_KHR_surfaceless_context`) are not available on every
    /// platform (e.g. the X11 default display returns `EGL_BAD_SURFACE` for a
    /// null-surface `eglMakeCurrent`), but a tiny pbuffer works everywhere.
    /// Lazily created on first `make_current`.
    surface: std::cell::Cell<EGLDisplay>,
    gl: Gl,
    /// EGLImages wrapping imported DMA-BUFs; destroyed at teardown. Keeping
    /// them alive is required: destroying an EGLImage releases the backing
    /// buffer, which would invalidate the GL texture bound to it.
    images: Vec<EGLImage>,
}

impl GlContext {
    /// Creates a surfaceless OpenGL core context (4.5, falling back to 3.0).
    pub fn new() -> Result<Self, String> {
        let egl = Egl::load()?;

        // SAFETY: EGL calls are safe with the loaded function pointers.
        unsafe {
            let display = (egl.get_display)(EGL_DEFAULT_DISPLAY);
            if display == EGL_NO_DISPLAY {
                return Err("eglGetDisplay failed".to_string());
            }
            let mut major = 0;
            let mut minor = 0;
            if (egl.initialize)(display, &mut major, &mut minor) == 0 {
                return Err(format!(
                    "eglInitialize failed (error {})",
                    (egl.get_error)()
                ));
            }

            let config_attrs: [EGLint; 9] = [
                EGL_SURFACE_TYPE,
                EGL_PBUFFER_BIT,
                EGL_COLOR_BUFFER_TYPE,
                EGL_RGB_BUFFER,
                EGL_RED_SIZE,
                8,
                EGL_ALPHA_SIZE,
                8,
                EGL_NONE,
            ];
            let mut config: EGLConfig = std::ptr::null();
            let mut num_configs: EGLint = 0;
            if (egl.choose_config)(
                display,
                config_attrs.as_ptr(),
                &mut config,
                1,
                &mut num_configs,
            ) == 0
                || num_configs < 1
            {
                return Err(format!(
                    "eglChooseConfig failed (error {})",
                    (egl.get_error)()
                ));
            }

            if (egl.bind_api)(EGL_OPENGL_API) == 0 {
                return Err(format!("eglBindAPI failed (error {})", (egl.get_error)()));
            }

            // Try OpenGL 4.5 core first, then fall back to 3.0 (both work with
            // EGL_KHR_surfaceless_context on Mesa).
            let context = {
                let attrs_45: [EGLint; 5] = [
                    EGL_CONTEXT_MAJOR_VERSION,
                    4,
                    EGL_CONTEXT_MINOR_VERSION,
                    5,
                    EGL_NONE,
                ];
                let c = (egl.create_context)(display, config, EGL_NO_CONTEXT, attrs_45.as_ptr());
                if !c.is_null() {
                    c
                } else {
                    let attrs_30: [EGLint; 3] = [EGL_CONTEXT_MAJOR_VERSION, 3, EGL_NONE];
                    let c =
                        (egl.create_context)(display, config, EGL_NO_CONTEXT, attrs_30.as_ptr());
                    if c.is_null() {
                        return Err(format!(
                            "eglCreateContext failed (error {})",
                            (egl.get_error)()
                        ));
                    }
                    c
                }
            };

            // Create a 1×1 pbuffer draw surface and make the context current.
            // Surfaceless contexts are not available on every EGL platform.
            let surface_attrs: [EGLint; 5] = [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE];
            let surface = (egl.create_pbuffer_surface)(display, config, surface_attrs.as_ptr());
            if surface.is_null() {
                return Err(format!(
                    "eglCreatePbufferSurface failed (error {})",
                    (egl.get_error)()
                ));
            }
            if (egl.make_current)(display, surface, surface, context) == 0 {
                return Err(format!(
                    "eglMakeCurrent failed (error {})",
                    (egl.get_error)()
                ));
            }

            let gl = Gl::load(&egl)?;

            // Verify we actually have a GL context.
            let mut version_major: i32 = 0;
            (gl.get_integerv)(GL_MAJOR_VERSION, &mut version_major);
            if version_major < 3 {
                return Err(format!("unexpected GL major version {version_major}"));
            }

            Ok(Self {
                egl,
                display,
                context,
                config,
                surface: std::cell::Cell::new(surface),
                gl,
                images: Vec::new(),
            })
        }
    }

    /// Creates (or recreates) the pbuffer draw surface and makes the context
    /// current on the calling thread.
    fn ensure_surface_and_current(&self) -> Result<(), String> {
        // SAFETY: EGL calls are safe with the loaded function pointers.
        unsafe {
            let surface = if self.surface.get().is_null() {
                let attrs: [EGLint; 5] = [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE];
                let s =
                    (self.egl.create_pbuffer_surface)(self.display, self.config, attrs.as_ptr());
                if s.is_null() {
                    return Err(format!(
                        "eglCreatePbufferSurface failed (error {})",
                        (self.egl.get_error)()
                    ));
                }
                self.surface.set(s);
                s
            } else {
                self.surface.get()
            };
            if (self.egl.make_current)(self.display, surface, surface, self.context) == 0 {
                return Err(format!(
                    "eglMakeCurrent failed (error {})",
                    (self.egl.get_error)()
                ));
            }
            Ok(())
        }
    }

    /// Makes this context current on the calling thread (creating the 1×1
    /// pbuffer draw surface on first use).
    pub fn make_current(&self) -> Result<(), String> {
        self.ensure_surface_and_current()
    }

    /// A `get_proc_address` trampoline for mpv: `ctx` must point to this
    /// context's EGL function table (see [`GlContext::mpv_proc_addr_ctx`]).
    ///
    /// # Safety
    ///
    /// `ctx` must be a valid pointer to an `Egl` function table that outlives
    /// the call; `name` must be a valid NUL-terminated C string.
    pub unsafe extern "C" fn mpv_get_proc_address(
        ctx: *mut c_void,
        name: *const c_char,
    ) -> *mut c_void {
        let egl = unsafe { &*(ctx as *const Egl) };
        unsafe { (egl.get_proc_address)(name) }
    }

    /// The context pointer to hand to mpv together with
    /// [`GlContext::mpv_get_proc_address`]. Valid for `self`'s lifetime.
    pub fn mpv_proc_addr_ctx(&self) -> *mut c_void {
        &self.egl as *const Egl as *mut c_void
    }

    /// Binds a DMA-BUF fd as a GL texture attached to a fresh FBO, returning
    /// `(texture_id, fbo_id)`. The context must be current. The buffer format
    /// must be [`DRM_FORMAT_ABGR8888`]; the (multiples-of-4) stride is passed
    /// through to EGL.
    pub fn texture_from_dmabuf(
        &mut self,
        fd: RawFd,
        width: u32,
        height: u32,
        stride: u32,
        drm_format: u32,
    ) -> Result<(u32, u32), String> {
        if drm_format != DRM_FORMAT_ABGR8888 {
            return Err(format!("unsupported drm_format 0x{drm_format:08x}"));
        }
        let Some(create_image) = self.egl.create_image else {
            return Err("eglCreateImageKHR is not available".to_string());
        };
        let Some(destroy_image) = self.egl.destroy_image else {
            return Err("eglDestroyImageKHR is not available".to_string());
        };

        // SAFETY: EGL image import from a valid fd, on the current context.
        let attrs: [EGLint; 13] = [
            EGL_WIDTH,
            width as EGLint,
            EGL_HEIGHT,
            height as EGLint,
            EGL_LINUX_DRM_FOURCC_EXT as EGLint,
            drm_format as EGLint,
            EGL_DMA_BUF_PLANE0_FD_EXT as EGLint,
            fd as EGLint,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT as EGLint,
            0,
            EGL_DMA_BUF_PLANE0_PITCH_EXT as EGLint,
            stride as EGLint,
            EGL_NONE,
        ];
        let image = unsafe {
            create_image(
                self.display,
                EGL_NO_CONTEXT,
                EGL_LINUX_DMA_BUF_EXT,
                0,
                attrs.as_ptr(),
            )
        };
        if image.is_null() {
            return Err(format!(
                "eglCreateImageKHR failed for dma-buf (error {})",
                unsafe { (self.egl.get_error)() }
            ));
        }

        let mut tex = 0u32;
        let mut fbo = 0u32;
        unsafe {
            (self.gl.gen_textures)(1, &mut tex);
            (self.gl.bind_texture)(GL_TEXTURE_2D, tex);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (self.gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            (self.gl.pixel_storei)(GL_UNPACK_ROW_LENGTH, 0);

            if let Some(egl_image_target) = self.gl.egl_image_target_texture_2d {
                (egl_image_target)(GL_TEXTURE_2D, image);
            } else {
                (destroy_image)(self.display, image);
                (self.gl.delete_textures)(1, &tex);
                return Err("glEGLImageTargetTexture2DOES is not available".to_string());
            }

            (self.gl.gen_framebuffers)(1, &mut fbo);
            (self.gl.bind_framebuffer)(GL_FRAMEBUFFER, fbo);
            (self.gl.framebuffer_texture_2d)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                tex,
                0,
            );

            let status = (self.gl.check_framebuffer_status)(GL_FRAMEBUFFER);
            if status != GL_FRAMEBUFFER_COMPLETE {
                (self.gl.delete_framebuffers)(1, &fbo);
                (self.gl.delete_textures)(1, &tex);
                (destroy_image)(self.display, image);
                return Err(format!("FBO incomplete (status 0x{status:x})"));
            }
        }

        // The GL texture references the EGLImage, so keep it alive for the
        // lifetime of this context (destroyed in Drop).
        self.images.push(image);
        Ok((tex, fbo))
    }

    /// Clears the given FBO to `color` and issues `glFinish`, guaranteeing the
    /// DMA-BUF write is visible before the fd is handed to Vulkan.
    pub fn clear_and_finish(&self, fbo: u32, width: u32, height: u32, color: [f32; 4]) {
        unsafe {
            (self.gl.bind_framebuffer)(GL_FRAMEBUFFER, fbo);
            (self.gl.viewport)(0, 0, width as i32, height as i32);
            (self.gl.clear_color)(color[0], color[1], color[2], color[3]);
            (self.gl.clear)(GL_COLOR_BUFFER_BIT);
            (self.gl.finish)();
        }
    }

    /// Clears the given FBO to transparent black and issues `glFinish`.
    pub fn render_and_finish(&self, fbo: u32, width: u32, height: u32) {
        self.clear_and_finish(fbo, width, height, [0.0, 0.0, 0.0, 1.0]);
    }

    /// Issues a `glFinish` on the current context.
    pub fn finish(&self) {
        // SAFETY: glFinish is safe on the current context.
        unsafe { (self.gl.finish)() };
    }
}

impl Drop for GlContext {
    fn drop(&mut self) {
        // SAFETY: destroy the surface, EGLImages, then the context, then the
        // display.
        unsafe {
            let _ = (self.egl.make_current)(
                self.display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if let Some(destroy_image) = self.egl.destroy_image {
                for &image in &self.images {
                    (destroy_image)(self.display, image);
                }
            }
            let surface = self.surface.get();
            if !surface.is_null() {
                (self.egl.destroy_surface)(self.display, surface);
            }
            (self.egl.destroy_context)(self.display, self.context);
            (self.egl.terminate)(self.display);
        }
    }
}
