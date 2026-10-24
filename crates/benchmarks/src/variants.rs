use std::io::Cursor;
use std::path::Path;

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::GenericImageView;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_jpeg::JpegDecoder;

pub type VariantResult = Result<(u32, u32, Vec<u8>), String>;

fn calculate_thumbnail_dimensions(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let ratio = src_w as f64 / src_h as f64;
    let max_ratio = max_w as f64 / max_h as f64;

    if ratio > max_ratio {
        let w = max_w;
        let h = (max_w as f64 / ratio).round() as u32;
        (w, h.max(1))
    } else {
        let w = (max_h as f64 * ratio).round() as u32;
        let h = max_h;
        (w.max(1), h)
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
    let src_img = image::RgbaImage::from_raw(src_w, src_h, src.to_vec())
        .ok_or_else(|| "failed to create RgbaImage from raw data".to_string())?;
    let mut dst_img = Image::new(dst_w, dst_h, PixelType::U8x4);
    let mut resizer = Resizer::new();
    resizer
        .resize(
            &src_img,
            &mut dst_img,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .map_err(|e| format!("{e}"))?;
    Ok(dst_img.into_vec())
}

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
    let len = rgba.len();
    for i in 0..len / 2 {
        rgba.swap(i, len - 1 - i);
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

fn is_jpg_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))
}

fn decode_jpeg_zune(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = JpegDecoder::new(ZCursor::new(bytes));
    decoder
        .decode_headers()
        .map_err(|e| format!("zune headers: {e:?}"))?;
    let info = decoder.info().ok_or("zune: no info after headers")?;
    let w = u32::from(info.width);
    let h = u32::from(info.height);

    let options = decoder.options().jpeg_set_out_colorspace(ColorSpace::RGBA);
    decoder.set_options(options);

    let pixels = decoder
        .decode()
        .map_err(|e| format!("zune decode: {e:?}"))?;
    Ok((w, h, pixels))
}

pub fn calculate_thumbnail_dimensions_for_test(
    src_w: u32,
    src_h: u32,
    max_w: u32,
    max_h: u32,
) -> (u32, u32) {
    calculate_thumbnail_dimensions(src_w, src_h, max_w, max_h)
}

