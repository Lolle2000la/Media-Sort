use std::path::Path;

use std::io::SeekFrom;

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, IntoImageView, ResizeAlg, ResizeOptions, Resizer};

use super::thumbnail::calculate_thumbnail_dimensions;

/// Maximum image dimension (per side) accepted by any decode path.
pub const MAX_DECODE_DIMENSION: u32 = 16_384;

/// Decode budget applied to every full image decode: no header dimension
/// above `MAX_DECODE_DIMENSION` px per side. image-crate decode paths are
/// additionally capped at 256 MiB of decoder allocation (`max_alloc`), so a
/// crafted header claiming a huge canvas cannot drive multi-GB transient
/// allocations in parallel thumbnail generation — the image crate's own
/// default (`Limits::default()`) has no dimension caps at all and only a
/// non-strict 512 MiB allocation cap. The turbojpeg path never consults
/// `max_alloc`; it is bounded by the dimension cap plus an explicit
/// allocation-site guard in `format_pipeline::decode_jpeg_turbojpeg_scaled`.
pub fn image_decode_limits() -> image::Limits {
    // `Limits` is #[non_exhaustive]; build via Default and mutate the
    // public fields.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

/// Open `path`, guess its format, apply the shared decode budget and decode.
pub fn decode_path_with_limits(path: &Path) -> Result<image::DynamicImage, image::ImageError> {
    let mut reader = image::ImageReader::open(path)?;
    reader.limits(image_decode_limits());
    reader.with_guessed_format()?.decode()
}

/// Decode in-memory image bytes under the shared decode budget.
pub fn decode_bytes_with_limits(bytes: &[u8]) -> Result<image::DynamicImage, image::ImageError> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
    reader.limits(image_decode_limits());
    reader.with_guessed_format()?.decode()
}

pub fn is_animated_gif(path: &Path) -> Option<bool> {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase() != "gif")
        .unwrap_or(true)
    {
        return None;
    }

    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    is_animated_gif_reader(&mut reader)
}

/// Header-only GIF scan: counts image descriptors (0x2C) without decoding
/// any frame data. A GIF is "animated" when it contains at least two images.
///
/// Decoding frames to answer this question is unsafe: `GifDecoder` allocates
/// `logical_screen_width * logical_screen_height * 4` on the first frame
/// read (image 0.25.10 gif.rs:279-291) with no size limit, so a crafted
/// 65535x65535 screen descriptor in a 15-byte file forces a ~16 GiB
/// allocation. The header scan below allocates nothing and only seeks.
fn is_animated_gif_reader<R: std::io::Read + std::io::Seek>(reader: &mut R) -> Option<bool> {
    let mut header = [0u8; 13];
    reader.read_exact(&mut header).ok()?;
    if &header[0..6] != b"GIF87a" && &header[0..6] != b"GIF89a" {
        return None;
    }

    // Logical screen descriptor: width(2) height(2) packed(1) bg(1) aspect(1).
    // Skip the global color table when the packed byte says one is present.
    let packed = header[10];
    let gct_bytes = if packed & 0x80 != 0 {
        3usize << ((packed & 0x07) + 1)
    } else {
        0
    };

    let file_len = reader.seek(SeekFrom::End(0)).ok()?;
    let start = 13u64.checked_add(gct_bytes as u64)?;
    if start > file_len {
        return None;
    }
    reader.seek(SeekFrom::Start(start)).ok()?;

    let mut images = 0u32;
    let mut blocks = 0u32;
    let mut block = [0u8; 1];
    loop {
        blocks += 1;
        if blocks > 10_000 {
            return None;
        }
        if reader.read_exact(&mut block).is_err() {
            return None;
        }
        match block[0] {
            // Trailer: end of image data. Only a scan that reaches the
            // trailer (or positively detects a second image descriptor)
            // yields Some; any other termination — malformed input, block
            // cap, unexpected EOF — returns None so a corrupt GIF is never
            // misclassified as a known-static one.
            0x3B => return Some(images >= 2),
            0x2C => {
                // image descriptor: left(2) top(2) width(2) height(2) packed(1)
                images += 1;
                if images >= 2 {
                    return Some(true);
                }
                let mut descriptor = [0u8; 9];
                if reader.read_exact(&mut descriptor).is_err() {
                    return None;
                }
                // local color table, then the LZW minimum code size byte,
                // then the sub-block data stream
                let lct_bytes = if descriptor[8] & 0x80 != 0 {
                    3usize << ((descriptor[8] & 0x07) + 1)
                } else {
                    0
                };
                if reader
                    .seek(SeekFrom::Current(lct_bytes as i64 + 1))
                    .is_err()
                {
                    return None;
                }
                if !skip_sub_blocks(reader) {
                    return None;
                }
            }
            0x21 => {
                // extension: 1-byte label, then the sub-block data stream.
                // The plain-text extension (label 0x01) carries a 12-byte
                // header before its sub-blocks.
                let mut label = [0u8; 1];
                if reader.read_exact(&mut label).is_err() {
                    return None;
                }
                if label[0] == 0x01 && reader.seek(SeekFrom::Current(12)).is_err() {
                    return None;
                }
                if !skip_sub_blocks(reader) {
                    return None;
                }
            }
            _ => return None, // unknown block start: not a parseable GIF
        }
    }
}

