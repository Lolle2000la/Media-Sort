use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use zip::ZipArchive;

use super::archive::{EpubFailure, find_zip_entry_index, open_epub_zip};
use super::container::parse_epub_package;
use super::cover::{EpubZipTarget, collect_cover_candidates};
use super::opf::{ManifestItem, ParsedEpub};
use crate::cache::{BoundedLru, CanonicalFileKey};

/// LRU entry cap for parsed EPUB package/spine index metadata.
const EPUB_INDEX_CACHE_CAPACITY: usize = 8;

type EpubIndexCache = Mutex<BoundedLru<CanonicalFileKey, Arc<EpubIndex>>>;

static EPUB_INDEX_CACHE: OnceLock<EpubIndexCache> = OnceLock::new();

fn epub_index_cache() -> &'static EpubIndexCache {
    EPUB_INDEX_CACHE.get_or_init(|| {
        Mutex::new(BoundedLru::new(
            NonZeroUsize::new(EPUB_INDEX_CACHE_CAPACITY)
                .expect("EPUB_INDEX_CACHE_CAPACITY is non-zero"),
        ))
    })
}

fn epub_cache_get(key: &CanonicalFileKey) -> Option<Arc<EpubIndex>> {
    let mut guard = epub_index_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.get(key).map(Arc::clone)
}

fn epub_cache_insert(key: CanonicalFileKey, index: Arc<EpubIndex>) {
    let mut guard = epub_index_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.insert(key, index);
}

#[derive(Debug, Clone)]
pub(super) struct EpubSpineChapter {
    pub(super) resolved_path: String,
    pub(super) zip_index: Option<usize>,
    pub(super) encrypted: bool,
}

#[derive(Debug)]
pub(super) struct EpubIndex {
    pub(super) cover_candidates: Vec<EpubZipTarget>,
    pub(super) metadata: BTreeMap<String, String>,
    pub(super) spine_chapters: Vec<EpubSpineChapter>,
}

pub(super) type EpubIndexLookup = (
    CanonicalFileKey,
    Arc<EpubIndex>,
    Option<ZipArchive<BufReader<File>>>,
);

fn resolve_manifest_zip_index(
    archive: &ZipArchive<BufReader<File>>,
    item: &ManifestItem,
) -> Option<usize> {
    find_zip_entry_index(archive, &item.resolved_path).or_else(|| {
        item.fallback_raw_path
            .as_deref()
            .and_then(|raw| find_zip_entry_index(archive, raw))
    })
}

fn build_epub_index(archive: &ZipArchive<BufReader<File>>, epub: ParsedEpub) -> EpubIndex {
    let cover_candidates = collect_cover_candidates(&epub)
        .into_iter()
        .filter(|item| !epub.encrypted_uris.contains(&item.resolved_path))
        .filter_map(|item| {
            resolve_manifest_zip_index(archive, item).map(|zip_index| EpubZipTarget {
                resolved_path: item.resolved_path.clone(),
                zip_index,
            })
        })
        .collect();

    let spine_chapters = epub
        .spine
        .iter()
        .filter_map(|idref| epub.manifest.get(idref))
        .map(|item| EpubSpineChapter {
            resolved_path: item.resolved_path.clone(),
            zip_index: resolve_manifest_zip_index(archive, item),
            encrypted: epub.encrypted_uris.contains(&item.resolved_path),
        })
        .collect();

    let mut metadata = BTreeMap::new();
    metadata.insert("Chapters".into(), epub.spine.len().to_string());
    if let Some(title) = epub.metadata.title {
        metadata.insert("Title".into(), title);
    }
    if !epub.metadata.creators.is_empty() {
        metadata.insert("Author".into(), epub.metadata.creators.join(", "));
    }
    if let Some(publisher) = epub.metadata.publisher {
        metadata.insert("Publisher".into(), publisher);
    }
    if let Some(language) = epub.metadata.language {
        metadata.insert("Language".into(), language);
    }
    if let Some(date) = epub.metadata.date {
        metadata.insert("Date".into(), date);
    }
    if let Some(identifier) = epub.metadata.identifier {
        metadata.insert("Identifier".into(), identifier);
    }

    EpubIndex {
        cover_candidates,
        metadata,
        spine_chapters,
    }
}

pub(super) fn get_or_build_epub_index(path: &Path) -> Result<EpubIndexLookup, EpubFailure> {
    let key = CanonicalFileKey::from_path(path)?;
    if let Some(cached) = epub_cache_get(&key) {
        return Ok((key, cached, None));
    }
    let mut archive = open_epub_zip(&key.path)?;
    let epub = parse_epub_package(&mut archive)?;
    let index = Arc::new(build_epub_index(&archive, epub));
    epub_cache_insert(key.clone(), Arc::clone(&index));
    Ok((key, index, Some(archive)))
}

#[cfg(test)]
mod tests {
    use zip::CompressionMethod;

    use super::super::test_support::{TempTestDir, solid_png_bytes, write_epub_entries};
    use super::super::{DocumentPreview, extract_epub_metadata, load_epub_preview};
    use super::*;