pub fn resize_rgba_for_test(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, String> {
    resize_rgba(src, src_w, src_h, dst_w, dst_h)
}

// ── Image thumbnail variants ──────────────────────────────────────────

/// Baseline 1: exact replication of the current GUI image thumbnail path.
pub fn baseline_gui(path: &Path) -> VariantResult {
    let img =
        media_sort_backend::media::image_decoder::load_image(path).map_err(|e| format!("{e}"))?;
    let thumbnail = img.thumbnail(128, 128).to_rgba8();
    let (w, h) = thumbnail.dimensions();
    Ok((w, h, thumbnail.into_raw()))
}

/// Baseline 2: backend `generate_thumbnail` (includes wasteful symphonia probe).
pub fn backend_fir(path: &Path) -> VariantResult {
    media_sort_backend::media::thumbnail::generate_thumbnail(path, 128, 128)
        .map_err(|e| format!("{e}"))?
        .map(|d| d.into_parts())
        .ok_or_else(|| "backend thumbnail produced no image".to_string())
}

/// Like `backend_fir` but skip `extract_audio_cover` entirely for image extensions.
pub fn fir_no_probe(path: &Path) -> VariantResult {
    let img = image::ImageReader::open(path)
        .map_err(|e| format!("{e}"))?
        .with_guessed_format()
        .map_err(|e| format!("{e}"))?
        .decode()
        .map_err(|e| format!("{e}"))?;
    let img = media_sort_backend::media::image_decoder::apply_orientation(img, path);
    let img_rgba = img.to_rgba8();
    let (src_w, src_h) = img_rgba.dimensions();
    let (dst_w, dst_h) = calculate_thumbnail_dimensions(src_w, src_h, 128, 128);

    if dst_w == 0 || dst_h == 0 {
        return Ok((src_w, src_h, img_rgba.into_raw()));
    }

    let resized = resize_rgba(&img_rgba.into_raw(), src_w, src_h, dst_w, dst_h)?;
    Ok((dst_w, dst_h, resized))
}

/// Read file bytes ONCE, decode from memory, parse EXIF from memory,
/// apply orientation on the SMALL resized image (key optimization).
pub fn no_exif_reread(path: &Path) -> VariantResult {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let orientation = parse_exif_orientation_from_bytes(&bytes);

    let img = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
    let img_rgba = img.to_rgba8();
    let (src_w, src_h) = img_rgba.dimensions();

    resize_and_orient(&img_rgba.into_raw(), src_w, src_h, orientation, 128, 128)
}

/// Use zune-jpeg for JPEG, fall back to `image` crate for PNG.
/// Parse EXIF from in-memory bytes, orient after resize.
pub fn zune_decode(path: &Path) -> VariantResult {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let orientation = parse_exif_orientation_from_bytes(&bytes);

    let (src_w, src_h, decoded_rgba) = if is_jpg_ext(path) {
        decode_jpeg_zune(&bytes)?
    } else {
        let img = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        (w, h, rgba.into_raw())
    };

    resize_and_orient(&decoded_rgba, src_w, src_h, orientation, 128, 128)
}

/// Same as `zune_decode` but skip EXIF/orientation entirely.
pub fn zune_decode_no_orient(path: &Path) -> VariantResult {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;

    let (src_w, src_h, decoded_rgba) = if is_jpg_ext(path) {
        decode_jpeg_zune(&bytes)?
    } else {
        let img = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        (w, h, rgba.into_raw())
    };

    resize_and_orient(&decoded_rgba, src_w, src_h, None, 128, 128)
}

// ── Preview variants ──────────────────────────────────────────────────

/// Baseline 3: full-resolution RGBA from `load_image`.
pub fn baseline_full(path: &Path) -> VariantResult {
    let img =
        media_sort_backend::media::image_decoder::load_image(path).map_err(|e| format!("{e}"))?;
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8().into_raw();
    Ok((w, h, rgba))
}

/// Full decode, fast_image_resize to fit 1920×1440, orient after resize.
pub fn fir_downscale(path: &Path) -> VariantResult {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let orientation = parse_exif_orientation_from_bytes(&bytes);

    let img = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
    let img_rgba = img.to_rgba8();
    let (src_w, src_h) = img_rgba.dimensions();

    resize_and_orient(&img_rgba.into_raw(), src_w, src_h, orientation, 1920, 1440)
}

/// zune-jpeg decode + fast_image_resize to fit 1920×1440, orient after resize.
pub fn zune_preview(path: &Path) -> VariantResult {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let orientation = parse_exif_orientation_from_bytes(&bytes);

    let (src_w, src_h, decoded_rgba) = if is_jpg_ext(path) {
        decode_jpeg_zune(&bytes)?
    } else {
        let img = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        (w, h, rgba.into_raw())
    };

    resize_and_orient(&decoded_rgba, src_w, src_h, orientation, 1920, 1440)
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

fn resize_and_orient(
    src_rgba: &[u8],
    src_w: u32,
    src_h: u32,
    orientation: Option<u32>,
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let (eff_w, eff_h) = oriented_src_dims(src_w, src_h, orientation);
    let (final_w, final_h) = calculate_thumbnail_dimensions(eff_w, eff_h, max_w, max_h);

    if final_w == 0 || final_h == 0 {
        return Ok((src_w, src_h, src_rgba.to_vec()));
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
        Ok((fw, fh, resized))
    } else {
        Ok((resize_w, resize_h, resized))
    }
}

fn choose_turbojpeg_scaling_factor(
    width: usize,
    height: usize,
    target_min: u32,
) -> turbojpeg::ScalingFactor {
    let t = target_min as usize;
    let candidates = [
        turbojpeg::ScalingFactor::ONE_EIGHTH,
        turbojpeg::ScalingFactor::ONE_HALF,
        turbojpeg::ScalingFactor::ONE,
    ];
    for &factor in &candidates {
        if factor.scale(width) >= t && factor.scale(height) >= t {
            return factor;
        }
    }
    turbojpeg::ScalingFactor::ONE
}

fn decode_jpeg_turbojpeg_scaled(
    bytes: &[u8],
    target_min: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decompressor =
        turbojpeg::Decompressor::new().map_err(|e| format!("turbojpeg init: {e}"))?;
    let header = decompressor
        .read_header(bytes)
        .map_err(|e| format!("turbojpeg header: {e}"))?;
    let factor = choose_turbojpeg_scaling_factor(header.width, header.height, target_min);
    let scaled = header.scaled(factor);
    decompressor
        .set_scaling_factor(factor)
        .map_err(|e| format!("turbojpeg set_scale: {e}"))?;
    let pitch = scaled.width * 4;
    let mut image = turbojpeg::Image {
        pixels: vec![0u8; scaled.height * pitch],
        width: scaled.width,
        pitch,
        height: scaled.height,
        format: turbojpeg::PixelFormat::RGBA,
    };
    decompressor
        .decompress(bytes, image.as_deref_mut())
        .map_err(|e| format!("turbojpeg decompress: {e}"))?;
    Ok((scaled.width as u32, scaled.height as u32, image.pixels))
}

/// Use turbojpeg scaled decompress + fast_image_resize, orient after resize.
pub fn turbojpeg_scaled(path: &Path) -> VariantResult {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let orientation = parse_exif_orientation_from_bytes(&bytes);

    let (src_w, src_h, decoded_rgba) = if is_jpg_ext(path) {
        decode_jpeg_turbojpeg_scaled(&bytes, 128)?
    } else {
        let img = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        (w, h, rgba.into_raw())
    };

    resize_and_orient(&decoded_rgba, src_w, src_h, orientation, 128, 128)
}

/// turbojpeg scaled decode + fast_image_resize to fit 1920×1440, orient after resize.
pub fn turbojpeg_preview(path: &Path) -> VariantResult {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
    let orientation = parse_exif_orientation_from_bytes(&bytes);

    let (src_w, src_h, decoded_rgba) = if is_jpg_ext(path) {
        decode_jpeg_turbojpeg_scaled(&bytes, 1920)?
    } else {
        let img = image::load_from_memory(&bytes).map_err(|e| format!("decode: {e}"))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        (w, h, rgba.into_raw())
    };

    resize_and_orient(&decoded_rgba, src_w, src_h, orientation, 1920, 1440)
}

/// Spawn ffmpeg to extract a 128×128-fit frame as raw RGBA.
pub fn ffmpeg_extract(_player: &mut MpvContext, path: &Path) -> VariantResult {
    let probe_out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe spawn: {e}"))?;

    let probe_str =
        String::from_utf8(probe_out.stdout).map_err(|e| format!("ffprobe utf8: {e}"))?;
    let parts: Vec<&str> = probe_str.trim().split(',').collect();
    let src_w: u32 = parts
        .first()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1280);
    let src_h: u32 = parts
        .get(1)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(720);

    let scale_w = 128.min(src_w);
    let scale_h = 128.min(src_h);
    let (out_w, out_h) = if src_w as u64 * scale_h as u64 <= src_h as u64 * scale_w as u64 {
        ((scale_h * src_w / src_h).max(1), scale_h)
    } else {
        (scale_w, (scale_w * src_h / src_w).max(1))
    };

    let vf = format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,setsar=1",
        scale_w, scale_h
    );

    let ff_out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-ss", "0", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &vf,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg spawn: {e}"))?;

    let rgba = ff_out.stdout;
    let expected_len = (out_w * out_h * 4) as usize;
    if rgba.len() < expected_len {
        return Err(format!(
            "ffmpeg output too short: {} < {}",
            rgba.len(),
            expected_len
        ));
    }

    Ok((out_w, out_h, rgba[..expected_len].to_vec()))
}

