use std::collections::BTreeMap;
use std::path::Path;

pub use crate::metadata::MetadataError;

pub fn extract_image_metadata(
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, MetadataError> {
    use exif::Reader;

    let mut dirs: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    // 1. Add basic file metadata + dimensions
    let mut file_sec = crate::metadata::build_file_section(path)?;
    if let Ok((w, h)) = crate::media::image_decoder::decode_image_dimensions(path) {
        file_sec.insert("Dimensions".into(), format!("{} x {}", w, h));
    }
    dirs.insert("File".into(), file_sec);

    // 2. Try to load EXIF
    if let Ok(file) = std::fs::File::open(path) {
        let mut buf_reader = std::io::BufReader::new(&file);
        if let Ok(exif) = Reader::new().read_from_container(&mut buf_reader) {
            for field in exif.fields() {
                let ifd_name = format!("EXIF IFD {}", field.ifd_num);
                let tag_name = field.tag.to_string();
                let value = field.display_value().to_string();
                dirs.entry(ifd_name).or_default().insert(tag_name, value);
            }
        }
    }

    Ok(dirs)
}
