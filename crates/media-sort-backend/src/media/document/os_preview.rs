use std::path::Path;

use super::{DecodedImage, DocumentError};

/// Requests a native OS thumbnail for `path` (QuickLook on macOS, `IShellItemImageFactory` on
/// Windows, read-only XDG thumbnail cache lookup on Linux) scaled to fit within
/// `max_width x max_height`.
///
/// Returns `Ok(None)` cleanly when the platform has no cached/native thumbnail (e.g. uncached
/// Linux files, unsupported formats, missing files, or QuickLook timeouts).
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

#[cfg(target_os = "linux")]
fn xdg_thumbnails_base_dir() -> Option<std::path::PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(xdg).join("thumbnails"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| {
            std::path::PathBuf::from(home)
                .join(".cache")
                .join("thumbnails")
        })
}

#[cfg(target_os = "linux")]
fn xdg_thumbnail_path_in(
    cache_thumbnails_dir: &Path,
    source_path: &Path,
) -> Option<std::path::PathBuf> {
    const SUBDIRS: [&str; 4] = ["x-large", "xx-large", "large", "normal"];
    let canonical = std::fs::canonicalize(source_path).ok()?;
    let source_mtime = std::fs::metadata(&canonical).ok()?.modified().ok()?;
    let uri = super::canonical_path_to_file_url(&canonical);
    let filename = format!("{}.png", md5_hex(uri.as_bytes()));

    for subdir in SUBDIRS {
        let candidate = cache_thumbnails_dir.join(subdir).join(&filename);
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(thumb_mtime) = meta.modified() else {
            continue;
        };
        if thumb_mtime >= source_mtime {
            return Some(candidate);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn platform_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    let Some(base_dir) = xdg_thumbnails_base_dir() else {
        return Ok(None);
    };
    let Some(thumb_path) = xdg_thumbnail_path_in(&base_dir, path) else {
        return Ok(None);
    };
    let Ok(bytes) = super::read_file_capped(&thumb_path, super::MAX_ARCHIVE_ENTRY_BYTES) else {
        return Ok(None);
    };
    match crate::media::format_pipeline::decode_and_resize_bytes(&bytes, max_width, max_height) {
        Ok(img) => Ok(Some(img)),
        Err(_) => Ok(None),
    }
}

#[cfg(target_os = "linux")]
fn md5_hex(input: &[u8]) -> String {
    const MD5_S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const MD5_K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(input.len() + 72);
    msg.extend_from_slice(input);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (word, bytes) in m.iter_mut().zip(chunk.as_chunks::<4>().0.iter()) {
            *word = u32::from_le_bytes(*bytes);
        }

        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;

        for (i, (&k, &s)) in MD5_K.iter().zip(MD5_S.iter()).enumerate() {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let sum = f.wrapping_add(a).wrapping_add(k).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(sum.rotate_left(s));
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut hex = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            hex.push(char::from(HEX[(byte >> 4) as usize]));
            hex.push(char::from(HEX[(byte & 0x0F) as usize]));
        }
    }
    hex
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn platform_thumbnail(
    _path: &Path,
    _max_width: u32,
    _max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    Ok(None)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs::File;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "mediasort_xdg_thumb_test_{}_{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create test temp dir");
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn md5_matches_rfc1321_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex(b"abcdefghijklmnopqrstuvwxyz"),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );
        assert_eq!(
            md5_hex(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"),
            "d174ab98d277d9f5a5611c2c9f419d9f"
        );
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn xdg_thumbnail_path_in_checks_priority_and_freshness() {
        let dir = TestDir::new();
        let src = dir.0.join("my report.docx");
        let src_file = File::create(&src).expect("create source file");
        let base_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        src_file
            .set_modified(base_time)
            .expect("set source modified time");
        drop(src_file);

        let canonical = std::fs::canonicalize(&src).expect("canonicalize source");
        let uri = crate::media::document::canonical_path_to_file_url(&canonical);
        assert!(uri.ends_with("/my%20report.docx"));
        let thumb_name = format!("{}.png", md5_hex(uri.as_bytes()));

        let cache_dir = dir.0.join("thumbnails");
        let large_dir = cache_dir.join("large");
        let xlarge_dir = cache_dir.join("x-large");
        std::fs::create_dir_all(&large_dir).expect("create large dir");
        std::fs::create_dir_all(&xlarge_dir).expect("create x-large dir");

        // Stale large thumbnail (older than source) is ignored.
        let large_thumb = large_dir.join(&thumb_name);
        let f = File::create(&large_thumb).expect("create stale large thumb");
        f.set_modified(base_time - Duration::from_secs(10))
            .expect("set stale mtime");
        drop(f);
        assert!(xdg_thumbnail_path_in(&cache_dir, &src).is_none());

        // Fresh large thumbnail matches.
        let f = File::create(&large_thumb).expect("create fresh large thumb");
        f.set_modified(base_time).expect("set fresh mtime");
        drop(f);
        assert_eq!(xdg_thumbnail_path_in(&cache_dir, &src), Some(large_thumb));

        // Fresh x-large thumbnail takes priority over large.
        let xlarge_thumb = xlarge_dir.join(&thumb_name);
        let f = File::create(&xlarge_thumb).expect("create fresh x-large thumb");
        f.set_modified(base_time + Duration::from_secs(5))
            .expect("set x-large mtime");
        drop(f);
        assert_eq!(xdg_thumbnail_path_in(&cache_dir, &src), Some(xlarge_thumb));
    }
}
