use std::collections::HashSet;
use std::sync::OnceLock;

use strum::{EnumIter, IntoEnumIterator};

fn native_image_extensions() -> &'static [&'static str] {
    static EXTS: OnceLock<Vec<&'static str>> = OnceLock::new();
    EXTS.get_or_init(|| {
        let mut exts: Vec<&'static str> = image::ImageFormat::all()
            .filter(|f| f.can_read() && !matches!(f, image::ImageFormat::Gif))
            .flat_map(|f| f.extensions_str().iter().copied())
            .collect();
        // AVIF is always treated as a native image. On the shipped targets the
        // `avif-native` (dav1d) image feature already puts `avif` in `exts`, so
        // this is normally a no-op; keep the explicit entry so AVIF stays
        // visible to the scanner/classifier even in a build without a native
        // decoder (the backend can still decode it via the ffmpeg pipe fallback).
        // SVG and SVGZ are rendered via pure-Rust `resvg` in the backend.
        for extra in ["avif", "svg", "svgz"] {
            if !exts.contains(&extra) {
                exts.push(extra);
            }
        }
        exts
    })
}

pub static NATIVE_AUDIO_EXTS: &[&str] = &[
    "mp3", "flac", "ogg", "wav", "aac", "m4a", "wma", "opus", "aiff",
];

pub static NATIVE_DOCUMENT_EXTS: &[&str] = &["pdf", "md", "markdown", "txt", "csv", "tsv"];

pub static SYSTEM_REGISTRY: OnceLock<MediaRegistry> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter)]
pub enum MediaType {
    Image,
    Video,
    Audio,
    Document,
}

/// High-level document family used for UI icons, metadata labels, and preview dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocumentKind {
    Pdf,
    Markdown,
    PlainText,
}

impl DocumentKind {
    /// Classifies a lowercase or mixed-case file extension (without leading dot).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "pdf" => Some(Self::Pdf),
            "md" | "markdown" => Some(Self::Markdown),
            "txt" | "csv" | "tsv" => Some(Self::PlainText),
            _ => None,
        }
    }

    /// Classifies a filesystem path by its extension.
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Self::from_extension)
    }
}

impl MediaType {
    /// Returns the **compile-time baseline** extensions for this media type.
    ///
    /// For images, audio, and documents, this is the definitive list — those
    /// formats do not depend on any external video runtime.
    ///
    /// For video, the returned list is a **hardcoded fallback subset**. The
    /// actual set of playable video formats depends on the libmpv build
    /// installed on the user's system and is discovered at startup via
    /// [`MediaRegistry::init`]. Use [`MediaRegistry::determine_type`] for
    /// the authoritative type check that accounts for mpv-discovered formats.
    /// This method's video list exists so that:
    ///
    /// - [`MediaRegistry::fallback_allowed_extensions`] can provide sensible
    ///   defaults when mpv initialization fails.
    /// - The GUI's `detect_media_type` has a fast first-pass match before
    ///   falling back to the registry.
    ///
    /// In short: this is the "what we know without mpv" list, not the "what
    /// we support" list.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            MediaType::Image => native_image_extensions(),
            MediaType::Video => &[
                "mp4", "mkv", "webm", "avi", "mov", "wmv", "flv", "m4v", "gif",
            ],
            MediaType::Audio => NATIVE_AUDIO_EXTS,
            MediaType::Document => NATIVE_DOCUMENT_EXTS,
        }
    }

    /// Iterates over all [`MediaType`] variants in declaration order.
    pub fn iter() -> MediaTypeIter {
        <Self as IntoEnumIterator>::iter()
    }

    pub fn all_extensions() -> Vec<&'static str> {
        Self::iter()
            .flat_map(|ty| ty.extensions().iter().copied())
            .collect()
    }
}

pub struct MediaRegistry {
    pub allowed_extensions: HashSet<String>,
    pub mpv_extensions: HashSet<String>,
}

impl MediaRegistry {
    /// Initializes the global registry with the set of extensions discovered in the
    /// underlying `libmpv` build. Native image, audio, and document formats are always
    /// included first so the native stack wins any overlap. Subsequent calls are ignored —
    /// the registry can only be set once per process.
    pub fn init(mpv_discovered: HashSet<String>) {
        let mut allowed: HashSet<String> = native_image_extensions()
            .iter()
            .chain(NATIVE_AUDIO_EXTS.iter())
            .chain(NATIVE_DOCUMENT_EXTS.iter())
            .map(|ext| (*ext).to_string())
            .collect();

        allowed.extend(mpv_discovered.iter().cloned());

        let _ = SYSTEM_REGISTRY.set(MediaRegistry {
            allowed_extensions: allowed,
            mpv_extensions: mpv_discovered,
        });
    }

