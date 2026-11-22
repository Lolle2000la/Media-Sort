use std::collections::BTreeMap;
use std::path::Path;

use media_sort_core::media_type::DocumentKind;

use super::image_meta::MetadataError;
use crate::media::document::{comic, epub, office, pdf, text};

/// Extracts `"File"` and `"Document"` metadata sections for the metadata inspector.
///
/// Format-specific parser failures are logged and ignored so basic file attributes and `"Kind"`
/// always remain available.
pub fn extract_document_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    let meta = std::fs::metadata(path)?;
    let mut dirs: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    let mut file_sec: BTreeMap<String, String> = BTreeMap::new();
    if let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) {
        file_sec.insert("Name".into(), name);
    }
    let bytes = meta.len();
    let size_str = if bytes >= 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    };
    file_sec.insert("Size".into(), size_str);

    if let Ok(modified) = meta.modified() {
        let datetime: chrono::DateTime<chrono::Local> = modified.into();
        file_sec.insert(
            "Modified".into(),
            datetime.format("%Y-%m-%d %H:%M:%S").to_string(),
        );
    }
    dirs.insert("File".into(), file_sec);

    let kind = DocumentKind::from_path(path);
    let kind_label = match kind {
        Some(DocumentKind::Pdf) => "PDF",
        Some(DocumentKind::Comic) => "Comic Archive",
        Some(DocumentKind::Epub) => "EPUB",
        Some(DocumentKind::Spreadsheet) => "Spreadsheet",
        Some(DocumentKind::Office) => "Office Document",
        Some(DocumentKind::Markdown) => "Markdown",
        Some(DocumentKind::PlainText) => "Text",
        None => "Document",
    };

    let mut doc_sec: BTreeMap<String, String> = BTreeMap::new();
    doc_sec.insert("Kind".into(), kind_label.into());

    let specific = match kind {
        Some(DocumentKind::Pdf) => pdf::extract_pdf_metadata(path),
        Some(DocumentKind::Comic) => comic::extract_comic_metadata(path),
        Some(DocumentKind::Epub) => epub::extract_epub_metadata(path),
        Some(DocumentKind::Office) | Some(DocumentKind::Spreadsheet) => {
            office::extract_office_metadata(path)
        }
        Some(DocumentKind::Markdown) | Some(DocumentKind::PlainText) => {
            text::extract_text_metadata(path)
        }
        None => Ok(BTreeMap::new()),
    };

    match specific {
        Ok(fields) => {
            for (k, v) in fields {
                if !v.trim().is_empty() {
                    doc_sec.insert(k, v);
                }
            }
        }
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "Failed to extract format-specific document metadata"
            );
        }
    }

    dirs.insert("Document".into(), doc_sec);
    Ok(dirs)
}
