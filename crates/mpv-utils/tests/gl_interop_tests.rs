use mpv_utils::{GlFboTarget, SharedGpuFrameHandle, create_gl_render_context, render_gl_fbo};
use std::ffi::c_void;

extern "C" fn mock_get_proc_addr(_ctx: *mut c_void, _name: *const std::ffi::c_char) -> *mut c_void {
    std::ptr::null_mut()
}

#[test]
fn test_gl_fbo_target_creation() {
    let target = GlFboTarget {
        fbo_id: 1,
        width: 1920,
        height: 1080,
        internal_format: 0x8058, // GL_RGBA8
    };

    assert_eq!(target.fbo_id, 1);
    assert_eq!(target.width, 1920);
    assert_eq!(target.height, 1080);
    assert_eq!(target.internal_format, 0x8058);
}

#[test]
fn test_shared_gpu_frame_handle_software_fallback() {
    let handle = SharedGpuFrameHandle::SoftwareFallback;
    assert!(matches!(handle, SharedGpuFrameHandle::SoftwareFallback));
}

#[cfg(target_os = "linux")]
#[test]
fn test_shared_gpu_frame_handle_dmabuf_variant() {
    use mpv_utils::gl_interop::DmaBufHandle;

    // A real fd (memfd) so dropping the handle actually closes it.
    let fd = unsafe { libc::memfd_create(c"test-frame".as_ptr(), 0) };
    assert!(fd >= 0, "memfd_create failed");
    let handle = SharedGpuFrameHandle::DmaBuf(DmaBufHandle::new(fd, 1280, 720, 5120, 0));

    let SharedGpuFrameHandle::DmaBuf(h) = &handle else {
        panic!("expected DmaBuf handle");
    };
    assert_eq!(h.width(), 1280);
    assert_eq!(h.height(), 720);
    assert_eq!(h.stride(), 5120);
    assert_eq!(h.offset(), 0);
    assert!(handle.as_fd().is_some());

    // Consuming the handle transfers the fd without closing it.
    let consumed = handle.into_fd().expect("fd present");
    assert!(consumed >= 0);
    // SAFETY: close the transferred fd.
    unsafe { libc::close(consumed) };
}

#[test]
fn test_create_gl_render_context_null_handle_returns_error() {
    let result = unsafe {
        create_gl_render_context(
            std::ptr::null_mut(),
            mock_get_proc_addr,
            std::ptr::null_mut(),
        )
    };

    assert!(result.is_err());
}

#[test]
fn test_render_gl_fbo_null_context_returns_error() {
    let target = GlFboTarget {
        fbo_id: 0,
        width: 640,
        height: 480,
        internal_format: 0,
    };

    let result = unsafe { render_gl_fbo(std::ptr::null_mut(), target) };
    assert!(result.is_err());
}
