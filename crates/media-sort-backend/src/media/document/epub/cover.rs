use std::collections::HashSet;
use std::fs::File;
use std::io::BufReader;

use zip::ZipArchive;

use super::super::{DecodedImage, MAX_ARCHIVE_ENTRY_BYTES, is_image_entry};
use super::archive::read_zip_index_capped;
use super::opf::{ManifestItem, ParsedEpub};
use crate::media::format_pipeline::decode_and_resize_bytes;

#[derive(Debug, Clone)]
pub(super) struct EpubZipTarget {
    pub(super) resolved_path: String,
    pub(super) zip_index: usize,
}

fn is_raster_image_item(item: &ManifestItem) -> bool {
    let mt = item.media_type.trim().to_ascii_lowercase();
    if mt == "image/svg+xml" {
        return false;
    }
    if mt.starts_with("image/") {
        return true;
    }
    is_image_entry(&item.resolved_path)
}

/// Collects deduplicated raster cover candidates in priority order:
/// 1) EPUB 3 `properties="cover-image"`
/// 2) EPUB 2 `<meta name="cover" content="..."/>`
/// 3) First manifest image whose `id` or `href` contains `"cover"`.
pub(super) fn collect_cover_candidates(epub: &ParsedEpub) -> Vec<&ManifestItem> {
    let mut candidates: Vec<&ManifestItem> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    // (1) EPUB 3 `properties="cover-image"`
    for id in &epub.manifest_order {
        if let Some(item) = epub.manifest.get(id)
            && item.is_cover_image
            && is_raster_image_item(item)
            && seen.insert(item.resolved_path.as_str())
        {
            candidates.push(item);
        }
    }

    // (2) EPUB 2 `<meta name="cover" content="..."/>` if it points to an image
    if let Some(cover_id) = epub.metadata.cover_id.as_deref()
        && let Some(item) = epub.manifest.get(cover_id)
        && is_raster_image_item(item)
        && seen.insert(item.resolved_path.as_str())
    {
        candidates.push(item);
    }

    // (3) First image item whose id or href contains "cover" (case-insensitive)
    for id in &epub.manifest_order {
        if let Some(item) = epub.manifest.get(id)
            && is_raster_image_item(item)
            && (id.to_ascii_lowercase().contains("cover")
                || item.resolved_path.to_ascii_lowercase().contains("cover"))
            && seen.insert(item.resolved_path.as_str())
        {
            candidates.push(item);
            break;
        }
    }

    candidates
}

pub(super) fn try_decode_epub_cover(
    archive: &mut ZipArchive<BufReader<File>>,
    cover_candidates: &[EpubZipTarget],
    max_width: u32,
    max_height: u32,
) -> Option<DecodedImage> {
    for candidate in cover_candidates {
        let Ok(bytes) = read_zip_index_capped(
            archive,
            candidate.zip_index,
            &candidate.resolved_path,
            MAX_ARCHIVE_ENTRY_BYTES,
        ) else {
            continue;
        };
        if let Ok(img) = decode_and_resize_bytes(&bytes, max_width, max_height) {
            return Some(img);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use zip::CompressionMethod;

    use super::super::archive::{find_zip_entry_index, open_epub_zip};
    use super::super::opf::parse_opf;
    use super::super::test_support::{TempTestDir, solid_png_bytes, write_epub_entries};
    use super::*;

    #[test]
    fn collect_cover_candidates_respects_priority_dedup_and_svg_exclusion() {
        let opf = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
                <meta name="cover" content="meta-cover"/>
            </metadata>
            <manifest>
                <item id="svg-cover" href="cover.svg" media-type="image/svg+xml" properties="cover-image"/>
                <item id="xhtml-cover" href="cover.xhtml" media-type="application/xhtml+xml" properties="cover-image"/>
                <item id="fallback-cover" href="images/Cover-Alt.jpg" media-type="image/jpeg"/>
                <item id="meta-cover" href="images/meta.png" media-type="image/png"/>
                <item id="prop-cover" href="images/prop.png" media-type="image/png" properties="cover-image"/>
                <item id="dup-cover" href="images/prop.png" media-type="image/png" properties="cover-image"/>
            </manifest>
            <spine/>
        </package>"#;

        let parsed = parse_opf(opf, "OEBPS", HashSet::new());
        let candidates = collect_cover_candidates(&parsed);
        let paths: Vec<&str> = candidates
            .iter()
            .map(|c| c.resolved_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "OEBPS/images/prop.png",
                "OEBPS/images/meta.png",
                "OEBPS/images/Cover-Alt.jpg",
            ]
        );
    }

    #[test]
    fn try_decode_epub_cover_falls_back_when_first_candidate_is_corrupt() {
        let dir = TempTestDir::new("cover_fallback");
        let epub_path = dir.path().join("cover.epub");
        let valid_png = solid_png_bytes(10, 200, 30);

        write_epub_entries(
            &epub_path,
            &[
                ("bad.png", b"not a real png", CompressionMethod::Deflated),
                ("good.png", &valid_png, CompressionMethod::Deflated),
            ],
        );

        let mut archive = open_epub_zip(&epub_path).expect("open zip");
        let targets = vec![
            EpubZipTarget {
                resolved_path: "bad.png".to_string(),
                zip_index: find_zip_entry_index(&archive, "bad.png").expect("bad idx"),
            },
            EpubZipTarget {
                resolved_path: "good.png".to_string(),
                zip_index: find_zip_entry_index(&archive, "good.png").expect("good idx"),
            },
        ];

        let decoded = try_decode_epub_cover(&mut archive, &targets, 64, 64)
            .expect("second cover candidate decodes");
        assert!(decoded.width > 0 && decoded.height > 0);
    }
}
