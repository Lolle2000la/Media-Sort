use std::path::Path;

use super::{DecodedImage, DocumentError};

/// Requests a native OS thumbnail for `path` (QuickLook on macOS, `IShellItemImageFactory` on
/// Windows) scaled to fit within `max_width x max_height`.
///
/// Returns `Ok(None)` cleanly when the platform has no thumbnail provider (e.g. Linux, unsupported
/// formats, missing files, or QuickLook timeouts).
pub fn generate_os_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    let max_width = max_width.max(1);
    let max_height = max_height.max(1);
    if !path.is_file() {
        return Ok(None);
    }
    platform_thumbnail(path, max_width, max_height)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn fit_dimensions(src_w: u32, src_h: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    if src_w <= max_width && src_h <= max_height {
        (src_w.max(1), src_h.max(1))
    } else {
        let (w, h) = super::super::thumbnail::calculate_thumbnail_dimensions(
            src_w, src_h, max_width, max_height,
        );
        (w.max(1), h.max(1))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn unpremultiply_rgba_in_place(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else if a < 255 {
            let a16 = u16::from(a);
            let half = a16 / 2;
            px[0] = ((u16::from(px[0]) * 255 + half) / a16).min(255) as u8;
            px[1] = ((u16::from(px[1]) * 255 + half) / a16).min(255) as u8;
            px[2] = ((u16::from(px[2]) * 255 + half) / a16).min(255) as u8;
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::AnyThread as _;
    use objc2::rc::Retained;
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
        CGImageByteOrderInfo, CGRectZero, CGSizeZero,
    };
    use objc2_foundation::{NSError, NSString, NSURL};
    use objc2_quick_look_thumbnailing::{
        QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes,
        QLThumbnailGenerator, QLThumbnailRepresentation,
    };

    const QL_TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_DIM: usize = 16_384;

    fn extract_ql_image(
        rep: *mut QLThumbnailRepresentation,
        max_width: u32,
        max_height: u32,
    ) -> Option<DecodedImage> {
        // SAFETY: QuickLook passes either null or a valid `QLThumbnailRepresentation` pointer
        // into the completion handler; `Retained::retain` keeps it alive until `CGImage` is drawn.
        let rep_retained = unsafe { Retained::retain(rep) }?;
        // SAFETY: `rep_retained` is a valid `QLThumbnailRepresentation` instance.
        let cg_image = unsafe { rep_retained.CGImage() };

        let src_w = CGImage::width(Some(&cg_image));
        let src_h = CGImage::height(Some(&cg_image));
        if src_w == 0 || src_h == 0 || src_w > MAX_DIM || src_h > MAX_DIM {
            return None;
        }

        let (dst_w, dst_h) = fit_dimensions(src_w as u32, src_h as u32, max_width, max_height);
        let dst_w_usize = usize::try_from(dst_w).ok()?;
        let dst_h_usize = usize::try_from(dst_h).ok()?;
        let bytes_per_row = dst_w_usize.checked_mul(4)?;
        let total_bytes = bytes_per_row.checked_mul(dst_h_usize)?;
        let mut buf = vec![0u8; total_bytes];

        let color_space = CGColorSpace::new_device_rgb()?;
        let bitmap_info =
            CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0;

        // SAFETY: `buf` provides `bytes_per_row * dst_h_usize` writable bytes and outlives `ctx`
        // (dropped before `buf` is read or returned).
        let ctx = unsafe {
            CGBitmapContextCreate(
                buf.as_mut_ptr().cast(),
                dst_w_usize,
                dst_h_usize,
                8,
                bytes_per_row,
                Some(&color_space),
                bitmap_info,
            )
        }?;

        // SAFETY: `CGRectZero` is an immutable CoreGraphics global constant.
        let mut rect = unsafe { CGRectZero };
        rect.size.width = dst_w as _;
        rect.size.height = dst_h as _;
        CGContext::draw_image(Some(&ctx), rect, Some(&cg_image));
        drop(ctx);

        unpremultiply_rgba_in_place(&mut buf);
        Some(DecodedImage::new(dst_w, dst_h, buf))
    }

    let Some(path_str) = path.to_str() else {
        return Ok(None);
    };

    let ns_path = NSString::from_str(path_str);
    let url = NSURL::fileURLWithPath(&ns_path);
    // SAFETY: `CGSizeZero` is an immutable CoreGraphics global constant.
    let mut target_size = unsafe { CGSizeZero };
    target_size.width = max_width as _;
    target_size.height = max_height as _;

    // SAFETY: `url` is a valid `NSURL` and `alloc()` produces a freshly allocated request object.
    let request = unsafe {
        QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
            QLThumbnailGenerationRequest::alloc(),
            &url,
            target_size,
            1.0,
            QLThumbnailGenerationRequestRepresentationTypes::Thumbnail,
        )
    };
    // SAFETY: `request` is a valid initialized `QLThumbnailGenerationRequest`.
    unsafe { request.setIconMode(false) };

    let (tx, rx) = mpsc::channel::<Option<DecodedImage>>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));
    let block: RcBlock<dyn Fn(*mut QLThumbnailRepresentation, *mut NSError)> = RcBlock::new(
        move |rep: *mut QLThumbnailRepresentation, _err: *mut NSError| {
            let decoded = (!rep.is_null())
                .then(|| extract_ql_image(rep, max_width, max_height))
                .flatten();
            if let Some(sender) = tx_slot.lock().ok().and_then(|mut guard| guard.take()) {
                let _ = sender.send(decoded);
            }
        },
    );

    // SAFETY: `sharedGenerator` returns the singleton generator and `generateBestRepresentation...`
    // retains both `request` and `block` for the asynchronous thumbnail call.
    let generator = unsafe {
        let generator = QLThumbnailGenerator::sharedGenerator();
        generator.generateBestRepresentationForRequest_completionHandler(&request, &block);
        generator
    };

    match rx.recv_timeout(QL_TIMEOUT) {
        Ok(img) => Ok(img),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // SAFETY: `request` was created and submitted on this generator.
            unsafe { generator.cancelRequest(&request) };
            Ok(None)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Ok(None),
    }
}

