use std::collections::BTreeMap;
use std::path::Path;

use super::{
    DecodedImage, DocumentError, DocumentPreview, DocumentUnavailableReason, MAX_XML_ENTRY_BYTES,
};

mod archive;
mod container;
mod cover;
mod index;
mod markdown;
mod opf;
mod xml;

use archive::{EpubFailure, open_epub_zip, read_zip_index_capped};
use cover::try_decode_epub_cover;
use index::get_or_build_epub_index;
use markdown::xhtml_to_markdown;

/// Cover preview dimensions attached to chapter 0 of an EPUB preview.
const EPUB_PREVIEW_COVER_MAX_WIDTH: u32 = 320;
const EPUB_PREVIEW_COVER_MAX_HEIGHT: u32 = 480;

fn epub_failure_to_preview(err: EpubFailure) -> Result<DocumentPreview, DocumentError> {
    match err {
        EpubFailure::Encrypted => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::Encrypted,
        }),
        EpubFailure::NotFound(msg) | EpubFailure::Corrupt(msg) => {
            Ok(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(msg),
            })
        }
        EpubFailure::TooLarge(_) => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::TooLarge,
        }),
        EpubFailure::Io(io_err) => Err(DocumentError::Io(io_err)),
    }
}

/// Extracts and resizes the EPUB cover image to fit inside `max_width` × `max_height`.
///
/// Missing, encrypted, or unreadable covers cleanly return `Ok(None)`.
pub fn extract_epub_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    let (key, index, maybe_archive) = match get_or_build_epub_index(path) {
        Ok(res) => res,
        Err(EpubFailure::Encrypted) => return Ok(None),
        Err(err) => return Err(err.into_document_error()),
    };

    if index.cover_candidates.is_empty() {
        return Ok(None);
    }

    let mut archive = match maybe_archive {
        Some(a) => a,
        None => match open_epub_zip(&key.path) {
            Ok(a) => a,
            Err(EpubFailure::Encrypted) => return Ok(None),
            Err(err) => return Err(err.into_document_error()),
        },
    };

    Ok(try_decode_epub_cover(
        &mut archive,
        &index.cover_candidates,
        max_width,
        max_height,
    ))
}

/// Loads chapter `page_index` (clamped to the EPUB spine length) converted to Markdown,
/// plus an optional cover image (up to `320x480`) when `page_index == 0`.
pub fn load_epub_preview(path: &Path, page_index: usize) -> Result<DocumentPreview, DocumentError> {
    let (key, index, maybe_archive) = match get_or_build_epub_index(path) {
        Ok(res) => res,
        Err(err) => return epub_failure_to_preview(err),
    };

    if index.spine_chapters.is_empty() {
        return Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::UnsupportedOrCorrupt(
                "EPUB spine contains no readable chapters".to_string(),
            ),
        });
    }

    let total_chapters = index.spine_chapters.len();
    let clamped = page_index.min(total_chapters - 1);
    let chapter = &index.spine_chapters[clamped];

    if chapter.encrypted {
        return Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::Encrypted,
        });
    }

    let Some(zip_idx) = chapter.zip_index else {
        return Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::UnsupportedOrCorrupt(format!(
                "Missing EPUB entry '{}'",
                chapter.resolved_path
            )),
        });
    };

    let mut archive = match maybe_archive {
        Some(a) => a,
        None => match open_epub_zip(&key.path) {
            Ok(a) => a,
            Err(err) => return epub_failure_to_preview(err),
        },
    };

    let chapter_bytes = match read_zip_index_capped(
        &mut archive,
        zip_idx,
        &chapter.resolved_path,
        MAX_XML_ENTRY_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(err) => return epub_failure_to_preview(err),
    };

    let markdown = xhtml_to_markdown(&chapter_bytes);
    let cover = if page_index == 0 && !index.cover_candidates.is_empty() {
        try_decode_epub_cover(
            &mut archive,
            &index.cover_candidates,
            EPUB_PREVIEW_COVER_MAX_WIDTH,
            EPUB_PREVIEW_COVER_MAX_HEIGHT,
        )
    } else {
        None
    };

    Ok(DocumentPreview::Markdown {
        markdown,
        chapter_index: Some(clamped),
        total_chapters: Some(total_chapters),
        cover,
    })
}

/// Extracts EPUB package metadata (`Chapters`, `Title`, `Author`, `Publisher`, `Language`,
/// `Date`, `Identifier`) for the metadata inspector.
pub fn extract_epub_metadata(path: &Path) -> Result<BTreeMap<String, String>, DocumentError> {
    match get_or_build_epub_index(path) {
        Ok((_, index, _)) => Ok(index.metadata.clone()),
        Err(EpubFailure::Encrypted) => {
            let mut fields = BTreeMap::new();
            fields.insert("Encrypted".into(), "Yes".into());
            Ok(fields)
        }
        Err(err) => Err(err.into_document_error()),
    }
}

#[cfg(test)]
mod test_support {
    use std::fs::File;
    use std::io::{BufWriter, Cursor, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use zip::CompressionMethod;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    pub(super) struct TempTestDir(PathBuf);

    impl TempTestDir {
        pub(super) fn new(prefix: &str) -> Self {
            let id = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "media-sort-epub-{prefix}-{}-{id}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }

        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(super) fn solid_png_bytes(r: u8, g: u8, b: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([r, g, b, 255]));
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, image::ImageFormat::Png)
            .expect("encode solid PNG");
        cursor.into_inner()
    }

    pub(super) fn write_epub_entries(path: &Path, entries: &[(&str, &[u8], CompressionMethod)]) {
        let file = File::create(path).expect("create epub");
        let mut writer = ZipWriter::new(BufWriter::new(file));
        for &(name, data, method) in entries {
            let opts = SimpleFileOptions::default().compression_method(method);
            writer.start_file(name, opts).expect("start epub entry");
            writer.write_all(data).expect("write epub entry");
        }
        writer
            .finish()
            .expect("finish epub")
            .into_inner()
            .expect("flush epub");
    }
}
