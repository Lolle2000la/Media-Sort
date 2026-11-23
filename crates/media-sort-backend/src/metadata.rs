pub mod audio_meta;
pub mod document_meta;
pub mod image_meta;
pub mod video_meta;

use std::collections::BTreeMap;
use std::path::Path;

use media_sort_core::media_type::MediaType;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("extraction failed")]
    ExtractionFailed,
    #[error("metadata parse error: {0}")]
    ParseError(String),
}

impl From<id3::Error> for MetadataError {
    fn from(e: id3::Error) -> Self {
        MetadataError::ParseError(e.to_string())
    }
}

impl From<metaflac::Error> for MetadataError {
    fn from(e: metaflac::Error) -> Self {
        MetadataError::ParseError(e.to_string())
    }
}

impl From<mp4ameta::Error> for MetadataError {
    fn from(e: mp4ameta::Error) -> Self {
        MetadataError::ParseError(e.to_string())
    }
}

/// Build the common `"File"` metadata section (`Name`, `Size`, `Modified`) for `path`.
pub(crate) fn build_file_section(path: &Path) -> Result<BTreeMap<String, String>, MetadataError> {
    let meta = std::fs::metadata(path)?;
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

    Ok(file_sec)
}

pub(crate) fn merge_sections(
    dirs: &mut BTreeMap<String, BTreeMap<String, String>>,
    sections: BTreeMap<String, BTreeMap<String, String>>,
) {
    for (sec_name, sec_data) in sections {
        if sec_name == "File" {
            dirs.entry("File".into()).or_default().extend(sec_data);
        } else {
            dirs.insert(sec_name, sec_data);
        }
    }
}

/// Extract grouped metadata sections for `path` according to its [`MediaType`].
pub fn extract_metadata(
    path: &Path,
    media_type: MediaType,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    match media_type {
        MediaType::Image => image_meta::extract_image_metadata(path),
        MediaType::Audio => audio_meta::extract_audio_metadata(path),
        MediaType::Video => video_meta::extract_video_metadata(path),
        MediaType::Document => document_meta::extract_document_metadata(path),
    }
}
