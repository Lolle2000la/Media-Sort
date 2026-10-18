use std::path::{Path, PathBuf};

use id3::TagLike;
use image::GenericImageView;
use media_sort_backend::filesystem::scanner;
use media_sort_backend::media::image_decoder;
use media_sort_backend::media::thumbnail;
use media_sort_backend::metadata::audio_meta::extract_audio_metadata;
use media_sort_backend::metadata::image_meta::extract_image_metadata;
use media_sort_backend::metadata::video_meta::extract_video_metadata;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn test_extract_exif_from_jpeg() {
    let path = fixtures_dir().join("test_image.jpg");
    let result = extract_image_metadata(&path);
    assert!(result.is_ok());
}

#[test]
fn test_extract_from_png() {
    let path = fixtures_dir().join("test_image.png");
    let result = extract_image_metadata(&path);
    assert!(result.is_ok());
}

#[test]
fn test_file_not_found() {
    let path = Path::new("/nonexistent/path/to/file.jpg");
    let result = extract_image_metadata(path);
    assert!(result.is_err());
}

#[test]
fn test_load_jpeg() {
    let path = fixtures_dir().join("test_image.jpg");
    let img = image_decoder::load_image(&path).unwrap();
    assert_eq!(img.dimensions(), (64, 64));
}

#[test]
fn test_load_png() {
    let path = fixtures_dir().join("test_image.png");
    let img = image_decoder::load_image(&path).unwrap();
    assert_eq!(img.dimensions(), (32, 32));
}

#[test]
fn test_decode_dimensions() {
    let path = fixtures_dir().join("test_image.jpg");
    let dims = image_decoder::decode_image_dimensions(&path).unwrap();
    assert_eq!(dims, (64, 64));
}

#[test]
fn test_generate_thumbnail() {
    let path = fixtures_dir().join("test_image.jpg");
    let thumb = image_decoder::generate_thumbnail(&path, 32, 32).unwrap();
    let (w, h) = thumb.dimensions();
    assert!(w <= 32, "width {w} exceeds max 32");
    assert!(h <= 32, "height {h} exceeds max 32");
}

#[test]
fn test_load_file_not_found() {
    let path = Path::new("/nonexistent/image.png");
    let result = image_decoder::load_image(path);
    assert!(result.is_err());
}

#[test]
fn test_thumbnail_dimensions() {
    let path = fixtures_dir().join("test_image.jpg");
    let dims = thumbnail::thumbnail_dimensions(&path)
        .unwrap()
        .expect("expected thumbnail dimensions");
    assert_eq!(dims, (64, 64));

    let (w, h, _rgba) = thumbnail::generate_thumbnail(&path, 32, 32)
        .unwrap()
        .expect("expected a thumbnail image")
        .into_parts();
    assert!(w <= 32, "thumbnail width {w} should be clamped to max 32");
    assert!(h <= 32, "thumbnail height {h} should be clamped to max 32");
    assert!(
        w == 32 || h == 32,
        "at least one dimension must hit max (64x64 source should scale to 32x32), got {w}x{h}"
    );
}

#[test]
fn test_thumbnail_generation() {
    let path = fixtures_dir().join("test_image.jpg");
    let (w, h, rgba) = thumbnail::generate_thumbnail(&path, 32, 32)
        .unwrap()
        .expect("expected a thumbnail image")
        .into_parts();
    assert!(w > 0 && h > 0);
    assert!(w <= 32, "width {w} should be clamped to max 32");
    assert!(h <= 32, "height {h} should be clamped to max 32");
    assert!(
        w == 32 || h == 32,
        "at least one dimension must hit max (64x64 source should scale to 32x32), got {w}x{h}"
    );
    assert!(!rgba.is_empty());
    assert_eq!(rgba.len(), (w * h * 4) as usize);
}

#[test]
fn test_thumbnail_respects_max() {
    let path = fixtures_dir().join("test_image.jpg");
    let (w, h, _rgba) = thumbnail::generate_thumbnail(&path, 16, 16)
        .unwrap()
        .expect("expected a thumbnail image")
        .into_parts();
    assert!(w <= 16, "width {w} exceeds max 16");
    assert!(h <= 16, "height {h} exceeds max 16");
}