/// Skip a chain of GIF sub-blocks (length-prefixed chunks terminated by a
/// zero-length chunk), returning `false` on malformed or pathological input.
fn skip_sub_blocks<R: std::io::Read + std::io::Seek>(reader: &mut R) -> bool {
    let mut len = [0u8; 1];
    let mut chunks = 0u32;
    loop {
        chunks += 1;
        if chunks > 1_000_000 {
            return false;
        }
        if reader.read_exact(&mut len).is_err() {
            return false;
        }
        if len[0] == 0 {
            return true;
        }
        if reader.seek(SeekFrom::Current(len[0] as i64)).is_err() {
            return false;
        }
    }
}

pub fn load_image(path: &Path) -> Result<image::DynamicImage, image::ImageError> {
    let img = decode_path_with_limits(path)?;
    Ok(apply_orientation(img, path))
}

pub fn decode_image_dimensions(path: &Path) -> Result<(u32, u32), image::ImageError> {
    let (w, h) = image::ImageReader::open(path)?
        .with_guessed_format()?
        .into_dimensions()?;
    let orientation = read_exif_orientation_from_path(path).unwrap_or(1);
    Ok(if matches!(orientation, 5..=8) {
        (h, w)
    } else {
        (w, h)
    })
}

fn read_exif_orientation_from_path(path: &Path) -> Option<u32> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = std::io::BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut buf).ok()?;
    let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
    field.value.get_uint(0)
}

/// Decode `path` to fit inside the `max_width` x `max_height` box (preserving
/// aspect ratio), using the per-format optimized pipeline.
///
/// Image formats only. This is the preview path, and the GUI only asks for
/// previews of `MediaType::Image` entries (`load_full_image` returns no task
/// otherwise); audio cover art goes through
/// [`super::thumbnail::generate_thumbnail`], which reports "no embedded cover
/// art" as `Ok(None)` rather than an error.
pub fn load_preview(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<super::DecodedImage, image::ImageError> {
    super::format_pipeline::process_image(path, max_width, max_height)
}

pub fn generate_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<image::DynamicImage, image::ImageError> {
    let img = decode_path_with_limits(path)?;
    let img = apply_orientation(img, path);

    let img_rgba = img.to_rgba8();
    let (src_w, src_h) = img_rgba.dimensions();
    let (dst_w, dst_h) = calculate_thumbnail_dimensions(src_w, src_h, max_width, max_height);

    if dst_w == 0 || dst_h == 0 {
        return Ok(image::DynamicImage::ImageRgba8(img_rgba));
    }

    let mut dst_image = Image::new(
        dst_w,
        dst_h,
        img_rgba
            .pixel_type()
            .unwrap_or(fast_image_resize::PixelType::U8x4),
    );
    let mut resizer = Resizer::new();
    resizer
        .resize(
            &img_rgba,
            &mut dst_image,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .map_err(|e| {
            image::ImageError::Decoding(image::error::DecodingError::new(
                image::error::ImageFormatHint::Unknown,
                e.to_string(),
            ))
        })?;

    let buffer = dst_image.into_vec();
    Ok(image::DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(dst_w, dst_h, buffer).ok_or_else(|| {
            image::ImageError::Limits(image::error::LimitError::from_kind(
                image::error::LimitErrorKind::DimensionError,
            ))
        })?,
    ))
}

pub fn apply_orientation(mut img: image::DynamicImage, path: &Path) -> image::DynamicImage {
    if let Ok(file) = std::fs::File::open(path) {
        let mut buf_reader = std::io::BufReader::new(&file);
        if let Ok(exif) = exif::Reader::new().read_from_container(&mut buf_reader)
            && let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
            && let Some(val) = field.value.get_uint(0)
        {
            match val {
                2 => img = img.fliph(),
                3 => img = img.rotate180(),
                4 => img = img.flipv(),
                5 => img = img.fliph().rotate270(),
                6 => img = img.rotate90(),
                7 => img = img.fliph().rotate90(),
                8 => img = img.rotate270(),
                _ => {}
            }
        }
    }
    img
}
