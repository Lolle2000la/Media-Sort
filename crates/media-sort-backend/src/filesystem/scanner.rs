use std::path::{Path, PathBuf};
use std::sync::mpsc;

use media_sort_core::media_type::{MediaRegistry, MediaType, SYSTEM_REGISTRY};
use media_sort_core::models::MediaEntry;
use walkdir::WalkDir;

/// Classify a media path into `(MediaType, animated_gif)` using the baseline
/// extension lists first, then [`MediaRegistry::determine_type`] for
/// mpv-discovered video formats, falling back to `MediaType::Image` for
/// unrecognized extensions.
///
/// For `.gif` files, [`crate::media::image_decoder::is_animated_gif`] is
/// queried once: static GIFs and GIFs when `animate_gifs` is `false` are
/// classified as [`MediaType::Image`], while animated (or unverified) GIFs
/// with `animate_gifs == true` are classified as [`MediaType::Video`].
pub fn classify_media(path: &Path, animate_gifs: bool) -> (MediaType, Option<bool>) {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let media_type = [
        MediaType::Image,
        MediaType::Audio,
        MediaType::Document,
        MediaType::Video,
    ]
    .into_iter()
    .find(|ty| ty.extensions().contains(&ext.as_str()))
    .or_else(|| MediaRegistry::determine_type(&ext))
    .unwrap_or(MediaType::Image);

    if media_type == MediaType::Video && ext == "gif" {
        let animated = crate::media::image_decoder::is_animated_gif(path);
        let effective = if !animate_gifs || animated == Some(false) {
            MediaType::Image
        } else {
            MediaType::Video
        };
        return (effective, animated);
    }

    (media_type, None)
}

fn spawn_media_scan<T: Send + 'static>(
    dir: &Path,
    map_entry: impl Fn(PathBuf) -> T + Send + 'static,
) -> mpsc::Receiver<T> {
    let dir = dir.to_path_buf();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let allowed = SYSTEM_REGISTRY
            .get()
            .map(|r| r.allowed_extensions.clone())
            .unwrap_or_else(MediaRegistry::fallback_allowed_extensions);

        for entry in WalkDir::new(&dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|ext| allowed.contains(&ext.to_lowercase()))
            })
        {
            if tx.send(map_entry(entry.into_path())).is_err() {
                break;
            }
        }
    });

    rx
}

/// Scan `dir` (non-recursively) and stream matching media file paths.
pub fn scan_media_files(dir: &Path) -> mpsc::Receiver<PathBuf> {
    spawn_media_scan(dir, |path| path)
}

/// Scan `dir` (non-recursively) and stream classified [`MediaEntry`] items,
/// populating `animated` in a single GIF header pass per `.gif` file.
pub fn scan_media_entries(dir: &Path, animate_gifs: bool) -> mpsc::Receiver<MediaEntry> {
    spawn_media_scan(dir, move |path| {
        let (media_type, animated) = classify_media(&path, animate_gifs);
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        MediaEntry {
            path,
            media_type,
            file_name,
            animated,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_gif(path: &Path, frame_count: usize) {
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        for i in 0..frame_count {
            let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([i as u8 * 40, 0, 0, 255]));
            encoder.encode_frame(image::Frame::new(img)).unwrap();
        }
    }

    #[test]
    fn test_classify_media_images_and_svg() {
        for p in [
            "test.jpg",
            "test.png",
            "test.jpeg",
            "test.bmp",
            "vector.svg",
            "icon.SVGZ",
        ] {
            assert_eq!(classify_media(Path::new(p), true), (MediaType::Image, None));
        }
    }

    #[test]
    fn test_classify_media_video_and_audio() {
        for p in ["test.mp4", "test.mkv", "test.webm", "test.mov"] {
            assert_eq!(classify_media(Path::new(p), true), (MediaType::Video, None));
        }
        for p in ["test.mp3", "test.flac", "test.wav", "test.ogg"] {
            assert_eq!(classify_media(Path::new(p), true), (MediaType::Audio, None));
        }
    }

    #[test]
    fn test_classify_media_documents() {
        for p in [
            "readme.md",
            "guide.MARKDOWN",
            "notes.txt",
            "data.csv",
            "table.tsv",
        ] {
            assert_eq!(
                classify_media(Path::new(p), true),
                (MediaType::Document, None)
            );
        }
    }

    #[test]
    fn test_classify_media_unknown_fallback() {
        for p in ["test.xyz", "test", "test.doc", "test."] {
            assert_eq!(classify_media(Path::new(p), true), (MediaType::Image, None));
        }
    }

    #[test]
    fn test_classify_media_case_insensitive() {
        assert_eq!(
            classify_media(Path::new("test.JPG"), true),
            (MediaType::Image, None)
        );
        assert_eq!(
            classify_media(Path::new("test.MP3"), true),
            (MediaType::Audio, None)
        );
        assert_eq!(
            classify_media(Path::new("test.Mp4"), true),
            (MediaType::Video, None)
        );
    }

    #[test]
    fn test_classify_media_gifs() {
        let dir =
            std::env::temp_dir().join(format!("mediasort_classify_gif_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let static_gif = dir.join("static.gif");
        write_gif(&static_gif, 1);
        assert_eq!(
            classify_media(&static_gif, true),
            (MediaType::Image, Some(false))
        );
        assert_eq!(
            classify_media(&static_gif, false),
            (MediaType::Image, Some(false))
        );

        let anim_gif = dir.join("animated.gif");
        write_gif(&anim_gif, 2);
        assert_eq!(
            classify_media(&anim_gif, true),
            (MediaType::Video, Some(true))
        );
        assert_eq!(
            classify_media(&anim_gif, false),
            (MediaType::Image, Some(true))
        );

        // Non-existent gif falls back based on `animate_gifs` with `animated = None`.
        assert_eq!(
            classify_media(Path::new("missing.gif"), true),
            (MediaType::Video, None)
        );
        assert_eq!(
            classify_media(Path::new("missing.gif"), false),
            (MediaType::Image, None)
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_scan_media_entries_populates_classification_and_animated() {
        let dir =
            std::env::temp_dir().join(format!("mediasort_scan_entries_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let img = image::RgbImage::from_pixel(4, 4, image::Rgb([10, 20, 30]));
        img.save(dir.join("photo.jpg")).unwrap();
        write_gif(&dir.join("anim.gif"), 2);
        std::fs::write(dir.join("notes.bin"), b"ignore").unwrap();

        let entries: Vec<_> = scan_media_entries(&dir, true).into_iter().collect();
        assert_eq!(entries.len(), 2);

        let jpg = entries.iter().find(|e| e.file_name == "photo.jpg").unwrap();
        assert_eq!(jpg.media_type, MediaType::Image);
        assert_eq!(jpg.animated, None);

        let gif = entries.iter().find(|e| e.file_name == "anim.gif").unwrap();
        assert_eq!(gif.media_type, MediaType::Video);
        assert_eq!(gif.animated, Some(true));

        std::fs::remove_dir_all(&dir).ok();
    }
}