// ── Video thumbnail variants ──────────────────────────────────────────

use mpv_utils::{MpvContext, rotate_rgba};

/// Baseline 4: exact replication of the current video thumbnail path.
pub fn baseline_poll_10ms(player: &mut MpvContext, path: &Path) -> VariantResult {
    poll_based_thumbnail(player, path, std::time::Duration::from_millis(10))
}

/// Same as baseline but 1ms sleep interval.
pub fn poll_1ms(player: &mut MpvContext, path: &Path) -> VariantResult {
    poll_based_thumbnail(player, path, std::time::Duration::from_millis(1))
}

/// No sleep (spin/yield) — measures polling overhead contribution.
pub fn poll_0ms_spin(player: &mut MpvContext, path: &Path) -> VariantResult {
    poll_based_thumbnail(player, path, std::time::Duration::from_nanos(0))
}

/// Seek to 10% position before frame capture.
pub fn seek_10pct(player: &mut MpvContext, path: &Path) -> VariantResult {
    player.stop();

    if let Err(e) = player.load_file(path) {
        return Err(format!("Failed to load video: {e}"));
    }
    player.set_paused(true);

    let mut result = Err(format!(
        "Timed out generating video frame thumbnail for {}",
        path.display()
    ));
    let start = std::time::Instant::now();
    let mut seek_done = false;
    let target_canonical = path.canonicalize().ok();

    while start.elapsed() < std::time::Duration::from_millis(1000) {
        if player.has_frame_ready()
            && let Some(current_p_str) = player.get_current_path()
        {
            let current_p = std::path::PathBuf::from(current_p_str);
            let paths_match = current_p == path
                || target_canonical.as_ref().is_some_and(|tc| {
                    current_p == *tc || current_p.canonicalize().ok().as_ref() == Some(tc)
                });

            if paths_match {
                if !seek_done {
                    let (w, h) = player.get_video_size();
                    if w > 0 && h > 0 {
                        let dur = player.get_duration();
                        if dur > 0.0 {
                            player.seek(dur * 0.1);
                        }
                        seek_done = true;
                        continue;
                    }
                }

                let (w, h) = player.get_video_size();
                if w > 0 && h > 0 {
                    let rotate = player.get_video_rotation();
                    let (eff_w, eff_h) = if rotate.is_swapped() { (h, w) } else { (w, h) };

                    let max_w = 128.0f64;
                    let max_h = 128.0f64;
                    let scale = (max_w / eff_w as f64).min(max_h / eff_h as f64).min(1.0);

                    let render_w = ((w as f64 * scale) as i32) & !1;
                    let render_h = ((h as f64 * scale) as i32) & !1;

                    if render_w > 0 && render_h > 0 {
                        let mut buffer = vec![0u8; (render_w * render_h * 4) as usize];
                        if player.render_frame(render_w, render_h, &mut buffer).is_ok() {
                            let (final_w, final_h, final_rgba) =
                                rotate_rgba(render_w as u32, render_h as u32, &buffer, rotate);
                            result = Ok((final_w, final_h, final_rgba));
                            break;
                        }
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    player.stop();
    result
}

fn poll_based_thumbnail(
    player: &mut MpvContext,
    path: &Path,
    sleep: std::time::Duration,
) -> VariantResult {
    player.stop();

    if let Err(e) = player.load_file(path) {
        return Err(format!("Failed to load video: {e}"));
    }
    player.set_paused(true);

    let mut result = Err(format!(
        "Timed out generating video frame thumbnail for {}",
        path.display()
    ));
    let start = std::time::Instant::now();
    let target_canonical = path.canonicalize().ok();

    while start.elapsed() < std::time::Duration::from_millis(1000) {
        if player.has_frame_ready()
            && let Some(current_p_str) = player.get_current_path()
        {
            let current_p = std::path::PathBuf::from(current_p_str);
            let paths_match = current_p == path
                || target_canonical.as_ref().is_some_and(|tc| {
                    current_p == *tc || current_p.canonicalize().ok().as_ref() == Some(tc)
                });

            if paths_match {
                let (w, h) = player.get_video_size();
                if w > 0 && h > 0 {
                    let rotate = player.get_video_rotation();
                    let (eff_w, eff_h) = if rotate.is_swapped() { (h, w) } else { (w, h) };

                    let max_w = 128.0f64;
                    let max_h = 128.0f64;
                    let scale = (max_w / eff_w as f64).min(max_h / eff_h as f64).min(1.0);

                    let render_w = ((w as f64 * scale) as i32) & !1;
                    let render_h = ((h as f64 * scale) as i32) & !1;

                    if render_w > 0 && render_h > 0 {
                        let mut buffer = vec![0u8; (render_w * render_h * 4) as usize];
                        if player.render_frame(render_w, render_h, &mut buffer).is_ok() {
                            let (final_w, final_h, final_rgba) =
                                rotate_rgba(render_w as u32, render_h as u32, &buffer, rotate);
                            result = Ok((final_w, final_h, final_rgba));
                            break;
                        }
                    }
                }
            }
        }
        if sleep.is_zero() {
            std::thread::yield_now();
        } else {
            std::thread::sleep(sleep);
        }
    }

    player.stop();
    result
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/MockState"
        ))
        .join(name)
    }

    fn has_ffmpeg_tools() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            && std::process::Command::new("ffprobe")
                .arg("-version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
    }

    macro_rules! assert_thumbnail_valid {
        ($result:expr) => {
            let (w, h, rgba) = $result.unwrap();
            assert!(w > 0, "width must be > 0");
            assert!(h > 0, "height must be > 0");
            assert!(w <= 128, "width {w} > 128");
            assert!(h <= 128, "height {h} > 128");
            assert_eq!(
                rgba.len(),
                (w * h * 4) as usize,
                "rgba len {} != {}*4",
                rgba.len(),
                w * h
            );
        };
    }

    macro_rules! assert_preview_valid {
        ($result:expr) => {
            let (w, h, rgba) = $result.unwrap();
            assert!(w > 0);
            assert!(h > 0);
            assert!(w <= 1920);
            assert!(h <= 1440);
            assert_eq!(rgba.len(), (w * h * 4) as usize);
        };
    }

    #[test]
    fn test_baseline_gui() {
        assert_thumbnail_valid!(baseline_gui(&fixture("mock 1.jpg")));
        assert_thumbnail_valid!(baseline_gui(&fixture("mock 5.png")));
    }

    #[test]
    fn test_backend_fir() {
        assert_thumbnail_valid!(backend_fir(&fixture("mock 1.jpg")));
        assert_thumbnail_valid!(backend_fir(&fixture("mock 5.png")));
    }

    #[test]
    fn test_fir_no_probe() {
        assert_thumbnail_valid!(fir_no_probe(&fixture("mock 1.jpg")));
        assert_thumbnail_valid!(fir_no_probe(&fixture("mock 5.png")));
    }

    #[test]
    fn test_no_exif_reread() {
        assert_thumbnail_valid!(no_exif_reread(&fixture("mock 1.jpg")));
        assert_thumbnail_valid!(no_exif_reread(&fixture("mock 5.png")));
    }

    #[test]
    fn test_zune_decode() {
        assert_thumbnail_valid!(zune_decode(&fixture("mock 1.jpg")));
        assert_thumbnail_valid!(zune_decode(&fixture("mock 5.png")));
    }

    #[test]
    fn test_zune_decode_no_orient() {
        assert_thumbnail_valid!(zune_decode_no_orient(&fixture("mock 1.jpg")));
    }

    #[test]
    fn test_baseline_full() {
        let (w, h, rgba) = baseline_full(&fixture("mock 1.jpg")).unwrap();
        assert!(w > 0 && h > 0);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        let (w, h, rgba) = baseline_full(&fixture("mock 5.png")).unwrap();
        assert!(w > 0 && h > 0);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
    }

    #[test]
    fn test_fir_downscale() {
        assert_preview_valid!(fir_downscale(&fixture("mock 1.jpg")));
        assert_preview_valid!(fir_downscale(&fixture("mock 5.png")));
    }

    #[test]
    fn test_zune_preview() {
        assert_preview_valid!(zune_preview(&fixture("mock 1.jpg")));
        assert_preview_valid!(zune_preview(&fixture("mock 5.png")));
    }

    #[test]
    fn test_orientation_thumbs_agree() {
        let gui = baseline_gui(&fixture("mock 1.jpg")).unwrap();
        let (w, h, rgba) = no_exif_reread(&fixture("mock 1.jpg")).unwrap();
        assert_eq!((w, h), (gui.0, gui.1), "dimensions must match baseline_gui");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
    }

    #[test]
    fn test_turbojpeg_scaled() {
        assert_thumbnail_valid!(turbojpeg_scaled(&fixture("mock 1.jpg")));
        assert_thumbnail_valid!(turbojpeg_scaled(&fixture("mock 2.jpg")));
        assert_thumbnail_valid!(turbojpeg_scaled(&fixture("mock 5.png")));
    }

    #[test]
    fn test_turbojpeg_scaled_exif_orientation() {
        let dir = std::env::temp_dir().join("mediasort_bench_turbo_thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_oriented.jpg");

        let img = image::RgbImage::from_pixel(100, 50, image::Rgb([255, 0, 0]));
        let mut jpeg_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
        img.write_to(&mut cursor, image::ImageFormat::Jpeg).unwrap();

        let exif_app1: &[u8] = &[
            0xFF, 0xE1, 0x00, 0x22, 0x45, 0x78, 0x69, 0x66, 0x00, 0x00, 0x49, 0x49, 0x2A, 0x00,
            0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut oriented_jpeg = Vec::new();
        oriented_jpeg.extend_from_slice(&jpeg_bytes[..2]);
        oriented_jpeg.extend_from_slice(exif_app1);
        oriented_jpeg.extend_from_slice(&jpeg_bytes[2..]);
        std::fs::write(&path, &oriented_jpeg).unwrap();

        let (w, h, rgba) = turbojpeg_scaled(&path).unwrap();
        assert!(
            h > w,
            "Expected height {h} > width {w} due to EXIF orientation 6"
        );
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_turbojpeg_preview() {
        assert_preview_valid!(turbojpeg_preview(&fixture("mock 1.jpg")));
        assert_preview_valid!(turbojpeg_preview(&fixture("mock 5.png")));
    }

    #[test]
    fn test_ffmpeg_extract() {
        let mut player = match MpvContext::new_thumbnail_player() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SKIP: MpvContext::new_thumbnail_player() failed: {e}");
                return;
            }
        };
        if !has_ffmpeg_tools() {
            eprintln!("SKIP: ffmpeg/ffprobe not found on PATH");
            return;
        }
        let (w, h, rgba) = ffmpeg_extract(&mut player, &fixture("mock 3.mp4")).unwrap();
        assert!(w > 0 && h > 0);
        assert!(w <= 128 && h <= 128);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
    }
}
