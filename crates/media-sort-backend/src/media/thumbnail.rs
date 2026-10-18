use image::GenericImageView;
use std::path::Path;

pub(crate) fn calculate_thumbnail_dimensions(
    src_w: u32,
    src_h: u32,
    max_w: u32,
    max_h: u32,
) -> (u32, u32) {
    let ratio = src_w as f64 / src_h as f64;
    let max_ratio = max_w as f64 / max_h as f64;

    if ratio > max_ratio {
        (max_w, (max_w as f64 / ratio).round() as u32)
    } else {
        ((max_h as f64 * ratio).round() as u32, max_h)
    }
}

/// Produce a grid thumbnail for `path`.
///
/// Returns `Ok(None)` when the file has no image to show — an audio file
/// without embedded cover art. That is a normal, playable file, not an error.
pub fn generate_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<super::DecodedImage>, image::ImageError> {
    super::format_pipeline::thumbnail_image(path, max_width, max_height)
}

/// Dimensions of the image a thumbnail would be made from, or `Ok(None)` when
/// there is no image (an audio file without embedded cover art).
pub fn thumbnail_dimensions(path: &Path) -> Result<Option<(u32, u32)>, image::ImageError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if super::format_pipeline::is_audio_extension(&ext) {
        return Ok(match extract_audio_cover(path) {
            Some(bytes) => {
                Some(super::image_decoder::decode_bytes_with_limits(bytes.as_slice())?.dimensions())
            }
            None => None,
        });
    }
    let img = super::image_decoder::load_image(path)?;
    Ok(Some(img.dimensions()))
}

pub fn extract_audio_cover(path: &Path) -> Option<Vec<u8>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext.to_lowercase().as_str() {
        "mp3" | "aiff" | "wav" => {
            let tag = id3::Tag::read_from_path(path).ok()?;

            tag.pictures().next().map(|p| p.data.clone())
        }
        "flac" => {
            let tag = metaflac::Tag::read_from_path(path).ok()?;

            tag.pictures().next().map(|p| p.data.clone())
        }
        "mp4" | "m4a" | "m4v" | "mov" | "aac" => {
            let mut file = std::fs::File::open(path).ok()?;
            let tag = mp4ameta::Tag::read_from(&mut file).ok()?;
            tag.artwork().map(|img| img.data.to_vec())
        }
        _ => {
            use symphonia::core::formats::FormatOptions;
            use symphonia::core::formats::probe::Hint;
            use symphonia::core::io::MediaSourceStream;
            use symphonia::core::meta::MetadataOptions;

            let file = std::fs::File::open(path).ok()?;
            let mss = MediaSourceStream::new(Box::new(file), Default::default());
            let mut hint = Hint::new();
            if !ext.is_empty() {
                hint.with_extension(ext);
            }

            let mut format = symphonia::default::get_probe()
                .probe(
                    &hint,
                    mss,
                    FormatOptions::default(),
                    MetadataOptions::default(),
                )
                .ok()?;

            let mut metadata = format.metadata();

            if let Some(revision) = metadata.current()
                && let Some(visual) = revision.media.visuals.first()
            {
                return Some(visual.data.to_vec());
            }

            while !metadata.is_latest() {
                if let Some(revision) = metadata.pop()
                    && let Some(visual) = revision.media.visuals.first()
                {
                    return Some(visual.data.to_vec());
                }
            }
            None
        }
    }
}