    /// Serializes tests that assert on global `EPUB_INDEX_CACHE` state.
    static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn epub_index_cache_hits_on_chapter_turns_and_metadata() {
        let _guard = CACHE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let dir = TempTestDir::new("index_cache");
        let epub_path = dir.path().join("book.epub");
        let cover_png = solid_png_bytes(40, 120, 220);

        let container_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;

        let content_opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:uuid:epub-cache-test</dc:identifier>
    <dc:title>Cached Book</dc:title>
    <dc:creator>Grace Hopper</dc:creator>
  </metadata>
  <manifest>
    <item id="cover-img" href="images/cover.png" media-type="image/png" properties="cover-image"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch3" href="ch3.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
    <itemref idref="ch2"/>
    <itemref idref="ch3"/>
  </spine>
</package>"#;

        write_epub_entries(
            &epub_path,
            &[
                (
                    "mimetype",
                    b"application/epub+zip",
                    CompressionMethod::Stored,
                ),
                (
                    "META-INF/container.xml",
                    container_xml,
                    CompressionMethod::Deflated,
                ),
                (
                    "OEBPS/content.opf",
                    content_opf,
                    CompressionMethod::Deflated,
                ),
                (
                    "OEBPS/images/cover.png",
                    &cover_png,
                    CompressionMethod::Deflated,
                ),
                (
                    "OEBPS/ch1.xhtml",
                    b"<html><body><h1>One</h1><p>First</p></body></html>",
                    CompressionMethod::Deflated,
                ),
                (
                    "OEBPS/ch2.xhtml",
                    b"<html><body><h1>Two</h1><p>Second</p></body></html>",
                    CompressionMethod::Deflated,
                ),
                (
                    "OEBPS/ch3.xhtml",
                    b"<html><body><h1>Three</h1><p>Third</p></body></html>",
                    CompressionMethod::Deflated,
                ),
            ],
        );

        let key = CanonicalFileKey::from_path(&epub_path).expect("epub key");
        assert!(epub_cache_get(&key).is_none());

        let ch0 = load_epub_preview(&epub_path, 0).expect("chapter 0");
        match ch0 {
            DocumentPreview::Markdown {
                markdown,
                chapter_index,
                total_chapters,
                cover,
            } => {
                assert_eq!(chapter_index, Some(0));
                assert_eq!(total_chapters, Some(3));
                assert!(markdown.contains("# One"));
                assert!(cover.is_some());
            }
            other => panic!("expected Markdown for ch0, got {other:?}"),
        }

        let cached = epub_cache_get(&key).expect("epub cache populated");
        assert_eq!(cached.spine_chapters.len(), 3);
        assert_eq!(cached.cover_candidates.len(), 1);
        assert_eq!(
            cached.cover_candidates[0].resolved_path,
            "OEBPS/images/cover.png"
        );
        assert_eq!(
            cached.metadata.get("Title").map(String::as_str),
            Some("Cached Book")
        );

        let ch1 = load_epub_preview(&epub_path, 1).expect("chapter 1");
        match ch1 {
            DocumentPreview::Markdown {
                markdown,
                chapter_index,
                total_chapters,
                cover,
            } => {
                assert_eq!(chapter_index, Some(1));
                assert_eq!(total_chapters, Some(3));
                assert!(markdown.contains("# Two"));
                assert!(cover.is_none());
            }
            other => panic!("expected Markdown for ch1, got {other:?}"),
        }

        let ch_clamped = load_epub_preview(&epub_path, 99).expect("clamped chapter");
        match ch_clamped {
            DocumentPreview::Markdown {
                markdown,
                chapter_index,
                total_chapters,
                cover,
            } => {
                assert_eq!(chapter_index, Some(2));
                assert_eq!(total_chapters, Some(3));
                assert!(markdown.contains("# Three"));
                assert!(cover.is_none());
            }
            other => panic!("expected Markdown for clamped chapter, got {other:?}"),
        }

        let meta = extract_epub_metadata(&epub_path).expect("cached metadata");
        assert_eq!(meta.get("Chapters").map(String::as_str), Some("3"));
        assert_eq!(meta.get("Title").map(String::as_str), Some("Cached Book"));
        assert_eq!(meta.get("Author").map(String::as_str), Some("Grace Hopper"));

        // Rewriting the archive with different contents/size invalidates the cache key.
        let updated_opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Updated Book Title With Different Length</dc:title>
  </metadata>
  <manifest>
    <item id="only" href="only.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="only"/></spine>
</package>"#;
        write_epub_entries(
            &epub_path,
            &[
                (
                    "mimetype",
                    b"application/epub+zip",
                    CompressionMethod::Stored,
                ),
                (
                    "META-INF/container.xml",
                    container_xml,
                    CompressionMethod::Deflated,
                ),
                (
                    "OEBPS/content.opf",
                    updated_opf,
                    CompressionMethod::Deflated,
                ),
                (
                    "OEBPS/only.xhtml",
                    b"<html><body><p>Updated chapter body</p></body></html>",
                    CompressionMethod::Deflated,
                ),
            ],
        );

        let updated_meta = extract_epub_metadata(&epub_path).expect("updated metadata");
        assert_eq!(updated_meta.get("Chapters").map(String::as_str), Some("1"));
        assert_eq!(
            updated_meta.get("Title").map(String::as_str),
            Some("Updated Book Title With Different Length")
        );
    }

