use std::path::PathBuf;

use id3::TagLike;
use image::ImageFormat;
use media_sort_backend::media::DecodedImage;
use media_sort_backend::media::image_decoder::load_preview;
use media_sort_backend::media::thumbnail;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn mock_state_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources/MockState")
}

fn temp_dir(name: &str) -> PathBuf {
    // Unique per call: tests in this binary run in parallel threads of the
    // SAME process, and per-extension dirs shared between the thumbnail and
    // preview tests race (one test's remove_dir_all deletes the other's
    // files mid-assert -> sporadic NotFound).
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mediasort_{name}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Generate a `w x h` RGBA source image in a unique temp dir, saved in `fmt`.
fn save_source(fmt: ImageFormat, w: u32, h: u32) -> PathBuf {
    let ext = fmt.extensions_str()[0];
    let dir = temp_dir(&format!("fmt_{ext}"));
    let path = dir.join(format!("src.{ext}"));
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
    });
    if fmt == ImageFormat::Jpeg {
        let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
        rgb.save_with_format(&path, fmt).unwrap();
    } else {
        img.save_with_format(&path, fmt).unwrap();
    }
    path
}

fn assert_aspect(src_w: u32, src_h: u32, w: u32, h: u32) {
    let src_ratio = src_w as f64 / src_h as f64;
    let dst_ratio = w as f64 / h as f64;
    assert!(
        (src_ratio - dst_ratio).abs() < 0.03,
        "aspect ratio not preserved: source {src_w}x{src_h} -> {w}x{h}"
    );
}

fn assert_thumbnail(
    src_w: u32,
    src_h: u32,
    result: Result<Option<DecodedImage>, image::ImageError>,
) {
    let DecodedImage {
        width: w,
        height: h,
        rgba,
    } = result.unwrap().expect("expected a thumbnail image");
    assert!(w > 0, "width must be > 0");
    assert!(h > 0, "height must be > 0");
    assert!(w <= 128, "thumbnail width {w} exceeds 128");
    assert!(h <= 128, "thumbnail height {h} exceeds 128");
    assert_eq!(
        rgba.len(),
        (w * h * 4) as usize,
        "rgba len {} != {w}*{h}*4",
        rgba.len()
    );
    assert_aspect(src_w, src_h, w, h);
}

fn assert_preview(src_w: u32, src_h: u32, result: Result<DecodedImage, image::ImageError>) {
    let DecodedImage {
        width: w,
        height: h,
        rgba,
    } = result.unwrap();
    assert!(w > 0 && h > 0);
    assert!(w <= 1920, "preview width {w} exceeds 1920");
    assert!(h <= 1440, "preview height {h} exceeds 1440");
    assert_eq!(
        rgba.len(),
        (w * h * 4) as usize,
        "rgba len {} != {w}*{h}*4",
        rgba.len()
    );
    assert_aspect(src_w, src_h, w, h);
}

const PROGRAMMATIC_FORMATS: [ImageFormat; 5] = [
    ImageFormat::Png,
    ImageFormat::Bmp,
    ImageFormat::Tga,
    ImageFormat::Qoi,
    ImageFormat::Jpeg,
];

