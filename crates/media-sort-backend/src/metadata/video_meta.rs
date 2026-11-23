use std::collections::BTreeMap;
use std::path::Path;

use crate::metadata::{MetadataError, build_file_section, merge_sections};

pub fn extract_video_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    let mut dirs: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    // 1. Add basic file metadata
    dirs.insert("File".into(), build_file_section(path)?);

    // 2. Read specific video tags
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let specific_tags = match ext.to_lowercase().as_str() {
        "mp4" | "m4v" | "mov" => extract_mp4_metadata(path),
        "mkv" | "webm" => extract_matroska_metadata(path),
        _ => extract_generic_container_metadata(path),
    };

    if let Ok(tags) = specific_tags {
        merge_sections(&mut dirs, tags);
    }

    Ok(dirs)
}

fn extract_mp4_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    use mp4ameta::Tag;

    let mut file = std::fs::File::open(path)?;
    let tag = Tag::read_from(&mut file)?;

    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut mp4_section: BTreeMap<String, String> = BTreeMap::new();

    if let Some(title) = tag.title() {
        mp4_section.insert("Title".into(), title.to_string());
    }
    if let Some(artist) = tag.artist() {
        mp4_section.insert("Artist".into(), artist.to_string());
    }
    if let Some(album) = tag.album() {
        mp4_section.insert("Album".into(), album.to_string());
    }
    if let Some(year) = tag.year() {
        mp4_section.insert("Year".into(), year.to_string());
    }
    if let Some(genre) = tag.genre() {
        mp4_section.insert("Genre".into(), genre.to_string());
    }
    if let Some(track) = tag.track_number() {
        mp4_section.insert("Track".into(), track.to_string());
    }
    if let Some(total) = tag.total_tracks() {
        mp4_section.insert("Total Tracks".into(), total.to_string());
    }

    if !mp4_section.is_empty() {
        sections.insert("MP4 Metadata".into(), mp4_section);
    }

    Ok(sections)
}

fn extract_matroska_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    extract_generic_container_metadata(path)
}

fn collect_revision_tags(
    revision: &symphonia::core::meta::MetadataRevision,
    container_sec: &mut BTreeMap<String, String>,
) {
    for tag in &revision.media.tags {
        let key = match &tag.std {
            Some(std_key) => format!("{std_key:?}"),
            None => tag.raw.key.clone(),
        };
        let val = tag.raw.value.to_string();
        if !val.trim().is_empty() {
            container_sec.insert(key, val);
        }
    }
}

pub fn extract_generic_container_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let mut sections = BTreeMap::new();

    if let Ok(file) = std::fs::File::open(path) {
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let meta_opts = MetadataOptions::default();
        let fmt_opts = FormatOptions::default();

        if let Ok(mut format) =
            symphonia::default::get_probe().probe(&hint, mss, fmt_opts, meta_opts)
        {
            let mut metadata = format.metadata();
            let mut container_sec = BTreeMap::new();

            while !metadata.is_latest() {
                if let Some(revision) = metadata.pop() {
                    collect_revision_tags(&revision, &mut container_sec);
                }
            }

            if let Some(revision) = metadata.current() {
                collect_revision_tags(revision, &mut container_sec);
            }

            if !container_sec.is_empty() {
                sections.insert("Container Metadata".into(), container_sec);
            }
        }
    }

    Ok(sections)
}
