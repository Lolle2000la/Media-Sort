use std::collections::BTreeMap;
use std::path::Path;

use media_sort_core::media_type::DocumentKind;

use crate::media::document::text;
use crate::metadata::{MetadataError, build_file_section, merge_sections};

/// Extracts `"File"` and `"Document"` metadata sections for the metadata inspector.
///
/// Format-specific parser failures are logged and ignored so basic file attributes and `"Kind"`
/// always remain available.
pub fn extract_document_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    let mut dirs: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    dirs.insert("File".into(), build_file_section(path)?);

    let kind = DocumentKind::from_path(path);
    let kind_label = match kind {
        Some(DocumentKind::Markdown) => "Markdown",
        Some(DocumentKind::PlainText) => "Text",
        None => "Document",
    };

    let mut doc_sec: BTreeMap<String, String> = BTreeMap::new();
    doc_sec.insert("Kind".into(), kind_label.into());

    let specific = match kind {
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

    let mut sections = BTreeMap::new();
    sections.insert("Document".into(), doc_sec);
    merge_sections(&mut dirs, sections);
    Ok(dirs)
}