    /// Returns the set of extensions that would be allowed even when the global
    /// registry has not been initialized yet.
    ///
    /// Used as a safety net for the filesystem scanner and tests so behavior
    /// remains sensible if [`MediaRegistry::init`] was never called (e.g. mpv
    /// failed to load). The video extensions come from the compile-time
    /// baseline in [`MediaType::extensions`] — mpv-discovered formats that
    /// aren't in that hardcoded list are NOT included here.
    pub fn fallback_allowed_extensions() -> HashSet<String> {
        let mut set: HashSet<String> = native_image_extensions()
            .iter()
            .chain(NATIVE_AUDIO_EXTS.iter())
            .chain(NATIVE_DOCUMENT_EXTS.iter())
            .map(|ext| (*ext).to_string())
            .collect();
        set.extend(
            MediaType::Video
                .extensions()
                .iter()
                .map(|ext| (*ext).to_string()),
        );
        set
    }

    /// Determines the media type for the given file extension using a strict priority
    /// order:
    /// 1. Native image formats
    /// 2. Native audio formats
    /// 3. Native document formats
    /// 4. Anything supported by the discovered `libmpv` build is treated as `Video`
    /// 5. Returns `None` when the extension does not match any known handler.
    ///
    /// This is the **authoritative** type check — it accounts for the full
    /// set of formats that mpv reported at startup, which may be broader than
    /// the compile-time baseline returned by [`MediaType::extensions`].
    /// Prefer this over `MediaType::extensions().contains(...)` when you need
    /// to know whether a file can actually be played.
    pub fn determine_type(ext: &str) -> Option<MediaType> {
        let ext_lower = ext.to_lowercase();

        if native_image_extensions().contains(&ext_lower.as_str()) {
            return Some(MediaType::Image);
        }
        if NATIVE_AUDIO_EXTS.contains(&ext_lower.as_str()) {
            return Some(MediaType::Audio);
        }
        if NATIVE_DOCUMENT_EXTS.contains(&ext_lower.as_str()) {
            return Some(MediaType::Document);
        }

        let registry = SYSTEM_REGISTRY.get()?;
        if registry.mpv_extensions.contains(&ext_lower) {
            return Some(MediaType::Video);
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determine_type_native_image_wins_over_mpv() {
        assert_eq!(MediaRegistry::determine_type("jpg"), Some(MediaType::Image));
        assert_eq!(MediaRegistry::determine_type("PNG"), Some(MediaType::Image));
    }

    #[test]
    fn test_determine_type_native_audio_wins_over_mpv() {
        assert_eq!(MediaRegistry::determine_type("mp3"), Some(MediaType::Audio));
        assert_eq!(
            MediaRegistry::determine_type("FLAC"),
            Some(MediaType::Audio)
        );
    }

    #[test]
    fn test_determine_type_unknown_returns_none_without_registry() {
        use std::collections::HashSet;
        let mpv: HashSet<String> = ["mkv".into(), "webm".into()].into_iter().collect();
        MediaRegistry::init(mpv);
        assert_eq!(MediaRegistry::determine_type("xyz"), None);
    }

    #[test]
    fn test_fallback_allowed_extensions_includes_native() {
        let fallback = MediaRegistry::fallback_allowed_extensions();
        for ext in native_image_extensions() {
            assert!(fallback.contains(*ext), "missing native image ext {ext}");
        }
        for ext in NATIVE_AUDIO_EXTS {
            assert!(fallback.contains(*ext), "missing native audio ext {ext}");
        }
        for ext in NATIVE_DOCUMENT_EXTS {
            assert!(fallback.contains(*ext), "missing native document ext {ext}");
        }
    }

    #[test]
    fn test_image_extensions() {
        let exts = MediaType::Image.extensions();
        assert!(exts.contains(&"png"));
        assert!(exts.contains(&"jpg"));
        assert!(exts.contains(&"jpeg"));
        assert!(exts.contains(&"bmp"));
        assert!(exts.contains(&"webp"));
        assert!(exts.contains(&"svg"));
        assert!(exts.contains(&"svgz"));
        assert_eq!(MediaRegistry::determine_type("SVG"), Some(MediaType::Image));
        assert_eq!(
            MediaRegistry::determine_type("svgz"),
            Some(MediaType::Image)
        );
    }

    #[test]
    fn test_video_extensions() {
        let exts = MediaType::Video.extensions();
        assert!(exts.contains(&"mp4"));
        assert!(exts.contains(&"mkv"));
        assert!(exts.contains(&"webm"));
        assert!(exts.contains(&"avi"));
        assert!(exts.contains(&"mov"));
        assert!(exts.contains(&"gif"));
    }

    #[test]
    fn test_audio_extensions() {
        let exts = MediaType::Audio.extensions();
        assert!(exts.contains(&"mp3"));
        assert!(exts.contains(&"flac"));
        assert!(exts.contains(&"wav"));
        assert!(exts.contains(&"ogg"));
        assert!(exts.contains(&"aac"));
        assert!(exts.contains(&"opus"));
    }

    #[test]
    fn test_all_extensions_no_duplicates() {
        let all = MediaType::all_extensions();
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(all.len(), sorted.len(), "duplicate extensions found");
    }

    #[test]
    fn test_all_extensions_covers_each_type() {
        let all = MediaType::all_extensions();
        let image_exts = MediaType::Image.extensions();
        let video_exts = MediaType::Video.extensions();
        let audio_exts = MediaType::Audio.extensions();
        let document_exts = MediaType::Document.extensions();

        for ext in image_exts {
            assert!(all.contains(ext), "image ext {} not in all_extensions", ext);
        }
        for ext in video_exts {
            assert!(all.contains(ext), "video ext {} not in all_extensions", ext);
        }
        for ext in audio_exts {
            assert!(all.contains(ext), "audio ext {} not in all_extensions", ext);
        }
        for ext in document_exts {
            assert!(
                all.contains(ext),
                "document ext {} not in all_extensions",
                ext
            );
        }

        assert_eq!(
            all.len(),
            image_exts.len() + video_exts.len() + audio_exts.len() + document_exts.len(),
            "all_extensions count mismatch"
        );
    }

    #[test]
    fn test_document_extensions_and_kind() {
        let exts = MediaType::Document.extensions();
        assert!(exts.contains(&"pdf"));
        assert!(exts.contains(&"md"));
        assert!(exts.contains(&"markdown"));
        assert!(exts.contains(&"txt"));
        assert!(exts.contains(&"csv"));
        assert!(exts.contains(&"tsv"));

        assert_eq!(
            MediaRegistry::determine_type("PDF"),
            Some(MediaType::Document)
        );
        assert_eq!(
            MediaRegistry::determine_type("MD"),
            Some(MediaType::Document)
        );
        assert_eq!(
            MediaRegistry::determine_type("txt"),
            Some(MediaType::Document)
        );
        assert_eq!(DocumentKind::from_extension("pdf"), Some(DocumentKind::Pdf));
        assert_eq!(
            DocumentKind::from_extension("md"),
            Some(DocumentKind::Markdown)
        );
        assert_eq!(
            DocumentKind::from_extension("MARKDOWN"),
            Some(DocumentKind::Markdown)
        );
        assert_eq!(
            DocumentKind::from_extension("txt"),
            Some(DocumentKind::PlainText)
        );
        assert_eq!(
            DocumentKind::from_extension("csv"),
            Some(DocumentKind::PlainText)
        );
        assert_eq!(
            DocumentKind::from_extension("tsv"),
            Some(DocumentKind::PlainText)
        );
        assert_eq!(DocumentKind::from_extension("jpg"), None);
    }

    #[test]
    fn test_gif_not_in_image_extensions() {
        let image_exts = MediaType::Image.extensions();
        assert!(
            !image_exts.contains(&"gif"),
            "GIF should not be in image extensions"
        );
    }

    #[test]
    fn test_gif_in_video_extensions() {
        let video_exts = MediaType::Video.extensions();
        assert!(
            video_exts.contains(&"gif"),
            "GIF should be in video extensions"
        );
    }

    #[test]
    fn test_determine_type_empty_extension() {
        use std::collections::HashSet;
        let mpv: HashSet<String> = ["mkv".into(), "webm".into()].into_iter().collect();
        MediaRegistry::init(mpv);
        let result = MediaRegistry::determine_type("");
        assert!(result.is_none());
    }

    #[test]
    fn test_determine_type_no_extension() {
        use std::collections::HashSet;
        let mpv: HashSet<String> = ["mkv".into(), "webm".into()].into_iter().collect();
        MediaRegistry::init(mpv);
        let result = MediaRegistry::determine_type("noextension");
        assert!(result.is_none());
    }

    #[test]
    fn test_determine_type_mixed_case() {
        assert_eq!(MediaRegistry::determine_type("JpG"), Some(MediaType::Image));
        assert_eq!(
            MediaRegistry::determine_type("FlAc"),
            Some(MediaType::Audio)
        );
    }

    #[test]
    fn test_media_registry_init_and_determine_type() {
        use std::collections::HashSet;

        let mpv_exts: HashSet<String> = ["mkv".into(), "webm".into()].into_iter().collect();
        MediaRegistry::init(mpv_exts);

        assert_eq!(MediaRegistry::determine_type("jpg"), Some(MediaType::Image));
        assert_eq!(MediaRegistry::determine_type("png"), Some(MediaType::Image));
        assert_eq!(MediaRegistry::determine_type("mp3"), Some(MediaType::Audio));
        assert_eq!(
            MediaRegistry::determine_type("flac"),
            Some(MediaType::Audio)
        );

        assert_eq!(MediaRegistry::determine_type("xyz"), None);

        let was_init_by_us = SYSTEM_REGISTRY
            .get()
            .is_some_and(|r| r.mpv_extensions.contains("mkv"));
        assert!(
            was_init_by_us,
            "registry init should have populated mpv_extensions"
        );
        if was_init_by_us {
            assert_eq!(MediaRegistry::determine_type("mkv"), Some(MediaType::Video));
            assert_eq!(
                MediaRegistry::determine_type("webm"),
                Some(MediaType::Video)
            );
        }

        let novel: HashSet<String> = ["novel_init_idempotent".into()].into_iter().collect();
        MediaRegistry::init(novel);
        if was_init_by_us {
            assert_eq!(MediaRegistry::determine_type("novel_init_idempotent"), None);
        }
    }
}
