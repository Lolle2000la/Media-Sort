use std::path::Path;

use media_sort_core::media_type::MediaType;

use super::pool::ThumbnailResult;
use super::{THUMBNAIL_SIZE, ThumbnailError};

/// Generate a thumbnail for an image (including `.ico`/`.ICO`/`.svg`/`.svgz`
/// and GIFs) or embedded audio cover art via `media-sort-backend`.
pub fn generate_image_thumbnail(path: &Path, media_type: MediaType) -> ThumbnailResult {
    media_sort_backend::media::thumbnail::generate_thumbnail(path, THUMBNAIL_SIZE, THUMBNAIL_SIZE)
        .map(|opt| opt.map(|decoded| decoded.into_parts()))
        .map_err(|e| {
            let prefix = if media_type == MediaType::Audio {
                "Audio cover thumbnail error"
            } else {
                "Image decoding failed"
            };
            ThumbnailError::Decode(format!("{prefix}: {e}"))
        })
}