#[test]
fn test_thumbnail_programmatic_formats() {
    for fmt in PROGRAMMATIC_FORMATS {
        let path = save_source(fmt, 300, 200);
        assert_thumbnail(300, 200, thumbnail::generate_thumbnail(&path, 128, 128));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}

#[test]
fn test_thumbnail_large_jpeg_fixture() {
    let path = mock_state_dir().join("mock 1.jpg");
    assert_thumbnail(5260, 3511, thumbnail::generate_thumbnail(&path, 128, 128));
}

#[test]
fn test_thumbnail_exif_orientation() {
    let dir = temp_dir("exif_thumb");
    let path = dir.join("oriented.jpg");

    let img = image::RgbImage::from_pixel(100, 50, image::Rgb([255, 0, 0]));
    let mut jpeg_bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut jpeg_bytes),
        ImageFormat::Jpeg,
    )
    .unwrap();

    let exif_app1: &[u8] = &[
        0xFF, 0xE1, // APP1 marker
        0x00, 0x22, // Length of segment (34 bytes)
        0x45, 0x78, 0x69, 0x66, 0x00, 0x00, // "Exif\0\0"
        0x49, 0x49, 0x2A, 0x00, // TIFF header "II", 0x002A
        0x08, 0x00, 0x00, 0x00, // Offset to 0th IFD (8)
        0x01, 0x00, // Number of fields (1)
        0x12, 0x01, // Tag: 0x0112 (Orientation)
        0x03, 0x00, // Type: SHORT (3)
        0x01, 0x00, 0x00, 0x00, // Count: 1
        0x06, 0x00, 0x00, 0x00, // Value: 6 (rotate 90 CW)
        0x00, 0x00, 0x00, 0x00, // Offset to next IFD (0)
    ];

    let mut oriented_jpeg = Vec::new();
    oriented_jpeg.extend_from_slice(&jpeg_bytes[..2]); // 0xFF, 0xD8 (SOI)
    oriented_jpeg.extend_from_slice(exif_app1);
    oriented_jpeg.extend_from_slice(&jpeg_bytes[2..]); // rest of JPEG
    std::fs::write(&path, &oriented_jpeg).unwrap();

    let (w, h, rgba) = thumbnail::generate_thumbnail(&path, 128, 128)
        .unwrap()
        .expect("oriented JPEG should yield a thumbnail")
        .into_parts();
    assert!(
        h > w,
        "Expected height {h} > width {w} due to EXIF orientation 6 (90 deg CW)"
    );
    assert_eq!(rgba.len(), (w * h * 4) as usize);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_preview_programmatic_formats() {
    for fmt in PROGRAMMATIC_FORMATS {
        let path = save_source(fmt, 300, 200);
        assert_preview(300, 200, load_preview(&path, 1920, 1440));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}

#[test]
fn test_preview_large_jpeg_capped() {
    let path = mock_state_dir().join("mock 1.jpg");
    let (w, h, rgba) = load_preview(&path, 1920, 1440).unwrap().into_parts();
    assert!(w <= 1920 && h <= 1440, "preview not capped to box: {w}x{h}");
    assert!(
        w < 5260,
        "preview width should be downscaled from 5260, got {w}"
    );
    assert_eq!(rgba.len(), (w * h * 4) as usize);
}

#[test]
fn test_audio_cover_thumbnail() {
    let dir = temp_dir("audio_cover");
    let path = dir.join("cover.mp3");
    std::fs::write(&path, b"").unwrap();

    let cover = image::RgbaImage::from_pixel(300, 200, image::Rgba([255, 0, 0, 255]));
    let mut png_bytes = Vec::new();
    cover
        .write_to(&mut std::io::Cursor::new(&mut png_bytes), ImageFormat::Png)
        .unwrap();

    let mut tag = id3::Tag::new();
    tag.add_frame(id3::frame::Picture {
        mime_type: "image/png".to_string(),
        picture_type: id3::frame::PictureType::CoverFront,
        description: "cover".to_string(),
        data: png_bytes,
    });
    tag.write_to_path(&path, id3::Version::Id3v24).unwrap();

    assert_thumbnail(300, 200, thumbnail::generate_thumbnail(&path, 128, 128));
    std::fs::remove_dir_all(&dir).ok();
}

/// Write a minimal, tag-less PCM WAV file (canonical 44-byte RIFF header plus
/// silence), so the "no embedded cover art" path can be exercised without a
/// bundled fixture or an ffmpeg dependency.
fn write_silent_wav(path: &std::path::Path) {
    let sample_rate: u32 = 8000;
    let channels: u16 = 1;
    let bits: u16 = 8;
    let frames: u32 = 800; // 0.1 s
    let data_len = frames * u32::from(bits / 8) * u32::from(channels);
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits / 8);
    let block_align = channels * (bits / 8);

    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    buf.resize(44 + data_len as usize, 128u8); // silence (unsigned 8-bit center)
    std::fs::write(path, &buf).unwrap();
}

