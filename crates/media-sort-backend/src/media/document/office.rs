use std::path::Path;

use super::{
    DocumentError, DocumentPreview, DocumentUnavailableReason, PREVIEW_TARGET_HEIGHT,
    PREVIEW_TARGET_WIDTH, os_preview, pdf, soffice_pipe,
};

mod archive;
mod metadata;
mod odp;
mod odt;
mod oxide;
mod rtf;
mod spreadsheet;
mod thumbnail;
mod xml;

pub use metadata::extract_office_metadata;
pub use spreadsheet::load_spreadsheet_preview;
pub use thumbnail::{
    extract_fast_office_thumbnail, extract_office_thumbnail, render_libreoffice_thumbnail,
};

/// Loads an Office document preview.
///
/// When `enable_libreoffice` is enabled and LibreOffice is available, converts the document to a
/// cached PDF file and renders `page_index` via `pdf::load_pdf_preview`; otherwise (or on
/// conversion failure) falls back to pure-Rust XML/RTF parsing or OS thumbnails.
pub fn load_office_preview(
    path: &Path,
    page_index: usize,
    enable_libreoffice: bool,
) -> Result<DocumentPreview, DocumentError> {
    if enable_libreoffice && soffice_pipe::is_libreoffice_available() {
        match soffice_pipe::convert_to_pdf_path(path) {
            Ok(Some(pdf_path)) => match pdf::load_pdf_preview(&pdf_path, page_index) {
                Ok(preview @ DocumentPreview::PagedImage { .. }) => return Ok(preview),
                Ok(other) => {
                    tracing::warn!(
                        path = %path.display(),
                        preview = ?other,
                        "LibreOffice-converted PDF produced non-paged preview; falling back to pure-Rust parser"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "Failed to render LibreOffice-converted PDF; falling back to pure-Rust parser"
                    );
                }
            },
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "LibreOffice conversion failed; falling back to pure-Rust parser"
                );
            }
        }
    }

    let ext = archive::extension_lower(path);
    match ext.as_str() {
        "docx" | "pptx" => oxide::load_oxide_preview(path),
        "odt" => odt::load_odt_preview(path),
        "odp" => odp::load_odp_preview(path),
        "rtf" => rtf::load_rtf_preview(path),
        "doc" | "ppt" => {
            let preview = oxide::load_oxide_preview(path)?;
            if matches!(
                preview,
                DocumentPreview::Unavailable {
                    reason: ref r
                } if !matches!(r, DocumentUnavailableReason::Encrypted)
            ) && let Some(image) = os_preview::generate_os_thumbnail(
                path,
                PREVIEW_TARGET_WIDTH,
                PREVIEW_TARGET_HEIGHT,
            )? {
                return Ok(DocumentPreview::PagedImage {
                    image,
                    page_index: 0,
                    total_pages: 1,
                });
            }
            Ok(preview)
        }
        _ => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::NoPreviewBackend,
        }),
    }
}