#[cfg(target_os = "windows")]
fn platform_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    use std::path::PathBuf;

    use fast_image_resize::images::Image;
    use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
    use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK, SIZE};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDIBits, GetObjectW, HBITMAP, HDC,
    };
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, IBindCtx,
    };
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
        SIIGBF_THUMBNAILONLY,
    };
    use windows::core::HSTRING;

    const MAX_DIM: u32 = 16_384;

    struct ComGuard(bool);

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Matches a successful `CoInitializeEx` (`S_OK` or `S_FALSE`) on this thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    struct BitmapGuard(HBITMAP);

    impl Drop for BitmapGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` is an owned GDI `HBITMAP` returned by `GetImage`.
                unsafe {
                    let _ = DeleteObject(self.0.into());
                }
            }
        }
    }

    struct DcGuard(HDC);

    impl Drop for DcGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: `self.0` is an owned memory DC created by `CreateCompatibleDC`.
                unsafe {
                    let _ = DeleteDC(self.0);
                }
            }
        }
    }

    fn clean_shell_path(path: &Path) -> PathBuf {
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let s = abs.to_string_lossy();
        if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{unc}"))
        } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
            PathBuf::from(stripped)
        } else {
            abs
        }
    }

    fn resize_rgba_to_fit(
        src_w: u32,
        src_h: u32,
        rgba: Vec<u8>,
        max_width: u32,
        max_height: u32,
    ) -> Result<DecodedImage, DocumentError> {
        let (dst_w, dst_h) = fit_dimensions(src_w, src_h, max_width, max_height);
        if dst_w == src_w && dst_h == src_h {
            return Ok(DecodedImage::new(src_w, src_h, rgba));
        }
        let src_img = image::RgbaImage::from_raw(src_w, src_h, rgba)
            .ok_or_else(|| DocumentError::Message("Invalid OS thumbnail buffer".to_string()))?;
        let mut dst_img = Image::new(dst_w, dst_h, PixelType::U8x4);
        let mut resizer = Resizer::new();
        resizer
            .resize(
                &src_img,
                &mut dst_img,
                &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
            )
            .map_err(|e| DocumentError::Message(format!("Failed to resize OS thumbnail: {e}")))?;
        Ok(DecodedImage::new(dst_w, dst_h, dst_img.into_vec()))
    }

    fn fetch_shell_thumbnail(
        path: &Path,
        max_width: u32,
        max_height: u32,
    ) -> Result<Option<DecodedImage>, DocumentError> {
        let clean = clean_shell_path(path);
        let hpath = HSTRING::from(clean.as_path());

        // SAFETY: COM is initialized on this thread and `hpath` is a valid wide string.
        let Ok(factory) = (unsafe {
            SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(&hpath, None::<&IBindCtx>)
        }) else {
            return Ok(None);
        };

        let size = SIZE {
            cx: i32::try_from(max_width).unwrap_or(i32::MAX),
            cy: i32::try_from(max_height).unwrap_or(i32::MAX),
        };
        // SAFETY: `factory` is a valid `IShellItemImageFactory` COM interface.
        let Ok(hbm) =
            (unsafe { factory.GetImage(size, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK) })
        else {
            return Ok(None);
        };
        if hbm.is_invalid() {
            return Ok(None);
        }
        let _bm_guard = BitmapGuard(hbm);

        let mut bmp = BITMAP::default();
        let bmp_size = i32::try_from(size_of::<BITMAP>()).unwrap_or(0);
        // SAFETY: `hbm` is a valid `HBITMAP` and `&raw mut bmp` points to `bmp_size` writable bytes.
        let got_obj = unsafe { GetObjectW(hbm.into(), bmp_size, Some((&raw mut bmp).cast())) };
        if got_obj == 0 {
            return Ok(None);
        }

        let Ok(w) = u32::try_from(bmp.bmWidth) else {
            return Ok(None);
        };
        let h = bmp.bmHeight.unsigned_abs();
        if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
            return Ok(None);
        }
        let Ok(neg_h) = i32::try_from(h).map(|v| -v) else {
            return Ok(None);
        };

        // SAFETY: Passing `None` creates a memory DC compatible with the screen.
        let hdc = unsafe { CreateCompatibleDC(None) };
        if hdc.is_invalid() {
            return Ok(None);
        }
        let _dc_guard = DcGuard(hdc);

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap_or(0),
            biWidth: bmp.bmWidth,
            biHeight: neg_h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let Some(pixel_bytes) = usize::try_from(w)
            .ok()
            .and_then(|wu| usize::try_from(h).ok().and_then(|hu| wu.checked_mul(hu)))
            .and_then(|px| px.checked_mul(4))
        else {
            return Ok(None);
        };
        let mut buf = vec![0u8; pixel_bytes];

        // SAFETY: `hbm` is not selected into `hdc`, `buf` holds `w * h * 4` bytes matching the
        // 32-bit top-down `BITMAPINFOHEADER`, and `bmi` is properly initialized.
        let scanlines = unsafe {
            GetDIBits(
                hdc,
                hbm,
                0,
                h,
                Some(buf.as_mut_ptr().cast()),
                &raw mut bmi,
                DIB_RGB_COLORS,
            )
        };
        if scanlines == 0 {
            return Ok(None);
        }

        let force_opaque = bmp.bmBitsPixel < 32 || !buf.chunks_exact(4).any(|px| px[3] != 0);
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            if force_opaque {
                px[3] = 255;
            }
        }
        if !force_opaque {
            unpremultiply_rgba_in_place(&mut buf);
        }

        resize_rgba_to_fit(w, h, buf, max_width, max_height).map(Some)
    }

    // SAFETY: Initializes COM apartment state for this worker thread if not already initialized.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let _com = if hr == S_OK || hr == S_FALSE {
        ComGuard(true)
    } else if hr == RPC_E_CHANGED_MODE {
        ComGuard(false)
    } else {
        return Ok(None);
    };

    // Run Shell/GDI work in a dedicated function so all COM objects drop before `CoUninitialize`.
    fetch_shell_thumbnail(path, max_width, max_height)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_thumbnail(
    _path: &Path,
    _max_width: u32,
    _max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    Ok(None)
}