#[test]
fn test_audio_without_cover_yields_no_thumbnail() {
    // Regression: an audio file without embedded cover art used to be handed
    // to the *image* decoder as a fallback, which failed with "the file
    // extension `.wav` was not recognized as an image format" and marked the
    // whole file as unsupported. No cover art is not a decode failure — the
    // file must report "no thumbnail" and stay usable.
    let dir = temp_dir("audio_no_cover");
    let wav = dir.join("silent.wav");
    write_silent_wav(&wav);

    // The bug's exact reproduction case: a plain `.wav` with no tags.
    let result = thumbnail::generate_thumbnail(&wav, 128, 128);
    match result {
        Ok(None) => {}
        Ok(Some(_)) => panic!("tag-less WAV has no cover art; expected no thumbnail"),
        Err(e) => panic!("tag-less WAV is not a decode failure, got error: {e}"),
    }
    assert!(
        matches!(thumbnail::thumbnail_dimensions(&wav), Ok(None)),
        "tag-less WAV must report no thumbnail dimensions"
    );

    // The general case: every audio format without a cover behaves the same.
    for name in ["test_audio.mp3", "test_audio.flac"] {
        let path = fixtures_dir().join(name);
        match thumbnail::generate_thumbnail(&path, 128, 128) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("fixture {name} has no cover; expected no thumbnail"),
            Err(e) => panic!("fixture {name} without cover must not be a decode error: {e}"),
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_thumbnail_avif() {
    // Requires an ffmpeg build with libsvtav1; skip gracefully otherwise.
    let Ok(enc) = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
    else {
        eprintln!("ffmpeg unavailable; skipping avif thumbnail test");
        return;
    };
    if !String::from_utf8_lossy(&enc.stdout).contains("libsvtav1") {
        eprintln!("libsvtav1 unavailable; skipping avif thumbnail test");
        return;
    }

    let dir = temp_dir("avif_thumb");
    let png_path = dir.join("src.png");
    let avif_path = dir.join("src.avif");
    let img = image::RgbaImage::from_pixel(600, 400, image::Rgba([10, 200, 30, 255]));
    img.save_with_format(&png_path, ImageFormat::Png).unwrap();

    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(&png_path)
        .args([
            "-c:v",
            "libsvtav1",
            "-crf",
            "30",
            "-preset",
            "12",
            "-still-picture",
            "1",
            "-f",
            "avif",
        ])
        .arg(&avif_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("avif encode failed; skipping avif thumbnail test");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
    }

    assert_thumbnail(
        600,
        400,
        thumbnail::generate_thumbnail(&avif_path, 128, 128),
    );
    assert_preview(600, 400, load_preview(&avif_path, 1920, 1440));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_video_frame() {
    use media_sort_backend::media::ffmpeg_pipe;

    if ffmpeg_pipe::find_ffmpeg().is_none() {
        eprintln!("SKIP: ffmpeg not found");
        return;
    }

    let path = mock_state_dir().join("mock 3.mp4");
    let result = ffmpeg_pipe::extract_frame(&path, 128, 128);
    let (w, h, rgba) = result
        .expect("extract_frame should succeed for mock video")
        .into_parts();

    assert!(w > 0 && w <= 128, "width {w} out of range");
    assert!(h > 0 && h <= 128, "height {h} out of range");
    assert_eq!(rgba.len(), (w * h * 4) as usize);

    let expected_ratio = 1280.0 / 720.0;
    let actual_ratio = w as f64 / h as f64;
    assert!(
        (expected_ratio - actual_ratio).abs() < 0.03,
        "aspect ratio not preserved: expected {expected_ratio:.3}, got {actual_ratio:.3}"
    );
}

#[test]
fn test_jpeg_sof_bomb_dimension_cap() {
    // A tiny JPEG whose SOF header claims 65500x65500 used to drive a
    // 268 MB buffer allocation from the turbojpeg path before decompression
    // (per file, in parallel across the grid). The dimension cap must
    // reject it before any allocation.
    let dir = std::env::temp_dir().join(format!("mediasort_jpeg_bomb_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bomb.jpg");

    // Encode a real 1x1 JPEG, then patch the SOF width/height to the
    // turbojpeg maximum (65500).
    let img = image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0]));
    let mut jpeg_bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut jpeg_bytes),
        ImageFormat::Jpeg,
    )
    .unwrap();
    let mut i = 2;
    while i < jpeg_bytes.len() - 1 {
        let marker = jpeg_bytes[i + 1];
        if matches!(marker, 0xC0..=0xC2) {
            // SOF payload: precision(1) height(2) width(2) ...
            let payload = i + 4;
            jpeg_bytes[payload + 1..payload + 3].copy_from_slice(&65500u16.to_be_bytes());
            jpeg_bytes[payload + 3..payload + 5].copy_from_slice(&65500u16.to_be_bytes());
            break;
        }
        let seg_len = u16::from_be_bytes([jpeg_bytes[i + 2], jpeg_bytes[i + 3]]) as usize;
        i += 2 + seg_len;
    }

    std::fs::write(&path, &jpeg_bytes).unwrap();

    let started = std::time::Instant::now();
    let result = thumbnail::generate_thumbnail(&path, 128, 128);
    let elapsed = started.elapsed();

    assert!(result.is_err(), "oversized JPEG must be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("exceed the 16384px limit"),
        "error should name the dimension cap, got: {msg}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "oversized JPEG took {elapsed:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