#[test]
fn test_thumbnail_aspect_ratio() {
    let img = image::RgbImage::from_pixel(100, 50, image::Rgb([255, 0, 0]));
    let tmp_path = std::env::temp_dir().join("test_thumbnail_aspect_ratio.jpg");
    img.save_with_format(&tmp_path, image::ImageFormat::Jpeg)
        .unwrap();

    let (w, h, _rgba) = thumbnail::generate_thumbnail(&tmp_path, 20, 20)
        .unwrap()
        .expect("expected a thumbnail image")
        .into_parts();
    assert!(w <= 20 && h <= 20);
    assert!(
        w == 20 || h == 20,
        "Expected one dimension to hit max, got {w}x{h}"
    );

    std::fs::remove_file(&tmp_path).ok();
}

#[test]
fn test_extract_mp3_metadata() {
    let path = fixtures_dir().join("test_audio.mp3");
    let result = extract_audio_metadata(&path);
    assert!(result.is_ok());
}

#[test]
fn test_extract_flac_metadata() {
    let path = fixtures_dir().join("test_audio.flac");
    let result = extract_audio_metadata(&path);
    assert!(result.is_ok());
}

#[test]
fn test_extract_video_from_non_video() {
    let path = fixtures_dir().join("test_image.jpg");
    let result = extract_video_metadata(&path);
    assert!(result.is_ok());
}

// ============================================================
// Additional tests from audit
// ============================================================