    #[test]
    fn build_epub_index_excludes_encrypted_covers_marks_encrypted_chapters_and_resolves_fallback_paths()
     {
        let dir = TempTestDir::new("index_enc_fallback");
        let epub_path = dir.path().join("enc_fallback.epub");
        let cover_png = solid_png_bytes(90, 90, 90);

        let container_xml = br#"<container version="1.0"><rootfiles>
            <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
        </rootfiles></container>"#;
        let encryption_xml = br#"<encryption xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
            <enc:EncryptedData>
                <enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
                <enc:CipherData><enc:CipherReference URI="OEBPS/enc-cover.png"/></enc:CipherData>
            </enc:EncryptedData>
            <enc:EncryptedData>
                <enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
                <enc:CipherData><enc:CipherReference URI="OEBPS/secret.xhtml"/></enc:CipherData>
            </enc:EncryptedData>
        </encryption>"#;
        let opf = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata/>
            <manifest>
                <item id="c1" href="enc-cover.png" media-type="image/png" properties="cover-image"/>
                <item id="c2" href="plain%20cover.png" media-type="image/png" properties="cover-image"/>
                <item id="ch1" href="secret.xhtml" media-type="application/xhtml+xml"/>
                <item id="ch2" href="my%20chapter.xhtml" media-type="application/xhtml+xml"/>
            </manifest>
            <spine>
                <itemref idref="ch1"/>
                <itemref idref="ch2"/>
            </spine>
        </package>"#;

        write_epub_entries(
            &epub_path,
            &[
                (
                    "META-INF/container.xml",
                    container_xml,
                    CompressionMethod::Deflated,
                ),
                (
                    "META-INF/encryption.xml",
                    encryption_xml,
                    CompressionMethod::Deflated,
                ),
                ("OEBPS/content.opf", opf, CompressionMethod::Deflated),
                (
                    "OEBPS/enc-cover.png",
                    &cover_png,
                    CompressionMethod::Deflated,
                ),
                // Literal `%20` in ZIP entry names exercises `fallback_raw_path`.
                (
                    "OEBPS/plain%20cover.png",
                    &cover_png,
                    CompressionMethod::Deflated,
                ),
                ("OEBPS/secret.xhtml", b"cipher", CompressionMethod::Deflated),
                (
                    "OEBPS/my%20chapter.xhtml",
                    b"<html><body><p>Plain</p></body></html>",
                    CompressionMethod::Deflated,
                ),
            ],
        );

        let mut archive = open_epub_zip(&epub_path).expect("open zip");
        let parsed = parse_epub_package(&mut archive).expect("parse package");
        let index = build_epub_index(&archive, parsed);

        assert_eq!(index.cover_candidates.len(), 1);
        assert_eq!(
            index.cover_candidates[0].resolved_path,
            "OEBPS/plain cover.png"
        );
        assert_eq!(index.spine_chapters.len(), 2);
        assert!(index.spine_chapters[0].encrypted);
        assert!(!index.spine_chapters[1].encrypted);
        assert!(index.spine_chapters[1].zip_index.is_some());
    }

    #[test]
    fn epub_index_cache_evicts_oldest_entry_beyond_capacity_8() {
        let _guard = CACHE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let dir = TempTestDir::new("index_evict");
        let container_xml = br#"<container version="1.0"><rootfiles>
            <rootfile full-path="content.opf" media-type="application/oebps-package+xml"/>
        </rootfiles></container>"#;

        let mut keys = Vec::new();
        for i in 0..=EPUB_INDEX_CACHE_CAPACITY {
            let path = dir.path().join(format!("book_{i}.epub"));
            let opf = format!(
                "<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\">\
                 <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
                 <dc:title>Book {i}</dc:title></metadata>\
                 <manifest><item id=\"c\" href=\"c.xhtml\" media-type=\"application/xhtml+xml\"/></manifest>\
                 <spine><itemref idref=\"c\"/></spine></package>"
            );
            write_epub_entries(
                &path,
                &[
                    (
                        "META-INF/container.xml",
                        container_xml,
                        CompressionMethod::Deflated,
                    ),
                    ("content.opf", opf.as_bytes(), CompressionMethod::Deflated),
                    (
                        "c.xhtml",
                        b"<html><body><p>Body</p></body></html>",
                        CompressionMethod::Deflated,
                    ),
                ],
            );
            let (key, _, _) = get_or_build_epub_index(&path).expect("index epub");
            keys.push(key);
        }

        // The oldest entry (0) was evicted when inserting the 9th entry; entries 1..=8 remain.
        assert!(epub_cache_get(&keys[0]).is_none());
        for key in &keys[1..] {
            assert!(epub_cache_get(key).is_some());
        }
    }
}
