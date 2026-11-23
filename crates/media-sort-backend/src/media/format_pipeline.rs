//! Per-format optimized image decoding pipeline shared by the thumbnail
//! generator and the preview loader. The per-format strategies are ported
//! from the benchmark-validated variants in `crates/benchmarks/src/variants.rs`:
//!
//! - jpg/jpeg: turbojpeg scaled decompress (largest scaling factor whose
//!   scaled dimensions are still >= the target) + fast_image_resize bilinear
//!   to the exact fit. EXIF orientation is parsed from the same in-memory
//!   bytes and applied *after* the resize.
//! - bmp/tga/qoi/ff/farbfeld: image-crate decode + fast_image_resize bilinear.
//!   These formats carry no EXIF, so no orientation handling and no
//!   re-opening of the file.
//! - avif: native dav1d decode (`avif-native` feature) with an ffmpeg pipe
//!   fallback for builds without a native AVIF decoder.
//! - svg/svgz: pure-Rust `resvg` rasterization directly at the target box.
//! - ico: `ico` crate multi-entry selection (largest entry fitting the target
//!   box for thumbnails, or largest overall scaled down; largest entry for
//!   full decode / preview / dimensions).
//! - audio extensions: embedded cover art, resized with fast_image_resize
//!   (`thumbnail_image` returns `None` when there is no cover art — that is a
//!   normal, playable file, not a decode failure).
//! - everything else: `image_decoder::load_image` (keeps EXIF orientation for
//!   jpeg-like formats) + the image crate's `thumbnail()`.

use std::io::Cursor;
use std::path::Path;

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use super::DecodedImage;
use super::thumbnail::calculate_thumbnail_dimensions;

/// Formats handled by a dedicated decoder/renderer rather than the generic
/// `image` crate pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpecialImageFormat {
    Svg,
    Ico,
}

/// Detect whether `path` uses a special-format pipeline (case-insensitive).
pub(crate) fn detect_special_format(path: &Path) -> Option<SpecialImageFormat> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    if super::svg::is_svg_extension(ext) {
        Some(SpecialImageFormat::Svg)
    } else if ext.eq_ignore_ascii_case("ico") {
        Some(SpecialImageFormat::Ico)
    } else {
        None
    }
}

fn extension_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

pub(crate) fn is_audio_extension(ext: &str) -> bool {
    media_sort_core::media_type::MediaType::Audio
        .extensions()
        .contains(&ext)
}

fn to_image_error(err: String) -> image::ImageError {
    image::ImageError::Decoding(image::error::DecodingError::new(
        image::error::ImageFormatHint::Unknown,
        err,
    ))
}

