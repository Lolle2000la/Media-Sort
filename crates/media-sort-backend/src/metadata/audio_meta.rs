use std::collections::BTreeMap;
use std::path::Path;

use crate::metadata::{MetadataError, build_file_section, merge_sections};

pub fn extract_audio_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    let mut dirs: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    // 1. Add basic file metadata
    dirs.insert("File".into(), build_file_section(path)?);

    // 2. Read specific audio tags
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let specific_tags = match ext.to_lowercase().as_str() {
        "mp3" | "aiff" | "wav" => extract_id3_metadata(path),
        "flac" => extract_flac_metadata(path),
        "ogg" | "opus" => extract_vorbis_comment_metadata(path),
        "m4a" | "aac" => super::video_meta::extract_video_metadata(path),
        _ => extract_generic_container_metadata(path),
    };

    if let Ok(tags) = specific_tags {
        merge_sections(&mut dirs, tags);
    }

    Ok(dirs)
}

fn extract_id3_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    use id3::{Tag, TagLike};

    let tag = Tag::read_from_path(path)?;
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut id3_section: BTreeMap<String, String> = BTreeMap::new();

    if let Some(title) = tag.title() {
        id3_section.insert("Title".into(), title.to_string());
    }
    if let Some(artist) = tag.artist() {
        id3_section.insert("Artist".into(), artist.to_string());
    }
    if let Some(album) = tag.album() {
        id3_section.insert("Album".into(), album.to_string());
    }
    if let Some(year) = tag.year() {
        id3_section.insert("Year".into(), year.to_string());
    }
    if let Some(genre) = tag.genre() {
        id3_section.insert("Genre".into(), genre.to_string());
    }
    if let Some(track) = tag.track() {
        id3_section.insert("Track".into(), track.to_string());
    }

    if !id3_section.is_empty() {
        sections.insert("ID3 Metadata".into(), id3_section);
    }

    Ok(sections)
}

fn extract_flac_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    use metaflac::Tag;

    let tag = Tag::read_from_path(path)?;
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut flac_section: BTreeMap<String, String> = BTreeMap::new();

    if let Some(vc) = tag.vorbis_comments() {
        flac_section.extend(
            vc.comments
                .iter()
                .filter_map(|(key, values)| Some((key.clone(), values.first()?.clone()))),
        );
    }

    if !flac_section.is_empty() {
        sections.insert("FLAC Vorbis Comment".into(), flac_section);
    }

    Ok(sections)
}

fn extract_vorbis_comment_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    super::video_meta::extract_generic_container_metadata(path)
}

fn extract_generic_container_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    super::video_meta::extract_generic_container_metadata(path)
}
