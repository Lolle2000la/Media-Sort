mod document;
mod image;
mod pool;
mod video;

use std::path::Path;

use media_sort_core::media_type::MediaType;

pub use pool::KeepPredicate;

pub const THUMBNAIL_SIZE: u32 = 128;

/// Failure of a thumbnail generation request.
///
/// [`Decode`](Self::Decode) is a real file problem (corrupt data, unsupported
/// codec) and gets recorded in `media_errors` as a persistent "failed" badge.
/// [`Transient`](Self::Transient) is a queue/worker-pool problem (queue
/// timeout, dead pool, ffmpeg/mpv request timeout) — the file itself is
/// fine, so it is NOT recorded; the card just stays without a thumbnail and
/// the next `load_visible_thumbnails` pass re-queues it.
#[derive(Debug, Clone)]
pub enum ThumbnailError {
    Decode(String),
    Transient(String),
}

impl ThumbnailError {
    pub(crate) fn transient(message: impl Into<String>) -> Self {
        Self::Transient(message.into())
    }
}

/// Generate a thumbnail with default options (no cancellation predicate).
#[allow(dead_code)]
pub fn generate_thumbnail(
    path: &Path,
    media_type: MediaType,
    animated: Option<bool>,
) -> Result<Option<(u32, u32, Vec<u8>)>, ThumbnailError> {
    generate_thumbnail_with_options(path, media_type, animated, None)
}