fn ico_decoding_error(
    msg: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> image::ImageError {
    image::ImageError::Decoding(image::error::DecodingError::new(
        image::error::ImageFormatHint::Name("ico".to_string()),
        msg,
    ))
}

fn io_image_error(err: std::io::Error) -> image::ImageError {
    image::ImageError::IoError(std::io::Error::new(err.kind(), err.to_string()))
}

/// Decode a special-format image at full/native resolution as a [`image::DynamicImage`].
pub(crate) fn load_special_image(
    path: &Path,
    fmt: SpecialImageFormat,
) -> Result<image::DynamicImage, image::ImageError> {
    match fmt {
        SpecialImageFormat::Svg => super::svg::load_svg_dynamic(path),
        SpecialImageFormat::Ico => {
            let icon_dir = read_ico_dir(path)?;
            let entry = largest_ico_entry(&icon_dir)?;
            let decoded = decode_ico_entry(entry)?;
            super::image_decoder::rgba_to_dynamic(decoded.width, decoded.height, decoded.rgba)
        }
    }
}

/// Read dimensions of a special-format image without full pixel work when possible.
pub(crate) fn special_image_dimensions(
    path: &Path,
    fmt: SpecialImageFormat,
) -> Result<(u32, u32), image::ImageError> {
    match fmt {
        SpecialImageFormat::Svg => super::svg::svg_dimensions(path),
        SpecialImageFormat::Ico => {
            let icon_dir = read_ico_dir(path)?;
            let entry = largest_ico_entry(&icon_dir)?;
            let (w, h) = (entry.width(), entry.height());
            let max_dim = super::image_decoder::MAX_DECODE_DIMENSION;
            if w == 0 || h == 0 || w > max_dim || h > max_dim {
                return Err(image::ImageError::Limits(
                    image::error::LimitError::from_kind(
                        image::error::LimitErrorKind::DimensionError,
                    ),
                ));
            }
            Ok((w, h))
        }
    }
}

pub(crate) fn render_special_image(
    path: &Path,
    fmt: SpecialImageFormat,
    max_width: u32,
    max_height: u32,
    ico_prefer_fit: bool,
) -> Result<DecodedImage, image::ImageError> {
    match fmt {
        SpecialImageFormat::Svg => super::svg::render_svg(path, max_width, max_height),
        SpecialImageFormat::Ico => decode_ico_box(path, max_width, max_height, ico_prefer_fit),
    }
}

/// Decode `path` to fit inside the `max_width` x `max_height` box, preserving
/// aspect ratio.
///
/// Image formats only — audio cover art is dispatched by [`thumbnail_image`].
pub(crate) fn process_image(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    process_image_with_mode(path, max_width, max_height, false)
}

fn process_image_with_mode(
    path: &Path,
    max_width: u32,
    max_height: u32,
    ico_prefer_fit: bool,
) -> Result<DecodedImage, image::ImageError> {
    if let Some(special) = detect_special_format(path) {
        return render_special_image(path, special, max_width, max_height, ico_prefer_fit);
    }

    let ext = extension_lower(path);
    match ext.as_str() {
        "jpg" | "jpeg" => jpeg_image(path, max_width, max_height),
        "bmp" | "tga" | "qoi" | "ff" | "farbfeld" => decode_fir_image(path, max_width, max_height),
        "avif" => avif_image(path, max_width, max_height),
        _ => fallback_image(path, max_width, max_height),
    }
}

/// Produce a grid thumbnail for `path`.
///
/// Unlike [`process_image`], this distinguishes "there is no image to show"
/// from "decoding failed": an audio file with no embedded cover art is a
/// normal, playable file, so it returns `Ok(None)` instead of an error.
/// Genuine decode failures — including cover art that is present but corrupt —
/// stay `Err`.
pub(crate) fn thumbnail_image(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, image::ImageError> {
    if is_audio_extension(&extension_lower(path)) {
        return audio_cover_image(path, max_width, max_height);
    }
    process_image_with_mode(path, max_width, max_height, true).map(Some)
}

/// Decode an audio file's embedded cover art into the thumbnail box.
///
/// `Ok(None)` means the file carries no embedded cover art. It must NOT fall
/// back to decoding the audio file itself: the audio container is not an
/// image, and that fallback produced "the file extension `.wav` was not
/// recognized as an image format" and marked the whole file unsupported.
fn audio_cover_image(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, image::ImageError> {
    let Some(bytes) = super::thumbnail::extract_audio_cover(path) else {
        return Ok(None);
    };
    let img = super::image_decoder::decode_bytes_with_limits(bytes.as_slice())?;
    resize_with_fir(&img.to_rgba8(), max_width, max_height).map(Some)
}

fn read_ico_dir(path: &Path) -> Result<ico::IconDir, image::ImageError> {
    let file = std::fs::File::open(path)?;
    ico::IconDir::read(std::io::BufReader::new(file)).map_err(ico_decoding_error)
}

fn ico_entry_key(entry: &ico::IconDirEntry) -> (u64, u16) {
    (
        u64::from(entry.width()) * u64::from(entry.height()),
        entry.bits_per_pixel(),
    )
}

fn largest_ico_entry(icon_dir: &ico::IconDir) -> Result<&ico::IconDirEntry, image::ImageError> {
    icon_dir
        .entries()
        .iter()
        .max_by_key(|e| ico_entry_key(e))
        .ok_or_else(|| ico_decoding_error("ICO file contains no icon entries"))
}

fn decode_ico_entry(entry: &ico::IconDirEntry) -> Result<DecodedImage, image::ImageError> {
    let max_dim = super::image_decoder::MAX_DECODE_DIMENSION;
    if entry.width() > max_dim || entry.height() > max_dim {
        return Err(image::ImageError::Limits(
            image::error::LimitError::from_kind(image::error::LimitErrorKind::DimensionError),
        ));
    }
    let icon = entry.decode().map_err(ico_decoding_error)?;
    let (w, h) = (icon.width(), icon.height());
    if w == 0 || h == 0 || w > max_dim || h > max_dim {
        return Err(image::ImageError::Limits(
            image::error::LimitError::from_kind(image::error::LimitErrorKind::DimensionError),
        ));
    }
    Ok(DecodedImage::new(w, h, icon.rgba_data().to_vec()))
}

fn decode_ico_box(
    path: &Path,
    max_width: u32,
    max_height: u32,
    prefer_fitting_entry: bool,
) -> Result<DecodedImage, image::ImageError> {
    let icon_dir = read_ico_dir(path)?;
    let entry = if prefer_fitting_entry {
        icon_dir
            .entries()
            .iter()
            .filter(|e| e.width() <= max_width && e.height() <= max_height)
            .max_by_key(|e| ico_entry_key(e))
            .map(Ok)
            .unwrap_or_else(|| largest_ico_entry(&icon_dir))?
    } else {
        largest_ico_entry(&icon_dir)?
    };

    let decoded = decode_ico_entry(entry)?;
    if max_width > 0 && max_height > 0 && (decoded.width > max_width || decoded.height > max_height)
    {
        return resize_rgba_to_box(
            &decoded.rgba,
            decoded.width,
            decoded.height,
            max_width,
            max_height,
        );
    }
    Ok(decoded)
}

fn decode_fir_image(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    let img = super::image_decoder::decode_path_with_limits(path)?;
    resize_with_fir(&img.to_rgba8(), max_width, max_height)
}

fn resize_with_fir(
    img_rgba: &image::RgbaImage,
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    let (src_w, src_h) = img_rgba.dimensions();
    resize_rgba_to_box(img_rgba.as_raw(), src_w, src_h, max_width, max_height)
}

/// Decodes encoded image bytes using size/memory limits and resizes to fit `max_width x max_height`.
pub(crate) fn decode_and_resize_bytes(
    bytes: &[u8],
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    let img = super::image_decoder::decode_bytes_with_limits(bytes)?;
    resize_with_fir(&img.to_rgba8(), max_width, max_height)
}

fn resize_rgba_to_box(
    rgba: &[u8],
    src_w: u32,
    src_h: u32,
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    let (dst_w, dst_h) = calculate_thumbnail_dimensions(src_w, src_h, max_width, max_height);

    if dst_w == 0 || dst_h == 0 {
        return Ok(DecodedImage::new(src_w, src_h, rgba.to_vec()));
    }

    let resized = resize_rgba(rgba, src_w, src_h, dst_w, dst_h).map_err(to_image_error)?;
    Ok(DecodedImage::new(dst_w, dst_h, resized))
}

fn jpeg_image(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    let bytes = std::fs::read(path).map_err(io_image_error)?;
    let orientation = parse_exif_orientation_from_bytes(&bytes);
    let scaled = decode_jpeg_turbojpeg_scaled(&bytes, orientation, max_width, max_height)
        .map_err(to_image_error)?;
    resize_and_orient_to(
        &scaled.image.rgba,
        scaled.image.width,
        scaled.image.height,
        orientation,
        scaled.target_width,
        scaled.target_height,
    )
    .map_err(to_image_error)
}

fn avif_image(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    // Native decode via dav1d (`avif-native` feature) is in-process and fast.
    // The ffmpeg pipe only serves as a fallback for files the native decoder
    // rejects.
    match fallback_image(path, max_width, max_height) {
        Ok(result) => Ok(result),
        Err(_) => ffmpeg_pipe(path, max_width, max_height).map_err(to_image_error),
    }
}

fn fallback_image(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    let img = super::image_decoder::load_image(path)?;
    let thumbnail = img.thumbnail(max_width, max_height).to_rgba8();
    let (w, h) = thumbnail.dimensions();
    Ok(DecodedImage::new(w, h, thumbnail.into_raw()))
}

// ── EXIF orientation (ported from benchmarks variants.rs) ─────────────

fn parse_exif_orientation_from_bytes(bytes: &[u8]) -> Option<u32> {
    let mut cursor = Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;
    let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
    field.value.get_uint(0)
}

fn apply_orientation_to_rgba(w: u32, h: u32, rgba: &mut Vec<u8>, orientation: u32) -> (u32, u32) {
    match orientation {
        2 => {
            flip_horizontal_inplace(w, h, rgba);
            (w, h)
        }
        3 => {
            rotate_180_inplace(w, h, rgba);
            (w, h)
        }
        4 => {
            flip_vertical_inplace(w, h, rgba);
            (w, h)
        }
        5 => {
            flip_horizontal_inplace(w, h, rgba);
            let (nw, nh, ndata) = rotate_rgba_270_owned(w, h, rgba);
            *rgba = ndata;
            (nw, nh)
        }
        6 => {
            let (nw, nh, ndata) = rotate_rgba_90_owned(w, h, rgba);
            *rgba = ndata;
            (nw, nh)
        }
        7 => {
            flip_horizontal_inplace(w, h, rgba);
            let (nw, nh, ndata) = rotate_rgba_90_owned(w, h, rgba);
            *rgba = ndata;
            (nw, nh)
        }
        8 => {
            let (nw, nh, ndata) = rotate_rgba_270_owned(w, h, rgba);
            *rgba = ndata;
            (nw, nh)
        }
        _ => (w, h),
    }
}

fn flip_horizontal_inplace(w: u32, _h: u32, rgba: &mut [u8]) {
    let stride = (w * 4) as usize;
    for row in rgba.chunks_exact_mut(stride) {
        for x in 0..(w as usize / 2) {
            let left = x * 4;
            let right = (w as usize - 1 - x) * 4;
            for i in 0..4 {
                row.swap(left + i, right + i);
            }
        }
    }
}

fn flip_vertical_inplace(w: u32, h: u32, rgba: &mut [u8]) {
    let stride = (w * 4) as usize;
    for y in 0..(h as usize / 2) {
        let top = y * stride;
        let bottom = (h as usize - 1 - y) * stride;
        for x in 0..stride {
            rgba.swap(top + x, bottom + x);
        }
    }
}

fn rotate_180_inplace(_w: u32, _h: u32, rgba: &mut [u8]) {
    // Swap 4-byte PIXEL groups, not individual bytes: reversing the raw
    // buffer would also reverse the channel order inside each pixel
    // (RGBA -> ABGR).
    let pixels = rgba.len() / 4;
    for i in 0..pixels / 2 {
        let j = pixels - 1 - i;
        for c in 0..4 {
            rgba.swap(i * 4 + c, j * 4 + c);
        }
    }
}

fn rotate_rgba_90_owned(w: u32, h: u32, rgba: &[u8]) -> (u32, u32, Vec<u8>) {
    let dst_w = h;
    let dst_h = w;
    let mut dst = vec![0u8; (dst_w * dst_h * 4) as usize];
    let src_stride = (w * 4) as usize;
    let dst_stride = (dst_w * 4) as usize;
    for dst_y in 0..dst_h {
        let src_x = dst_y;
        for dst_x in 0..dst_w {
            let src_y = h - 1 - dst_x;
            let src_idx = (src_y as usize * src_stride) + (src_x as usize * 4);
            let dst_idx = (dst_y as usize * dst_stride) + (dst_x as usize * 4);
            dst[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
        }
    }
    (dst_w, dst_h, dst)
}

fn rotate_rgba_270_owned(w: u32, h: u32, rgba: &[u8]) -> (u32, u32, Vec<u8>) {
    let dst_w = h;
    let dst_h = w;
    let mut dst = vec![0u8; (dst_w * dst_h * 4) as usize];
    let src_stride = (w * 4) as usize;
    let dst_stride = (dst_w * 4) as usize;
    for dst_y in 0..dst_h {
        let src_x = w - 1 - dst_y;
        for dst_x in 0..dst_w {
            let src_y = dst_x;
            let src_idx = (src_y as usize * src_stride) + (src_x as usize * 4);
            let dst_idx = (dst_y as usize * dst_stride) + (dst_x as usize * 4);
            dst[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
        }
    }
    (dst_w, dst_h, dst)
}

fn is_orientation_swap(o: u32) -> bool {
    o == 6 || o == 8 || o == 5 || o == 7
}

fn oriented_src_dims(src_w: u32, src_h: u32, orientation: Option<u32>) -> (u32, u32) {
    if let Some(o) = orientation
        && is_orientation_swap(o)
    {
        (src_h, src_w)
    } else {
        (src_w, src_h)
    }
}

/// Resizes to precomputed final dims (orientation applied afterwards).
/// The final dims must come from the TRUE source dims: DCT-scaled decodes
/// round each axis independently, so recomputing the target box from the
/// decoded (already scaled) dims distorts extreme aspect ratios
/// (1000x10 -> 1/8 decode 125x2 -> (100,2) instead of (100,1)).
fn resize_and_orient_to(
    src_rgba: &[u8],
    src_w: u32,
    src_h: u32,
    orientation: Option<u32>,
    final_w: u32,
    final_h: u32,
) -> Result<DecodedImage, String> {
    if final_w == 0 || final_h == 0 {
        return Ok(DecodedImage::new(src_w, src_h, src_rgba.to_vec()));
    }

    let (resize_w, resize_h) = if let Some(o) = orientation
        && is_orientation_swap(o)
    {
        (final_h, final_w)
    } else {
        (final_w, final_h)
    };

    let mut resized = resize_rgba(src_rgba, src_w, src_h, resize_w, resize_h)?;

    if let Some(o) = orientation {
        let (fw, fh) = apply_orientation_to_rgba(resize_w, resize_h, &mut resized, o);
        Ok(DecodedImage::new(fw, fh, resized))
    } else {
        Ok(DecodedImage::new(resize_w, resize_h, resized))
    }
}

fn resize_rgba(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, String> {
    if dst_w == 0 || dst_h == 0 {
        return Ok(Vec::new());
    }
    thread_local! {
        static RESIZER: std::cell::RefCell<Resizer> = std::cell::RefCell::new(Resizer::new());
    }
    let src_img = image::RgbaImage::from_raw(src_w, src_h, src.to_vec())
        .ok_or_else(|| "failed to create RgbaImage from raw data".to_string())?;
    let mut dst_img = Image::new(dst_w, dst_h, PixelType::U8x4);
    RESIZER
        .with_borrow_mut(|resizer| {
            resizer.resize(
                &src_img,
                &mut dst_img,
                &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
            )
        })
        .map_err(|e| format!("{e}"))?;
    Ok(dst_img.into_vec())
}

// ── turbojpeg scaled JPEG decompress (ported from variants.rs) ────────

fn choose_turbojpeg_scaling_factor(
    width: usize,
    height: usize,
    need_w: usize,
    need_h: usize,
) -> turbojpeg::ScalingFactor {
    // Smallest decode that still covers the size the image will be resized
    // to, per axis (never upscale afterwards).
    let candidates = [
        turbojpeg::ScalingFactor::ONE_EIGHTH,
        turbojpeg::ScalingFactor::ONE_QUARTER,
        turbojpeg::ScalingFactor::ONE_HALF,
        turbojpeg::ScalingFactor::ONE,
    ];
    for &factor in &candidates {
        if factor.scale(width) >= need_w && factor.scale(height) >= need_h {
            return factor;
        }
    }
    turbojpeg::ScalingFactor::ONE
}

/// Intermediate scaled JPEG decode plus the final output dims computed
/// from the TRUE header dims (see resize_and_orient_to docs).
struct ScaledDecode {
    image: DecodedImage,
    target_width: u32,
    target_height: u32,
}

/// Reject JPEGs whose SOF header dimensions exceed the shared decode budget's
/// dimension cap. turbojpeg accepts up to 65500px per side from the SOF
/// marker, and the scaled buffer is allocated from those header dims BEFORE
/// decompressing - so a crafted file claiming 65500x65500 would force a
/// ~268 MB allocation per decode, multiplied across the parallel thumbnail
/// pipeline. Rejecting beyond `MAX_DECODE_DIMENSION` (far above any monitor)
/// prevents the allocation entirely; the allocation-site guard below
/// additionally enforces `max_alloc` on the scaled buffer itself.
fn decode_jpeg_turbojpeg_scaled(
    bytes: &[u8],
    orientation: Option<u32>,
    max_w: u32,
    max_h: u32,
) -> Result<ScaledDecode, String> {
    let mut decompressor =
        turbojpeg::Decompressor::new().map_err(|e| format!("turbojpeg init: {e}"))?;
    let header = decompressor
        .read_header(bytes)
        .map_err(|e| format!("turbojpeg header: {e}"))?;
    let max_dimension = super::image_decoder::MAX_DECODE_DIMENSION as usize;
    if header.width > max_dimension || header.height > max_dimension {
        return Err(format!(
            "JPEG dimensions {}x{} exceed the {}px limit",
            header.width, header.height, max_dimension
        ));
    }
    // Required decode resolution and final output dims, both computed from
    // the TRUE header dims (not the rounded DCT-scaled decode dims).
    let (eff_w, eff_h) = oriented_src_dims(header.width as u32, header.height as u32, orientation);
    let (final_w, final_h) = calculate_thumbnail_dimensions(eff_w, eff_h, max_w, max_h);
    let (need_w, need_h) = if orientation.is_some_and(is_orientation_swap) {
        (final_h, final_w)
    } else {
        (final_w, final_h)
    };
    let factor = choose_turbojpeg_scaling_factor(
        header.width,
        header.height,
        need_w.max(1) as usize,
        need_h.max(1) as usize,
    );
    let scaled = header.scaled(factor);
    decompressor
        .set_scaling_factor(factor)
        .map_err(|e| format!("turbojpeg set_scale: {e}"))?;
    let pitch = scaled
        .width
        .checked_mul(4)
        .ok_or_else(|| "JPEG pitch overflow".to_string())?;
    let buffer_size = scaled
        .height
        .checked_mul(pitch)
        .ok_or_else(|| "JPEG buffer size overflow".to_string())?;
    // The scaled buffer is allocated from header-derived dims before
    // decompressing; enforce the shared decode budget's `max_alloc` here at
    // the allocation site itself (turbojpeg never consults it internally).
    let max_alloc_bytes = super::image_decoder::image_decode_limits()
        .max_alloc
        .unwrap_or(u64::MAX);
    if buffer_size as u64 > max_alloc_bytes {
        return Err(format!(
            "JPEG scaled buffer of {buffer_size} bytes exceeds the {} MiB decode allocation cap",
            max_alloc_bytes / (1024 * 1024)
        ));
    }
    let mut image = turbojpeg::Image {
        pixels: vec![0u8; buffer_size],
        width: scaled.width,
        pitch,
        height: scaled.height,
        format: turbojpeg::PixelFormat::RGBA,
    };
    decompressor
        .decompress(bytes, image.as_deref_mut())
        .map_err(|e| format!("turbojpeg decompress: {e}"))?;
    Ok(ScaledDecode {
        image: DecodedImage::new(scaled.width as u32, scaled.height as u32, image.pixels),
        target_width: final_w,
        target_height: final_h,
    })
}

// ── ffmpeg subprocess pipe (delegates to shared ffmpeg_pipe module) ──

fn ffmpeg_pipe(path: &Path, max_w: u32, max_h: u32) -> Result<DecodedImage, String> {
    super::ffmpeg_pipe::extract_frame(path, max_w, max_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_180_swaps_pixels_not_channels() {
        // Two pixels with distinct channels: byte-level reversal would turn
        // RGBA into ABGR; pixel-level reversal must keep channels intact.
        let mut rgba = vec![1, 2, 3, 4, 10, 20, 30, 40];
        rotate_180_inplace(2, 1, &mut rgba);
        assert_eq!(rgba, vec![10, 20, 30, 40, 1, 2, 3, 4]);
    }

    #[test]
    fn flip_horizontal_keeps_channels() {
        let mut rgba = vec![1, 2, 3, 4, 10, 20, 30, 40];
        flip_horizontal_inplace(2, 1, &mut rgba);
        assert_eq!(rgba, vec![10, 20, 30, 40, 1, 2, 3, 4]);
    }

    #[test]
    fn flip_vertical_keeps_channels() {
        let mut rgba = vec![1, 2, 3, 4, 10, 20, 30, 40];
        flip_vertical_inplace(1, 2, &mut rgba);
        assert_eq!(rgba, vec![10, 20, 30, 40, 1, 2, 3, 4]);
    }

    #[test]
    fn scaling_factor_covers_needed_dims_per_axis() {
        // Preview case from review: 4000x3000 into a 1920x1440 box needs
        // 1920x1440 output; 1/2 decode (2000x1500) covers it per axis and
        // must not be rejected by a single min-dimension threshold.
        let f = choose_turbojpeg_scaling_factor(4000, 3000, 1920, 1440);
        assert_eq!(f, turbojpeg::ScalingFactor::ONE_HALF);

        // Thumbnail case: 1/8 of 5260x3511 = 658x439 covers 128x86.
        let f = choose_turbojpeg_scaling_factor(5260, 3511, 128, 86);
        assert_eq!(f, turbojpeg::ScalingFactor::ONE_EIGHTH);

        // Quarter slot: 1/8 would not cover 800x600 but 1/4 does.
        let f = choose_turbojpeg_scaling_factor(4000, 3000, 800, 600);
        assert_eq!(f, turbojpeg::ScalingFactor::ONE_QUARTER);

        // Full decode when nothing smaller covers the target.
        let f = choose_turbojpeg_scaling_factor(4000, 3000, 3900, 2900);
        assert_eq!(f, turbojpeg::ScalingFactor::ONE);
    }

    #[test]
    fn scaling_needs_account_for_orientation_swap() {
        // 3000x4000 (portrait stored) with orientation 6 -> effective
        // 4000x3000 landscape; into 1920x1440 box the needed stored dims are
        // 1440x1920 (swapped back), so 1/2 decode (1500x2000) suffices.
        let (eff_w, eff_h) = oriented_src_dims(3000, 4000, Some(6));
        assert_eq!((eff_w, eff_h), (4000, 3000));
        let (final_w, final_h) = calculate_thumbnail_dimensions(eff_w, eff_h, 1920, 1440);
        assert_eq!((final_w, final_h), (1920, 1440));
        let (need_w, need_h) = (final_h, final_w); // swapped back
        let f = choose_turbojpeg_scaling_factor(3000, 4000, need_w as usize, need_h as usize);
        assert_eq!(f, turbojpeg::ScalingFactor::ONE_HALF);
    }

    fn write_test_ico(path: &Path, sizes: &[u32]) {
        let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
        for &size in sizes {
            let rgba = vec![200u8; (size * size * 4) as usize];
            let image = ico::IconImage::from_rgba_data(size, size, rgba);
            let entry = ico::IconDirEntry::encode(&image).unwrap();
            icon_dir.add_entry(entry);
        }
        let file = std::fs::File::create(path).unwrap();
        icon_dir.write(file).unwrap();
    }

    #[test]
    fn ico_and_uppercase_ico_thumbnail_preview_and_dimensions() {
        use image::GenericImageView as _;

        let dir = std::env::temp_dir().join(format!("mediasort_ico_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let lower_ico = dir.join("multi.ico");
        write_test_ico(&lower_ico, &[16, 48, 256]);

        // Thumbnail picks largest entry <= box (48x48 for 128x128, 16x16 for 32x32).
        let thumb_128 = super::super::thumbnail::generate_thumbnail(&lower_ico, 128, 128)
            .unwrap()
            .expect("ico thumbnail expected");
        assert_eq!((thumb_128.width, thumb_128.height), (48, 48));

        let thumb_32 = super::super::thumbnail::generate_thumbnail(&lower_ico, 32, 32)
            .unwrap()
            .expect("ico thumbnail expected");
        assert_eq!((thumb_32.width, thumb_32.height), (16, 16));

        let dec_thumb = super::super::image_decoder::generate_thumbnail(&lower_ico, 128, 128)
            .expect("image_decoder::generate_thumbnail should handle .ico");
        assert_eq!(dec_thumb.dimensions(), (48, 48));

        // Preview, load_image, and dimension queries pick the largest entry (256x256).
        let preview = super::super::image_decoder::load_preview(&lower_ico, 1920, 1440).unwrap();
        assert_eq!((preview.width, preview.height), (256, 256));

        let loaded = super::super::image_decoder::load_image(&lower_ico).unwrap();
        assert_eq!(loaded.dimensions(), (256, 256));

        assert_eq!(
            super::super::image_decoder::decode_image_dimensions(&lower_ico).unwrap(),
            (256, 256)
        );
        assert_eq!(
            super::super::thumbnail::thumbnail_dimensions(&lower_ico).unwrap(),
            Some((256, 256))
        );

        // Uppercase .ICO where no entry fits 32x32: picks largest overall (256x256)
        // and resizes down to 32x32 for thumbnail, while preview decodes 256x256.
        let upper_ico = dir.join("LARGE.ICO");
        write_test_ico(&upper_ico, &[64, 256]);

        let upper_thumb = super::super::thumbnail::generate_thumbnail(&upper_ico, 32, 32)
            .unwrap()
            .expect("uppercase .ICO thumbnail expected");
        assert_eq!((upper_thumb.width, upper_thumb.height), (32, 32));

        let upper_preview =
            super::super::image_decoder::load_preview(&upper_ico, 1920, 1440).unwrap();
        assert_eq!((upper_preview.width, upper_preview.height), (256, 256));
        assert_eq!(
            super::super::image_decoder::decode_image_dimensions(&upper_ico).unwrap(),
            (256, 256)
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