#[test]
fn test_extract_audio_metadata_unknown_extension() {
    let dir = std::env::temp_dir().join("mediasort_audio_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.xyz");
    std::fs::write(&path, b"not audio data").unwrap();
    let result = extract_audio_metadata(&path);
    assert!(result.is_ok());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_audio_metadata_nonexistent() {
    let result = extract_audio_metadata(Path::new("/nonexistent/audio_xyz.mp3"));
    assert!(result.is_err());
}

#[test]
fn test_extract_video_metadata_nonexistent() {
    let result = extract_video_metadata(Path::new("/nonexistent/video_xyz.mp4"));
    assert!(result.is_err());
}

// ============================================================
// Scanner edge cases
// ============================================================

#[test]
fn test_scan_nonexistent_directory() {
    let results: Vec<_> = scanner::scan_media_files(Path::new("/nonexistent/dir_12345_xyz"))
        .into_iter()
        .collect();
    assert!(results.is_empty());
}

// ============================================================
// Image decoder error paths
// ============================================================

#[test]
fn test_decode_dimensions_nonexistent() {
    let result = image_decoder::decode_image_dimensions(Path::new("/nonexistent/img.jpg"));
    assert!(result.is_err());
}

#[test]
fn test_image_generate_thumbnail_nonexistent() {
    let result = image_decoder::generate_thumbnail(Path::new("/nonexistent/img.jpg"), 32, 32);
    assert!(result.is_err());
}

// ============================================================
// Thumbnail error paths
// ============================================================

#[test]
fn test_thumbnail_generate_nonexistent() {
    let result = thumbnail::generate_thumbnail(Path::new("/nonexistent/img.jpg"), 32, 32);
    assert!(result.is_err());
}

#[test]
fn test_thumbnail_dimensions_nonexistent() {
    let result = thumbnail::thumbnail_dimensions(Path::new("/nonexistent/img.jpg"));
    assert!(result.is_err());
}

// ============================================================
// Trash staging: stage_file with missing source
// ============================================================

#[test]
fn test_trash_stage_nonexistent_file() {
    let result = media_sort_backend::filesystem::trash::delete_to_trash(Path::new(
        "/nonexistent/file_xyz.txt",
    ));
    assert!(result.is_err());
}

// ============================================================
// Audio metadata extension routing
// ============================================================

#[test]
fn test_extract_audio_metadata_wav() {
    let dir = std::env::temp_dir().join(format!("mediasort_audio_wav_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.wav");
    std::fs::write(&path, b"").unwrap();
    let mut tag = id3::Tag::new();
    tag.set_title("Test Title");
    tag.write_to_path(&path, id3::Version::Id3v24).unwrap();
    let result = extract_audio_metadata(&path);
    assert!(result.is_ok());
    assert!(
        result
            .as_ref()
            .unwrap()
            .get("ID3 Metadata")
            .and_then(|s| s.get("Title"))
            .is_some_and(|t| t == "Test Title")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_audio_metadata_aiff() {
    let dir = std::env::temp_dir().join(format!("mediasort_audio_aiff_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.aiff");
    std::fs::write(&path, b"").unwrap();
    let mut tag = id3::Tag::new();
    tag.set_title("AIFF Title");
    tag.write_to_path(&path, id3::Version::Id3v24).unwrap();
    let result = extract_audio_metadata(&path);
    assert!(result.is_ok());
    assert!(
        result
            .as_ref()
            .unwrap()
            .get("ID3 Metadata")
            .and_then(|s| s.get("Title"))
            .is_some_and(|t| t == "AIFF Title")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_audio_metadata_ogg() {
    let dir = std::env::temp_dir().join(format!("mediasort_audio_ogg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.ogg");
    std::fs::write(&path, b"OggS....").unwrap();
    let result = extract_audio_metadata(&path);
    assert!(result.is_ok());
    assert!(result.unwrap().contains_key("File"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_audio_metadata_opus() {
    let dir = std::env::temp_dir().join(format!("mediasort_audio_opus_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.opus");
    std::fs::write(&path, b"OggS....OpusHead").unwrap();
    let result = extract_audio_metadata(&path);
    assert!(result.is_ok());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_audio_metadata_m4a() {
    let dir = std::env::temp_dir().join(format!("mediasort_audio_m4a_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.m4a");
    let data = minimal_mp4_container();
    std::fs::write(&path, &data).unwrap();
    let result = extract_audio_metadata(&path);
    assert!(result.is_ok(), "m4a metadata extraction should succeed");
    assert!(result.unwrap().contains_key("File"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_audio_metadata_aac() {
    let dir = std::env::temp_dir().join(format!("mediasort_audio_aac_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.aac");
    std::fs::write(&path, b"not real aac data").unwrap();
    let result = extract_audio_metadata(&path);
    assert!(result.is_ok());
    assert!(result.unwrap().contains_key("File"));
    std::fs::remove_dir_all(&dir).ok();
}

// ============================================================
// Video metadata routing
// ============================================================

#[test]
fn test_extract_video_metadata_mp4() {
    let dir = std::env::temp_dir().join(format!("mediasort_video_mp4_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.mp4");
    let data = minimal_mp4_container();
    std::fs::write(&path, &data).unwrap();
    let result = extract_video_metadata(&path);
    assert!(
        result.is_ok(),
        "mp4 video metadata extraction should succeed"
    );
    assert!(result.unwrap().contains_key("File"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_video_metadata_m4v() {
    let dir = std::env::temp_dir().join(format!("mediasort_video_m4v_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.m4v");
    let data = minimal_mp4_container();
    std::fs::write(&path, &data).unwrap();
    let result = extract_video_metadata(&path);
    assert!(
        result.is_ok(),
        "m4v video metadata extraction should succeed"
    );
    assert!(result.unwrap().contains_key("File"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_video_metadata_mov() {
    let dir = std::env::temp_dir().join(format!("mediasort_video_mov_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.mov");
    let data = minimal_mp4_container();
    std::fs::write(&path, &data).unwrap();
    let result = extract_video_metadata(&path);
    assert!(
        result.is_ok(),
        "mov video metadata extraction should succeed"
    );
    assert!(result.unwrap().contains_key("File"));
    std::fs::remove_dir_all(&dir).ok();
}

fn minimal_mp4_container() -> Vec<u8> {
    let mut out = Vec::new();

    // ftyp box: 24 bytes
    out.extend_from_slice(&24u32.to_be_bytes());
    out.extend_from_slice(b"ftyp");
    out.extend_from_slice(b"isom");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(b"isom");

    // moov box
    let moov_start = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // placeholder
    out.extend_from_slice(b"moov");

    // udta box inside moov
    let udta_start = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // placeholder
    out.extend_from_slice(b"udta");

    // meta box (full box) inside udta
    let meta_start = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // placeholder
    out.extend_from_slice(b"meta");
    out.extend_from_slice(&0u32.to_be_bytes()); // version=0, flags=0

    // hdlr box (full box) inside meta
    let hdlr_start = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // placeholder
    out.extend_from_slice(b"hdlr");
    out.extend_from_slice(&0u32.to_be_bytes()); // version=0, flags=0
    out.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    out.extend_from_slice(b"mdir"); // handler_type
    out.extend_from_slice(&[0u8; 12]); // reserved
    out.push(b'a'); // name (null-terminated)
    out.push(0);
    let hdlr_end = out.len();
    let hdlr_size = (hdlr_end - hdlr_start) as u32;
    out[hdlr_start..hdlr_start + 4].copy_from_slice(&hdlr_size.to_be_bytes());

    // ilst box inside meta — pre-populated with a dummy \xa9day atom so
    // mp4ameta can find at least one metadata entry.
    let ilst_start = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // placeholder for ilst size
    out.extend_from_slice(b"ilst");

    // \xa9day atom
    let day_start = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // placeholder for \xa9day size
    out.extend_from_slice(b"\xa9day");
    // data child (FullBox data atom)
    let data_start = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // placeholder for data size
    out.extend_from_slice(b"data");
    out.extend_from_slice(&[0u8; 8]); // reserved(4) + type_indicator(4)
    let data_end = out.len();
    let data_size = (data_end - data_start) as u32;
    out[data_start..data_start + 4].copy_from_slice(&data_size.to_be_bytes());

    let day_end = out.len();
    let day_size = (day_end - day_start) as u32;
    out[day_start..day_start + 4].copy_from_slice(&day_size.to_be_bytes());

    let ilst_end = out.len();
    let ilst_size = (ilst_end - ilst_start) as u32;
    out[ilst_start..ilst_start + 4].copy_from_slice(&ilst_size.to_be_bytes());

    // backpatch meta
    let meta_end = out.len();
    let meta_size = (meta_end - meta_start) as u32;
    out[meta_start..meta_start + 4].copy_from_slice(&meta_size.to_be_bytes());

    // backpatch udta
    let udta_end = out.len();
    let udta_size = (udta_end - udta_start) as u32;
    out[udta_start..udta_start + 4].copy_from_slice(&udta_size.to_be_bytes());

    // backpatch moov
    let moov_end = out.len();
    let moov_size = (moov_end - moov_start) as u32;
    out[moov_start..moov_start + 4].copy_from_slice(&moov_size.to_be_bytes());

    out
}

#[test]
fn test_extract_video_metadata_mkv() {
    let dir = std::env::temp_dir().join(format!("mediasort_video_mkv_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.mkv");
    std::fs::write(&path, b"\x1a\x45\xdf\xa3....").unwrap();
    let result = extract_video_metadata(&path);
    assert!(result.is_ok());
    assert!(result.unwrap().contains_key("File"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_extract_video_metadata_webm() {
    let dir = std::env::temp_dir().join(format!("mediasort_video_webm_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.webm");
    std::fs::write(&path, b"\x1a\x45\xdf\xa3....webm").unwrap();
    let result = extract_video_metadata(&path);
    assert!(result.is_ok());
    std::fs::remove_dir_all(&dir).ok();
}

// ============================================================
// Thumbnail aspect ratio edge cases
// ============================================================

#[test]
fn test_thumbnail_extreme_landscape() {
    let img = image::RgbImage::from_pixel(1000, 10, image::Rgb([255, 0, 0]));
    let tmp_path =
        std::env::temp_dir().join(format!("test_thumb_landscape_{}.jpg", std::process::id()));
    img.save_with_format(&tmp_path, image::ImageFormat::Jpeg)
        .unwrap();

    let (w, h, _rgba) = thumbnail::generate_thumbnail(&tmp_path, 100, 100)
        .unwrap()
        .expect("expected a thumbnail image")
        .into_parts();
    assert_eq!(w, 100, "extreme landscape width should be 100");
    assert_eq!(h, 1, "extreme landscape height should be 1");

    std::fs::remove_file(&tmp_path).ok();
}

#[test]
fn test_thumbnail_extreme_portrait() {
    let img = image::RgbImage::from_pixel(10, 1000, image::Rgb([255, 0, 0]));
    let tmp_path =
        std::env::temp_dir().join(format!("test_thumb_portrait_{}.jpg", std::process::id()));
    img.save_with_format(&tmp_path, image::ImageFormat::Jpeg)
        .unwrap();

    let (w, h, _rgba) = thumbnail::generate_thumbnail(&tmp_path, 100, 100)
        .unwrap()
        .expect("expected a thumbnail image")
        .into_parts();
    assert_eq!(w, 1, "extreme portrait width should be 1");
    assert_eq!(h, 100, "extreme portrait height should be 100");

    std::fs::remove_file(&tmp_path).ok();
}

// ============================================================
// is_animated_gif tests
// ============================================================

#[test]
fn test_is_animated_gif_non_gif_file_returns_none() {
    let path = fixtures_dir().join("test_image.jpg");
    let result = image_decoder::is_animated_gif(&path);
    assert_eq!(result, None);
}

#[test]
fn test_is_animated_gif_nonexistent_returns_none() {
    let result = image_decoder::is_animated_gif(Path::new("/nonexistent/file_xyz.gif"));
    assert_eq!(result, None);
}

#[test]
fn test_is_animated_gif_static_returns_false() {
    let dir = std::env::temp_dir().join(format!("mediasort_gif_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("static_test.gif");

    let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
    let file = std::fs::File::create(&path).unwrap();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        encoder
            .encode(&img, 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
    }

    let result = image_decoder::is_animated_gif(&path);
    assert_eq!(
        result,
        Some(false),
        "single-frame GIF should not be animated"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_is_animated_gif_multi_frame_returns_true() {
    let dir = std::env::temp_dir().join(format!("mediasort_gif_test2_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("animated_test.gif");

    let img1 = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
    let img2 = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 255, 0, 255]));
    let file = std::fs::File::create(&path).unwrap();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        encoder
            .encode(&img1, 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
        encoder
            .encode(&img2, 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
    }

    let result = image_decoder::is_animated_gif(&path);
    assert_eq!(result, Some(true), "two-frame GIF should be animated");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_is_animated_gif_fixture_does_not_panic() {
    let path = fixtures_dir().join("test_image.gif");
    let result = image_decoder::is_animated_gif(&path);
    assert!(result.is_some(), "fixture GIF should be identified as GIF");
}

// ============================================================
// Content assertion tests for metadata extraction
// ============================================================

#[test]
fn test_image_metadata_content_jpeg() {
    let fixture = fixtures_dir().join("test_image.jpg");
    let result = extract_image_metadata(&fixture);
    assert!(result.is_ok(), "should extract metadata from JPEG");
    let metadata = result.unwrap();
    assert!(metadata.contains_key("File"));
    let file_section = metadata.get("File").unwrap();
    assert!(file_section.contains_key("Name"));
    assert!(file_section.contains_key("Size"));
    assert!(!file_section.get("Name").unwrap().is_empty());
}

#[test]
fn test_image_metadata_content_png() {
    let fixture = fixtures_dir().join("test_image.png");
    let result = extract_image_metadata(&fixture);
    assert!(result.is_ok(), "should extract metadata from PNG");
    let metadata = result.unwrap();
    assert!(metadata.contains_key("File"));
}

#[test]
fn test_audio_metadata_content_mp3() {
    let fixture = fixtures_dir().join("test_audio.mp3");
    let result = extract_audio_metadata(&fixture);
    assert!(result.is_ok(), "should extract metadata from MP3");
    let metadata = result.unwrap();
    assert!(metadata.contains_key("File"));
    let file_section = metadata.get("File").unwrap();
    assert!(!file_section.get("Name").unwrap().is_empty());
}

#[test]
fn test_audio_metadata_content_flac() {
    let fixture = fixtures_dir().join("test_audio.flac");
    let result = extract_audio_metadata(&fixture);
    assert!(result.is_ok(), "should extract metadata from FLAC");
    let metadata = result.unwrap();
    assert!(metadata.contains_key("File"));
}

#[test]
fn test_image_metadata_nonexistent_file() {
    let result = extract_image_metadata(Path::new("/nonexistent/image_meta_test_12345.jpg"));
    assert!(result.is_err());
}

#[test]
fn test_audio_metadata_nonexistent_file() {
    let result = extract_audio_metadata(Path::new("/nonexistent/audio_meta_test_12345.mp3"));
    assert!(result.is_err());
}

#[test]
fn test_thumbnail_exif_orientation() {
    let img = image::RgbImage::from_pixel(100, 50, image::Rgb([255, 0, 0]));
    let mut jpeg_bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
    img.write_to(&mut cursor, image::ImageFormat::Jpeg).unwrap();

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

    let tmp_path =
        std::env::temp_dir().join(format!("test_exif_orient_{}.jpg", std::process::id()));
    std::fs::write(&tmp_path, &oriented_jpeg).unwrap();

    let (w, h, _rgba) = thumbnail::generate_thumbnail(&tmp_path, 128, 128)
        .unwrap()
        .expect("expected a thumbnail image")
        .into_parts();
    assert!(
        h > w,
        "Expected height {h} to be greater than width {w} due to EXIF orientation 6 (90 deg rotation)"
    );

    let dims = image_decoder::decode_image_dimensions(&tmp_path).unwrap();
    assert_eq!(
        dims,
        (50, 100),
        "Decoded dimensions should be 50x100 after EXIF orientation"
    );

    std::fs::remove_file(&tmp_path).ok();
}

#[test]
fn test_is_animated_gif_bomb_header_returns_fast() {
    // 15-byte GIF claiming a 65535x65535 logical screen: the old
    // decode-based check attempted a ~16 GiB allocation on the first frame
    // read (GifDecoder uses Limits::no_limits()). The header scan must
    // answer without allocating anything.
    let dir = std::env::temp_dir().join(format!("mediasort_gif_bomb_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bomb.gif");

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"GIF89a");
    // Logical screen descriptor: 65535x65535, no global color table.
    bytes.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00]);
    bytes.extend_from_slice(&[0x00, 0x00]);
    std::fs::write(&path, &bytes).unwrap();

    let started = std::time::Instant::now();
    let result = image_decoder::is_animated_gif(&path);
    let elapsed = started.elapsed();

    // No trailer and no image descriptors: the file is not a parseable
    // GIF, so the answer is None (unknown), never Some(false) — a corrupt
    // GIF must not be misclassified as a known-static one.
    assert_eq!(result, None, "bomb has no trailer or image descriptors");
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "header scan took {elapsed:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_is_animated_gif_truncated_returns_none() {
    // A single-frame GIF whose data is cut off before the trailer. The
    // pre-fix code (v3.0) counted truncated/corrupt items as frames via
    // GifFrameIterator, so a file like this was misclassified as ANIMATED
    // (`Some(true)`). The header scan must answer None (unknown) instead —
    // a truncated GIF is not a parseable GIF and must never be classified
    // as animated.
    let dir = std::env::temp_dir().join(format!("mediasort_gif_trunc_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("truncated.gif");

    let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
    let file = std::fs::File::create(&path).unwrap();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        encoder
            .encode(&img, 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
    }
    let mut bytes = std::fs::read(&path).unwrap();
    // Drop the trailer byte (and any trailing bytes) to simulate truncation.
    while bytes.last() == Some(&0x3B) {
        bytes.pop();
    }
    std::fs::write(&path, &bytes).unwrap();

    let result = image_decoder::is_animated_gif(&path);
    assert_eq!(
        result, None,
        "truncated GIF (no trailer) must not be classified as static"
    );

    std::fs::remove_dir_all(&dir).ok();
}