/// Generate a thumbnail for `path`.
///
/// Takes the entry's cached `media_type` and `animated` (populated at scan
/// time) plus an optional `should_keep` predicate checked before dispatching
/// and inside worker queues so scrolled-away items cancel without spawning
/// external decoders.
///
/// Returns `Ok(None)` when the request was cancelled or when an audio file has
/// no embedded cover art.
pub fn generate_thumbnail_with_options(
    path: &Path,
    media_type: MediaType,
    _animated: Option<bool>,
    should_keep: Option<KeepPredicate>,
) -> Result<Option<(u32, u32, Vec<u8>)>, ThumbnailError> {
    if should_keep.as_ref().is_some_and(|keep| !keep(path)) {
        return Ok(None);
    }

    if media_type == MediaType::Document {
        return document::generate_document_thumbnail(path, should_keep);
    }

    if media_type == MediaType::Video && !video::is_gif_path(path) {
        return video::generate_video_thumbnail(path, should_keep);
    }

    image::generate_image_thumbnail(path, media_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Encode a single PNG-compressed RGBA image as a minimal `.ico` file
    /// without depending on the `ico` crate in `media-sort-gui`.
    fn write_minimal_png_ico(path: &Path, width: u32, height: u32) {
        let img = ::image::RgbaImage::from_pixel(width, height, ::image::Rgba([0, 0, 0, 255]));
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            ::image::ImageFormat::Png,
        )
        .unwrap();

        let mut ico = Vec::with_capacity(22 + png_bytes.len());
        // ICONDIR header: reserved=0, type=1 (icon), count=1
        ico.extend_from_slice(&0u16.to_le_bytes());
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&1u16.to_le_bytes());
        // ICONDIRENTRY (16 bytes)
        ico.push(if width >= 256 { 0 } else { width as u8 });
        ico.push(if height >= 256 { 0 } else { height as u8 });
        ico.push(0); // color count
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
        ico.extend_from_slice(&(png_bytes.len() as u32).to_le_bytes());
        ico.extend_from_slice(&22u32.to_le_bytes()); // offset
        ico.extend_from_slice(&png_bytes);
        std::fs::write(path, &ico).unwrap();
    }

    #[test]
    fn test_is_gif_path_case_insensitive() {
        assert!(video::is_gif_path(Path::new("/x/a.gif")));
        assert!(video::is_gif_path(Path::new("/x/a.GIF")));
        assert!(video::is_gif_path(Path::new("/x/a.Gif")));
        assert!(!video::is_gif_path(Path::new("/x/a.png")));
        assert!(!video::is_gif_path(Path::new("/x/a.gifv")));
        assert!(!video::is_gif_path(Path::new("/x/agif")));
        assert!(!video::is_gif_path(Path::new("/x/gif")));
    }

    #[test]
    fn test_generate_thumbnail_animated_gif_classified_as_video() {
        let dir = std::env::temp_dir().join("mediasort_test_animated_gif_thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("animated.gif");

        let img1 = ::image::RgbaImage::from_pixel(200, 50, ::image::Rgba([255, 0, 0, 255]));
        let img2 = ::image::RgbaImage::from_pixel(200, 50, ::image::Rgba([0, 255, 0, 255]));
        let file = std::fs::File::create(&path).unwrap();
        {
            let mut encoder = ::image::codecs::gif::GifEncoder::new(file);
            encoder
                .encode(&img1, 200, 50, ::image::ExtendedColorType::Rgba8)
                .unwrap();
            encoder
                .encode(&img2, 200, 50, ::image::ExtendedColorType::Rgba8)
                .unwrap();
        }

        assert_eq!(
            media_sort_backend::media::image_decoder::is_animated_gif(&path),
            Some(true)
        );

        let result = generate_thumbnail(&path, MediaType::Video, Some(true));
        assert!(result.is_ok());
        let (w, h, rgba) = result.unwrap().expect("expected a thumbnail");
        assert_eq!((w, h), (128, 32));
        assert_eq!(rgba.len(), (128 * 32 * 4) as usize);
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_thumbnail_valid_image() {
        let dir = std::env::temp_dir().join("mediasort_test_thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.png");

        let img = ::image::RgbaImage::from_pixel(32, 32, ::image::Rgba([255, 0, 0, 255]));
        img.save(&path).unwrap();

        let result = generate_thumbnail(&path, MediaType::Image, None);
        assert!(result.is_ok());
        let (w, h, rgba) = result.unwrap().expect("expected a thumbnail");
        assert!(w > 0 && h > 0);
        assert!(!rgba.is_empty());
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_thumbnail_ico() {
        let dir = std::env::temp_dir().join("mediasort_test_ico");
        std::fs::create_dir_all(&dir).unwrap();

        for name in ["test.ico", "TEST.ICO"] {
            let path = dir.join(name);
            write_minimal_png_ico(&path, 32, 32);

            let result = generate_thumbnail(&path, MediaType::Image, None);
            assert!(result.is_ok(), "failed for {name}: {:?}", result.err());
            let (w, h, rgba) = result.unwrap().expect("expected a thumbnail");
            assert_eq!((w, h), (32, 32));
            assert_eq!(rgba.len(), (32 * 32 * 4) as usize);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_thumbnail_nonexistent() {
        let result = generate_thumbnail(
            Path::new("/nonexistent/image_xyz.jpg"),
            MediaType::Image,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_thumbnail_exif_orientation() {
        let dir = std::env::temp_dir().join("mediasort_test_exif_thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jpg");

        let img = ::image::RgbImage::from_pixel(100, 50, ::image::Rgb([255, 0, 0]));
        let mut jpeg_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
        img.write_to(&mut cursor, ::image::ImageFormat::Jpeg)
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
        oriented_jpeg.extend_from_slice(&jpeg_bytes[..2]);
        oriented_jpeg.extend_from_slice(exif_app1);
        oriented_jpeg.extend_from_slice(&jpeg_bytes[2..]);

        std::fs::write(&path, &oriented_jpeg).unwrap();

        let result = generate_thumbnail(&path, MediaType::Image, None);
        assert!(result.is_ok());
        let (w, h, rgba) = result.unwrap().expect("expected a thumbnail");
        assert!(
            h > w,
            "Expected thumbnail height {h} > width {w} due to EXIF orientation 6 (90 deg CW)"
        );
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        std::fs::remove_dir_all(&dir).ok();
    }

    fn write_silent_wav(path: &Path) {
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
        buf.resize(44 + data_len as usize, 128u8); // silence
        std::fs::write(path, &buf).unwrap();
    }

    #[test]
    fn test_generate_thumbnail_audio_without_cover_is_not_a_failure() {
        let dir = std::env::temp_dir().join("mediasort_test_audio_no_cover");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("silent.wav");
        write_silent_wav(&path);

        match generate_thumbnail(&path, MediaType::Audio, None) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("tag-less WAV has no cover art; expected no thumbnail"),
            Err(e) => panic!("tag-less WAV must not be a thumbnail failure: {e:?}"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_thumbnail_with_options_cancelled_by_keep_predicate() {
        let keep: KeepPredicate = Arc::new(|_| false);
        let result = generate_thumbnail_with_options(
            Path::new("/nonexistent/should_not_decode.jpg"),
            MediaType::Image,
            None,
            Some(keep),
        );
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_generate_thumbnail_text_document_returns_ok_none() {
        let dir = std::env::temp_dir().join("mediasort_test_doc_no_thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, b"hello document").unwrap();

        match generate_thumbnail(&path, MediaType::Document, None) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("plain text has no thumbnail; expected Ok(None)"),
            Err(e) => panic!("plain text must not fail thumbnail generation: {e:?}"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_thumbnail_with_options_document_cancelled_returns_ok_none() {
        let path = std::path::PathBuf::from("/nonexistent/notes.txt");
        let keep: KeepPredicate = Arc::new(|_| false);
        let result = generate_thumbnail_with_options(&path, MediaType::Document, None, Some(keep));
        assert!(matches!(result, Ok(None)));
    }
}
