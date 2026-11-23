use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use zip::ZipArchive;

use super::archive::extension_lower;
use crate::media::DecodedImage;
use crate::media::document::{
    DocumentError, MAX_ARCHIVE_ENTRY_BYTES, os_preview, pdf, read_zip_entry_capped, soffice_pipe,
};

/// Extracts a fast thumbnail for Office or spreadsheet documents without invoking LibreOffice.
///
/// Checks embedded archive thumbnails first (`Thumbnails/thumbnail.png`, `docProps/thumbnail.*`),
/// then falls back to the platform OS thumbnail provider (`os_preview::generate_os_thumbnail`).
/// Errors are logged at debug level and converted to `Ok(None)`.
pub fn extract_fast_office_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    let max_width = max_width.max(1);
    let max_height = max_height.max(1);
    let ext = extension_lower(path);

    if matches!(
        ext.as_str(),
        "docx" | "odt" | "pptx" | "odp" | "xlsx" | "xlsm" | "ods"
    ) && let Some(img) = extract_embedded_zip_thumbnail(path, max_width, max_height)
    {
        return Ok(Some(img));
    }

    match os_preview::generate_os_thumbnail(path, max_width, max_height) {
        Ok(Some(img)) => Ok(Some(img)),
        Ok(None) => Ok(None),
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "OS thumbnail lookup failed for Office document"
            );
            Ok(None)
        }
    }
}

/// Renders a thumbnail for an Office or spreadsheet document via LibreOffice.
///
/// Converts page 1 via `soffice_pipe::convert_first_page_to_pdf_path` and renders page 0 via
/// `pdf::render_pdf_thumbnail`. Errors are logged at debug level and converted to `Ok(None)` so
/// missing or unsupported Office thumbnails never pollute `media_errors`.
pub fn render_libreoffice_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    let max_width = max_width.max(1);
    let max_height = max_height.max(1);
    if !soffice_pipe::is_libreoffice_available() {
        return Ok(None);
    }

    match soffice_pipe::convert_first_page_to_pdf_path(path) {
        Ok(Some(pdf_path)) => match pdf::render_pdf_thumbnail(&pdf_path, max_width, max_height) {
            Ok(img) => Ok(img),
            Err(err) => {
                tracing::debug!(
                    path = %path.display(),
                    pdf_path = %pdf_path.display(),
                    error = %err,
                    "Failed to render PDF thumbnail from LibreOffice output"
                );
                Ok(None)
            }
        },
        Ok(None) => Ok(None),
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "LibreOffice conversion failed while generating Office thumbnail"
            );
            Ok(None)
        }
    }
}

/// Extracts a bounded thumbnail for Office or spreadsheet documents.
///
/// Checks fast embedded/OS sources via [`extract_fast_office_thumbnail`], and finally (when
/// `enable_libreoffice` is enabled) converts via [`render_libreoffice_thumbnail`].
pub fn extract_office_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
    enable_libreoffice: bool,
) -> Result<Option<DecodedImage>, DocumentError> {
    if let Some(img) = extract_fast_office_thumbnail(path, max_width, max_height)? {
        return Ok(Some(img));
    }
    if enable_libreoffice {
        return render_libreoffice_thumbnail(path, max_width, max_height);
    }
    Ok(None)
}

fn extract_embedded_zip_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Option<DecodedImage> {
    const CANDIDATES: [&str; 4] = [
        "Thumbnails/thumbnail.png",
        "docProps/thumbnail.png",
        "docProps/thumbnail.jpeg",
        "docProps/thumbnail.jpg",
    ];

    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(BufReader::new(file)).ok()?;

    let mut matched_names: Vec<String> = Vec::new();
    for candidate in CANDIDATES {
        if let Some(exact) = archive
            .file_names()
            .flatten()
            .find(|name| name.eq_ignore_ascii_case(candidate))
        {
            matched_names.push(exact.into_owned());
        }
    }

    for entry_name in matched_names {
        let Ok(bytes) = read_zip_entry_capped(&mut archive, &entry_name, MAX_ARCHIVE_ENTRY_BYTES)
        else {
            continue;
        };
        if let Ok(decoded) =
            crate::media::format_pipeline::decode_and_resize_bytes(&bytes, max_width, max_height)
        {
            return Some(decoded);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    use image::{ImageBuffer, Rgba};
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;

    use super::*;

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ms_office_thumb_test_{label}_{}_{seq}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn solid_png(r: u8, g: u8, b: u8) -> Vec<u8> {
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(4, 4, Rgba([r, g, b, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode png");
        out
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create zip");
        let mut writer = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in entries {
            writer.start_file(*name, opts).expect("start zip entry");
            writer.write_all(data).expect("write zip entry");
        }
        writer.finish().expect("finish zip");
    }

    #[test]
    fn embedded_thumbnail_precedence_and_corrupt_fallback() {
        let dir = TempDir::new("precedence");
        let odf_thumb = solid_png(10, 200, 30);
        let ooxml_thumb = solid_png(200, 20, 30);

        // `Thumbnails/thumbnail.png` takes precedence over `docProps/thumbnail.png` even when
        // listed later in the archive, and matching is ASCII case-insensitive.
        let both_path = dir.0.join("both.docx");
        write_zip(
            &both_path,
            &[
                ("docProps/thumbnail.png", &ooxml_thumb),
                ("Thumbnails/Thumbnail.PNG", &odf_thumb),
            ],
        );
        let decoded = extract_embedded_zip_thumbnail(&both_path, 32, 32).expect("decoded thumb");
        assert!(decoded.rgba[1] > decoded.rgba[0] && decoded.rgba[1] > decoded.rgba[2]);

        // Corrupt primary candidate falls through to the next valid candidate (`docProps/thumbnail.jpg`).
        let fallback_path = dir.0.join("fallback.pptx");
        write_zip(
            &fallback_path,
            &[
                ("Thumbnails/thumbnail.png", b"not a valid png"),
                ("docProps/thumbnail.jpg", &ooxml_thumb),
            ],
        );
        let decoded =
            extract_embedded_zip_thumbnail(&fallback_path, 32, 32).expect("fallback thumb");
        assert!(decoded.rgba[0] > decoded.rgba[1] && decoded.rgba[0] > decoded.rgba[2]);
    }

    #[test]
    fn embedded_thumbnail_returns_none_on_corrupt_zip_or_oversized_entry() {
        let dir = TempDir::new("corrupt");
        let corrupt = dir.0.join("corrupt.docx");
        std::fs::write(&corrupt, b"not a zip").unwrap();
        assert!(extract_embedded_zip_thumbnail(&corrupt, 32, 32).is_none());

        // Oversized (>32 MiB uncompressed) entry is rejected cleanly by read_zip_entry_capped.
        let oversized = dir.0.join("oversized.docx");
        let big = vec![0u8; (MAX_ARCHIVE_ENTRY_BYTES as usize) + 1024];
        write_zip(&oversized, &[("docProps/thumbnail.png", &big)]);
        assert!(extract_embedded_zip_thumbnail(&oversized, 32, 32).is_none());
    }
}
