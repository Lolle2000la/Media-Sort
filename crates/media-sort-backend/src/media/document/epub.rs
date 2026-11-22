use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use quick_xml::XmlVersion;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;
use zip::ZipArchive;
use zip::result::ZipError;

use super::comic::{ArchiveKey, INDEX_CACHE_CAPACITY, IndexLru, archive_key, lru_get, lru_insert};
use super::{
    DecodedImage, DocumentError, DocumentPreview, DocumentUnavailableReason,
    MAX_ARCHIVE_ENTRY_BYTES, MAX_XML_ENTRY_BYTES, is_image_entry, sanitize_archive_rel_path,
};
use crate::media::format_pipeline::decode_and_resize_bytes;

/// Cover preview dimensions attached to chapter 0 of an EPUB preview.
const EPUB_PREVIEW_COVER_MAX_WIDTH: u32 = 320;
const EPUB_PREVIEW_COVER_MAX_HEIGHT: u32 = 480;

static EPUB_INDEX_CACHE: OnceLock<IndexLru<EpubIndex>> = OnceLock::new();

fn epub_index_cache() -> &'static IndexLru<EpubIndex> {
    EPUB_INDEX_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(INDEX_CACHE_CAPACITY)))
}

#[derive(Debug, Clone)]
struct EpubZipTarget {
    resolved_path: String,
    zip_index: usize,
}

#[derive(Debug, Clone)]
struct EpubSpineChapter {
    resolved_path: String,
    zip_index: Option<usize>,
    encrypted: bool,
}

#[derive(Debug)]
struct EpubIndex {
    cover_candidates: Vec<EpubZipTarget>,
    metadata: BTreeMap<String, String>,
    spine_chapters: Vec<EpubSpineChapter>,
}

#[derive(Debug, Clone)]
struct ManifestItem {
    resolved_path: String,
    fallback_raw_path: Option<String>,
    media_type: String,
    is_cover_image: bool,
}

#[derive(Debug, Default)]
struct EpubMetadata {
    title: Option<String>,
    creators: Vec<String>,
    publisher: Option<String>,
    language: Option<String>,
    date: Option<String>,
    date_is_publication: bool,
    identifier: Option<String>,
    identifier_is_unique: bool,
    cover_id: Option<String>,
}

#[derive(Debug)]
struct ParsedEpub {
    metadata: EpubMetadata,
    manifest: HashMap<String, ManifestItem>,
    manifest_order: Vec<String>,
    spine: Vec<String>,
    encrypted_uris: HashSet<String>,
}

#[derive(Debug)]
enum EpubFailure {
    Encrypted,
    NotFound(String),
    TooLarge(String),
    Corrupt(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for EpubFailure {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl EpubFailure {
    fn into_document_error(self) -> DocumentError {
        match self {
            Self::Io(err) => DocumentError::Io(err),
            Self::Encrypted => DocumentError::Message("EPUB archive is encrypted".to_string()),
            Self::NotFound(msg) | Self::TooLarge(msg) | Self::Corrupt(msg) => {
                DocumentError::Message(msg)
            }
        }
    }
}

type EpubIndexLookup = (
    ArchiveKey,
    Arc<EpubIndex>,
    Option<ZipArchive<BufReader<File>>>,
);

fn epub_failure_to_preview(err: EpubFailure) -> Result<DocumentPreview, DocumentError> {
    match err {
        EpubFailure::Encrypted => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::Encrypted,
        }),
        EpubFailure::NotFound(msg) | EpubFailure::Corrupt(msg) => {
            Ok(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(msg),
            })
        }
        EpubFailure::TooLarge(_) => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::TooLarge,
        }),
        EpubFailure::Io(io_err) => Err(DocumentError::Io(io_err)),
    }
}

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

fn get_or_build_epub_index(path: &Path) -> Result<EpubIndexLookup, EpubFailure> {
    let key = archive_key(path)?;
    if let Some(cached) = lru_get(epub_index_cache(), &key) {
        return Ok((key, cached, None));
    }
    let mut archive = open_epub_zip(&key.path)?;
    let epub = parse_epub_package(&mut archive)?;
    let index = Arc::new(build_epub_index(&archive, epub));
    lru_insert(
        epub_index_cache(),
        INDEX_CACHE_CAPACITY,
        key.clone(),
        Arc::clone(&index),
    );
    Ok((key, index, Some(archive)))
}

/// Extracts and resizes the EPUB cover image to fit inside `max_width` × `max_height`.
///
/// Missing, encrypted, or unreadable covers cleanly return `Ok(None)`.
pub fn extract_epub_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    let (key, index, maybe_archive) = match get_or_build_epub_index(path) {
        Ok(res) => res,
        Err(EpubFailure::Encrypted) => return Ok(None),
        Err(err) => return Err(err.into_document_error()),
    };

    if index.cover_candidates.is_empty() {
        return Ok(None);
    }

    let mut archive = match maybe_archive {
        Some(a) => a,
        None => match open_epub_zip(&key.path) {
            Ok(a) => a,
            Err(EpubFailure::Encrypted) => return Ok(None),
            Err(err) => return Err(err.into_document_error()),
        },
    };

    Ok(try_decode_epub_cover(
        &mut archive,
        &index.cover_candidates,
        max_width,
        max_height,
    ))
}

/// Loads chapter `page_index` (clamped to the EPUB spine length) converted to Markdown,
/// plus an optional cover image (up to `320x480`) when `page_index == 0`.
pub fn load_epub_preview(path: &Path, page_index: usize) -> Result<DocumentPreview, DocumentError> {
    let (key, index, maybe_archive) = match get_or_build_epub_index(path) {
        Ok(res) => res,
        Err(err) => return epub_failure_to_preview(err),
    };

    if index.spine_chapters.is_empty() {
        return Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::UnsupportedOrCorrupt(
                "EPUB spine contains no readable chapters".to_string(),
            ),
        });
    }

    let total_chapters = index.spine_chapters.len();
    let clamped = page_index.min(total_chapters - 1);
    let chapter = &index.spine_chapters[clamped];

    if chapter.encrypted {
        return Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::Encrypted,
        });
    }

    let Some(zip_idx) = chapter.zip_index else {
        return Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::UnsupportedOrCorrupt(format!(
                "Missing EPUB entry '{}'",
                chapter.resolved_path
            )),
        });
    };

    let mut archive = match maybe_archive {
        Some(a) => a,
        None => match open_epub_zip(&key.path) {
            Ok(a) => a,
            Err(err) => return epub_failure_to_preview(err),
        },
    };

    let chapter_bytes = match read_zip_index_capped(
        &mut archive,
        zip_idx,
        &chapter.resolved_path,
        MAX_XML_ENTRY_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(err) => return epub_failure_to_preview(err),
    };

    let markdown = xhtml_to_markdown(&chapter_bytes);
    let cover = if page_index == 0 && !index.cover_candidates.is_empty() {
        try_decode_epub_cover(
            &mut archive,
            &index.cover_candidates,
            EPUB_PREVIEW_COVER_MAX_WIDTH,
            EPUB_PREVIEW_COVER_MAX_HEIGHT,
        )
    } else {
        None
    };

    Ok(DocumentPreview::Markdown {
        markdown,
        chapter_index: Some(clamped),
        total_chapters: Some(total_chapters),
        cover,
    })
}

/// Extracts EPUB package metadata (`Chapters`, `Title`, `Author`, `Publisher`, `Language`,
/// `Date`, `Identifier`) for the metadata inspector.
pub fn extract_epub_metadata(path: &Path) -> Result<BTreeMap<String, String>, DocumentError> {
    match get_or_build_epub_index(path) {
        Ok((_, index, _)) => Ok(index.metadata.clone()),
        Err(EpubFailure::Encrypted) => {
            let mut fields = BTreeMap::new();
            fields.insert("Encrypted".into(), "Yes".into());
            Ok(fields)
        }
        Err(err) => Err(err.into_document_error()),
    }
}

// ── ZIP & package resolution helpers ──────────────────────────────────

fn open_epub_zip(path: &Path) -> Result<ZipArchive<BufReader<File>>, EpubFailure> {
    let file = File::open(path)?;
    ZipArchive::new(BufReader::new(file)).map_err(map_zip_error)
}

fn map_zip_error(err: ZipError) -> EpubFailure {
    match err {
        ZipError::Io(io_err) => EpubFailure::Io(io_err),
        ZipError::InvalidPassword => EpubFailure::Encrypted,
        ZipError::FileNotFound => EpubFailure::NotFound("Entry not found in EPUB".to_string()),
        ZipError::UnsupportedArchive(msg)
            if msg == ZipError::PASSWORD_REQUIRED
                || msg.to_ascii_lowercase().contains("password")
                || msg.to_ascii_lowercase().contains("encrypt") =>
        {
            EpubFailure::Encrypted
        }
        other => EpubFailure::Corrupt(format!("Failed to read EPUB archive: {other}")),
    }
}

fn find_zip_entry_index(archive: &ZipArchive<BufReader<File>>, target: &str) -> Option<usize> {
    if let Some(idx) = archive.index_for_name(target) {
        return Some(idx);
    }
    let normalized_target = target.trim_start_matches("./");
    let mut ci_match = None;
    for (idx, name_res) in archive.file_names().enumerate() {
        let Ok(name) = name_res else {
            continue;
        };
        let norm = name.replace('\\', "/");
        let trimmed = norm.trim_start_matches("./");
        if trimmed == normalized_target {
            return Some(idx);
        }
        if ci_match.is_none() && trimmed.eq_ignore_ascii_case(normalized_target) {
            ci_match = Some(idx);
        }
    }
    ci_match
}

fn read_zip_index_capped(
    archive: &mut ZipArchive<BufReader<File>>,
    idx: usize,
    entry_path: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, EpubFailure> {
    let meta = archive.by_index_data(idx).map_err(map_zip_error)?;
    if meta.encrypted() {
        return Err(EpubFailure::Encrypted);
    }
    if meta.size() > max_bytes {
        return Err(EpubFailure::TooLarge(format!(
            "EPUB entry '{entry_path}' exceeds size limit ({} > {max_bytes} bytes)",
            meta.size()
        )));
    }
    let prealloc = usize::try_from(meta.size()).unwrap_or(0).min(1024 * 1024);
    drop(meta);

    let file = archive.by_index(idx).map_err(map_zip_error)?;
    if file.encrypted() {
        return Err(EpubFailure::Encrypted);
    }
    let mut buf = Vec::with_capacity(prealloc);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > max_bytes {
        return Err(EpubFailure::TooLarge(format!(
            "EPUB entry '{entry_path}' decompressed size exceeded {max_bytes} bytes"
        )));
    }
    Ok(buf)
}

fn read_zip_path_capped(
    archive: &mut ZipArchive<BufReader<File>>,
    entry_path: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, EpubFailure> {
    let Some(idx) = find_zip_entry_index(archive, entry_path) else {
        return Err(EpubFailure::NotFound(format!(
            "Missing EPUB entry '{entry_path}'"
        )));
    };
    read_zip_index_capped(archive, idx, entry_path, max_bytes)
}

fn parse_epub_package(
    archive: &mut ZipArchive<BufReader<File>>,
) -> Result<ParsedEpub, EpubFailure> {
    let container_bytes =
        read_zip_path_capped(archive, "META-INF/container.xml", MAX_XML_ENTRY_BYTES)?;
    let (opf_path, fallback_opf_path) = parse_container_opf_path(&container_bytes)?;

    let encrypted_uris =
        match read_zip_path_capped(archive, "META-INF/encryption.xml", MAX_XML_ENTRY_BYTES) {
            Ok(bytes) => parse_encrypted_uris(&bytes),
            Err(EpubFailure::NotFound(_)) => HashSet::new(),
            Err(EpubFailure::Encrypted) => return Err(EpubFailure::Encrypted),
            Err(_) => HashSet::new(),
        };

    if encrypted_uris.contains(&opf_path) {
        return Err(EpubFailure::Encrypted);
    }

    let (resolved_opf_path, opf_bytes) =
        match read_zip_path_capped(archive, &opf_path, MAX_XML_ENTRY_BYTES) {
            Ok(bytes) => (opf_path, bytes),
            Err(err @ EpubFailure::NotFound(_)) => {
                let Some(raw) = fallback_opf_path else {
                    return Err(err);
                };
                if encrypted_uris.contains(&raw) {
                    return Err(EpubFailure::Encrypted);
                }
                let bytes = read_zip_path_capped(archive, &raw, MAX_XML_ENTRY_BYTES)?;
                (raw, bytes)
            }
            Err(err) => return Err(err),
        };

    let opf_dir = resolved_opf_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or("");

    Ok(parse_opf(&opf_bytes, opf_dir, encrypted_uris))
}

fn percent_decode_path(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            let decoded = (hi << 4) | lo;
            if decoded == 0 {
                return None;
            }
            out.push(decoded);
            i += 3;
            continue;
        }
        if bytes[i] == 0 {
            return None;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn resolve_href(base_dir: &str, href: &str) -> (Option<String>, Option<String>) {
    let decoded = percent_decode_path(href).and_then(|d| sanitize_archive_rel_path(base_dir, &d));
    let raw = sanitize_archive_rel_path(base_dir, href);
    match (decoded, raw) {
        (Some(d), Some(r)) if d != r => (Some(d), Some(r)),
        (Some(d), _) => (Some(d), None),
        (None, Some(r)) => (Some(r), None),
        (None, None) => (None, None),
    }
}

// ── XML helpers ───────────────────────────────────────────────────────

fn decode_xml_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c));
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c));
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn tolerant_reader(xml: &str) -> Reader<&[u8]> {
    let mut reader = Reader::from_str(xml);
    let cfg = reader.config_mut();
    cfg.check_end_names = false;
    cfg.allow_dangling_amp = true;
    cfg.allow_unmatched_ends = true;
    reader
}

fn attr_value_local(start: &BytesStart<'_>, name: &str) -> Option<String> {
    for attr_res in start.attributes().with_checks(false) {
        let Ok(attr) = attr_res else {
            continue;
        };
        if attr
            .key
            .local_name()
            .into_inner()
            .eq_ignore_ascii_case(name)
        {
            let val = match attr.normalized_value(XmlVersion::Implicit1_0) {
                Ok(v) => v.into_owned(),
                Err(_) => quick_xml::escape::unescape_with(&attr.value, resolve_html_entity)
                    .map(|v| v.into_owned())
                    .unwrap_or_else(|_| attr.value.into_owned()),
            };
            return Some(val);
        }
    }
    None
}

fn parse_container_opf_path(bytes: &[u8]) -> Result<(String, Option<String>), EpubFailure> {
    let xml = decode_xml_bytes(bytes);
    let mut reader = tolerant_reader(&xml);
    let mut first_candidate: Option<(String, Option<String>)> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.local_name().into_inner().eq_ignore_ascii_case("rootfile")
                    && let Some(full_path) = attr_value_local(&e, "full-path")
                {
                    let (resolved, fallback) = resolve_href("", &full_path);
                    if let Some(path) = resolved {
                        let is_opf = attr_value_local(&e, "media-type").is_some_and(|m| {
                            m.eq_ignore_ascii_case("application/oebps-package+xml")
                        });
                        if is_opf {
                            return Ok((path, fallback));
                        }
                        if first_candidate.is_none() {
                            first_candidate = Some((path, fallback));
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(quick_xml::Error::IllFormed(_)) => {}
            Err(_) => break,
            _ => {}
        }
    }

    first_candidate.ok_or_else(|| {
        EpubFailure::Corrupt("EPUB container.xml contains no valid OPF rootfile".to_string())
    })
}

fn is_font_obfuscation_algorithm(alg: &str) -> bool {
    alg == "http://www.idpf.org/2008/embedding"
        || alg == "http://ns.adobe.com/pdf/enc#RC"
        || alg.ends_with("#embedding")
}

fn parse_encrypted_uris(bytes: &[u8]) -> HashSet<String> {
    let xml = decode_xml_bytes(bytes);
    let mut reader = tolerant_reader(&xml);
    let mut encrypted = HashSet::new();
    let mut in_encrypted_data = false;
    let mut current_alg: Option<String> = None;
    let mut current_uris: Vec<String> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = e.local_name().into_inner();
                if local.eq_ignore_ascii_case("EncryptedData") {
                    in_encrypted_data = true;
                    current_alg = None;
                    current_uris.clear();
                } else if in_encrypted_data {
                    if local.eq_ignore_ascii_case("EncryptionMethod") {
                        current_alg = attr_value_local(&e, "Algorithm");
                    } else if local.eq_ignore_ascii_case("CipherReference")
                        && let Some(uri) = attr_value_local(&e, "URI")
                    {
                        current_uris.push(uri);
                    }
                }
            }
            Ok(Event::Empty(e)) if in_encrypted_data => {
                let local = e.local_name().into_inner();
                if local.eq_ignore_ascii_case("EncryptionMethod") {
                    current_alg = attr_value_local(&e, "Algorithm");
                } else if local.eq_ignore_ascii_case("CipherReference")
                    && let Some(uri) = attr_value_local(&e, "URI")
                {
                    current_uris.push(uri);
                }
            }
            Ok(Event::End(e)) => {
                if e.local_name()
                    .into_inner()
                    .eq_ignore_ascii_case("EncryptedData")
                    && in_encrypted_data
                {
                    let is_obfuscation = current_alg
                        .as_deref()
                        .is_some_and(is_font_obfuscation_algorithm);
                    if !is_obfuscation {
                        for uri in current_uris.drain(..) {
                            let (resolved, fallback) = resolve_href("", &uri);
                            if let Some(p) = resolved {
                                encrypted.insert(p);
                            }
                            if let Some(p) = fallback {
                                encrypted.insert(p);
                            }
                        }
                    }
                    in_encrypted_data = false;
                    current_alg = None;
                    current_uris.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(quick_xml::Error::IllFormed(_)) => {}
            Err(_) => break,
            _ => {}
        }
    }

    encrypted
}

// ── OPF parser ────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpfSection {
    None,
    Metadata,
    Manifest,
    Spine,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetadataFieldKind {
    Title,
    Creator,
    Publisher,
    Language,
    Date { is_publication: bool },
    Identifier { is_unique: bool },
}

fn parse_opf(bytes: &[u8], opf_dir: &str, encrypted_uris: HashSet<String>) -> ParsedEpub {
    let xml = decode_xml_bytes(bytes);
    let mut reader = tolerant_reader(&xml);

    let mut metadata = EpubMetadata::default();
    let mut manifest: HashMap<String, ManifestItem> = HashMap::new();
    let mut manifest_order: Vec<String> = Vec::new();
    let mut spine_idrefs: Vec<String> = Vec::new();

    let mut unique_id_attr: Option<String> = None;
    let mut section = OpfSection::None;
    let mut depth: usize = 0;
    let mut section_depth: usize = 0;
    let mut active_meta: Option<(MetadataFieldKind, usize)> = None;
    let mut text_buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                depth += 1;
                let local = e.local_name().into_inner().to_ascii_lowercase();
                match local.as_str() {
                    "package" if depth == 1 => {
                        unique_id_attr = attr_value_local(&e, "unique-identifier");
                    }
                    "metadata" if section == OpfSection::None => {
                        section = OpfSection::Metadata;
                        section_depth = depth;
                    }
                    "manifest" if section == OpfSection::None => {
                        section = OpfSection::Manifest;
                        section_depth = depth;
                    }
                    "spine" if section == OpfSection::None => {
                        section = OpfSection::Spine;
                        section_depth = depth;
                    }
                    "meta" if section == OpfSection::Metadata => {
                        handle_opf_meta_attrs(&e, &mut metadata);
                    }
                    "item" if section == OpfSection::Manifest => {
                        handle_opf_manifest_item(&e, opf_dir, &mut manifest, &mut manifest_order);
                    }
                    "itemref" if section == OpfSection::Spine => {
                        if let Some(idref) = attr_value_local(&e, "idref")
                            && !idref.is_empty()
                        {
                            spine_idrefs.push(idref);
                        }
                    }
                    _ if section == OpfSection::Metadata && active_meta.is_none() => {
                        if let Some(kind) = classify_dc_field(&local, &e, unique_id_attr.as_deref())
                        {
                            active_meta = Some((kind, depth));
                            text_buf.clear();
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let local = e.local_name().into_inner().to_ascii_lowercase();
                match (section, local.as_str()) {
                    (OpfSection::Metadata, "meta") => {
                        handle_opf_meta_attrs(&e, &mut metadata);
                    }
                    (OpfSection::Manifest, "item") => {
                        handle_opf_manifest_item(&e, opf_dir, &mut manifest, &mut manifest_order);
                    }
                    (OpfSection::Spine, "itemref") => {
                        if let Some(idref) = attr_value_local(&e, "idref")
                            && !idref.is_empty()
                        {
                            spine_idrefs.push(idref);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                if let Some((kind, field_depth)) = active_meta
                    && depth == field_depth
                {
                    commit_dc_field(kind, &text_buf, &mut metadata);
                    active_meta = None;
                    text_buf.clear();
                }
                if section != OpfSection::None && depth == section_depth {
                    section = OpfSection::None;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(t)) => {
                if active_meta.is_some() {
                    text_buf.push_str(&t.xml10_content());
                }
            }
            Ok(Event::CData(c)) => {
                if active_meta.is_some() {
                    text_buf.push_str(&c.xml10_content());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if active_meta.is_some() {
                    push_entity_ref(&mut text_buf, &r);
                }
            }
            Ok(Event::Eof) => break,
            Err(quick_xml::Error::IllFormed(_)) => {}
            Err(_) => break,
            _ => {}
        }
    }

    let spine = spine_idrefs
        .into_iter()
        .filter(|idref| manifest.contains_key(idref))
        .collect();

    ParsedEpub {
        metadata,
        manifest,
        manifest_order,
        spine,
        encrypted_uris,
    }
}

fn handle_opf_meta_attrs(e: &BytesStart<'_>, metadata: &mut EpubMetadata) {
    if let Some(name) = attr_value_local(e, "name")
        && name.eq_ignore_ascii_case("cover")
        && let Some(content) = attr_value_local(e, "content")
    {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            metadata.cover_id = Some(trimmed.to_string());
        }
    }
}

fn handle_opf_manifest_item(
    e: &BytesStart<'_>,
    opf_dir: &str,
    manifest: &mut HashMap<String, ManifestItem>,
    manifest_order: &mut Vec<String>,
) {
    let Some(id) = attr_value_local(e, "id").map(|s| s.trim().to_string()) else {
        return;
    };
    let Some(href) = attr_value_local(e, "href") else {
        return;
    };
    if id.is_empty() {
        return;
    }
    let (Some(resolved_path), fallback_raw_path) = resolve_href(opf_dir, &href) else {
        return;
    };
    let media_type = attr_value_local(e, "media-type")
        .unwrap_or_default()
        .trim()
        .to_string();
    let is_cover_image = attr_value_local(e, "properties")
        .is_some_and(|props| props.split_whitespace().any(|tok| tok == "cover-image"));

    let item = ManifestItem {
        resolved_path,
        fallback_raw_path,
        media_type,
        is_cover_image,
    };
    match manifest.entry(id) {
        Entry::Vacant(v) => {
            manifest_order.push(v.key().clone());
            v.insert(item);
        }
        Entry::Occupied(mut o) => {
            o.insert(item);
        }
    }
}

fn classify_dc_field(
    local: &str,
    e: &BytesStart<'_>,
    unique_id_attr: Option<&str>,
) -> Option<MetadataFieldKind> {
    match local {
        "title" => Some(MetadataFieldKind::Title),
        "creator" => Some(MetadataFieldKind::Creator),
        "publisher" => Some(MetadataFieldKind::Publisher),
        "language" => Some(MetadataFieldKind::Language),
        "date" => {
            let is_publication = attr_value_local(e, "event")
                .is_some_and(|ev| ev.eq_ignore_ascii_case("publication"));
            Some(MetadataFieldKind::Date { is_publication })
        }
        "identifier" => {
            let matches_unique = unique_id_attr
                .is_some_and(|uid| attr_value_local(e, "id").is_some_and(|id| id == uid));
            let matches_isbn =
                attr_value_local(e, "scheme").is_some_and(|s| s.eq_ignore_ascii_case("isbn"));
            Some(MetadataFieldKind::Identifier {
                is_unique: matches_unique || matches_isbn,
            })
        }
        _ => None,
    }
}

fn collapse_inline_ws(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for word in raw.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

fn commit_dc_field(kind: MetadataFieldKind, raw: &str, metadata: &mut EpubMetadata) {
    let cleaned = collapse_inline_ws(raw);
    if cleaned.is_empty() {
        return;
    }
    match kind {
        MetadataFieldKind::Title => {
            if metadata.title.is_none() {
                metadata.title = Some(cleaned);
            }
        }
        MetadataFieldKind::Creator => {
            if !metadata.creators.iter().any(|c| c == &cleaned) {
                metadata.creators.push(cleaned);
            }
        }
        MetadataFieldKind::Publisher => {
            if metadata.publisher.is_none() {
                metadata.publisher = Some(cleaned);
            }
        }
        MetadataFieldKind::Language => {
            if metadata.language.is_none() {
                metadata.language = Some(cleaned);
            }
        }
        MetadataFieldKind::Date { is_publication } => {
            if metadata.date.is_none() || (is_publication && !metadata.date_is_publication) {
                metadata.date = Some(cleaned);
                metadata.date_is_publication = is_publication;
            }
        }
        MetadataFieldKind::Identifier { is_unique } => {
            if metadata.identifier.is_none() || (is_unique && !metadata.identifier_is_unique) {
                metadata.identifier = Some(cleaned);
                metadata.identifier_is_unique = is_unique;
            }
        }
    }
}

// ── Cover selection & decode ──────────────────────────────────────────

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

fn collect_cover_candidates(epub: &ParsedEpub) -> Vec<&ManifestItem> {
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

fn try_decode_epub_cover(
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

// ── XHTML / HTML to Markdown converter ────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum InlineKind {
    Bold,
    Italic,
    Code,
}

impl InlineKind {
    fn marker(self) -> &'static str {
        match self {
            Self::Bold => "**",
            Self::Italic => "*",
            Self::Code => "`",
        }
    }
}

struct InlineSpan {
    kind: InlineKind,
    emitted: bool,
}

struct MarkdownBuilder {
    out: String,
    pending_space: bool,
    at_line_start: bool,
    just_started_list_item: bool,
    blockquote_depth: usize,
    list_depth: usize,
    pre_depth: usize,
    bold_depth: usize,
    italic_depth: usize,
    code_depth: usize,
    active_inlines: Vec<InlineSpan>,
}

impl MarkdownBuilder {
    fn new() -> Self {
        Self {
            out: String::new(),
            pending_space: false,
            at_line_start: true,
            just_started_list_item: false,
            blockquote_depth: 0,
            list_depth: 0,
            pre_depth: 0,
            bold_depth: 0,
            italic_depth: 0,
            code_depth: 0,
            active_inlines: Vec::new(),
        }
    }

    fn flush_line_prefix_if_needed(&mut self) {
        if self.at_line_start && self.blockquote_depth > 0 && self.pre_depth == 0 {
            for _ in 0..self.blockquote_depth {
                self.out.push_str("> ");
            }
            self.at_line_start = false;
        }
    }

    fn flush_before_content(&mut self) {
        self.flush_line_prefix_if_needed();
        if self.pending_space {
            if !self.at_line_start && !self.out.ends_with([' ', '\n']) {
                self.out.push(' ');
            }
            self.pending_space = false;
        }
        for span in &mut self.active_inlines {
            if !span.emitted {
                self.out.push_str(span.kind.marker());
                span.emitted = true;
                self.at_line_start = false;
            }
        }
        self.just_started_list_item = false;
    }

    fn close_all_inlines(&mut self) {
        while let Some(span) = self.active_inlines.pop() {
            if span.emitted {
                self.out.push_str(span.kind.marker());
            }
        }
        self.bold_depth = 0;
        self.italic_depth = 0;
        self.code_depth = 0;
    }

    fn push_inline_start(&mut self, kind: InlineKind) {
        if self.pre_depth > 0 {
            return;
        }
        let depth = match kind {
            InlineKind::Bold => {
                self.bold_depth += 1;
                self.bold_depth
            }
            InlineKind::Italic => {
                self.italic_depth += 1;
                self.italic_depth
            }
            InlineKind::Code => {
                self.code_depth += 1;
                self.code_depth
            }
        };
        if depth == 1 {
            self.active_inlines.push(InlineSpan {
                kind,
                emitted: false,
            });
        }
    }

    fn push_inline_end(&mut self, kind: InlineKind) {
        if self.pre_depth > 0 {
            return;
        }
        let should_close = match kind {
            InlineKind::Bold => {
                self.bold_depth = self.bold_depth.saturating_sub(1);
                self.bold_depth == 0
            }
            InlineKind::Italic => {
                self.italic_depth = self.italic_depth.saturating_sub(1);
                self.italic_depth == 0
            }
            InlineKind::Code => {
                self.code_depth = self.code_depth.saturating_sub(1);
                self.code_depth == 0
            }
        };
        if !should_close {
            return;
        }
        if let Some(pos) = self.active_inlines.iter().rposition(|s| s.kind == kind) {
            let span = self.active_inlines.remove(pos);
            if span.emitted {
                self.out.push_str(span.kind.marker());
            }
        }
    }

    fn ensure_newlines(&mut self, count: usize) {
        self.pending_space = false;
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        if self.out.is_empty() {
            self.at_line_start = true;
            return;
        }
        let trailing_newlines = self.out.bytes().rev().take_while(|&b| b == b'\n').count();
        for _ in trailing_newlines..count {
            self.out.push('\n');
        }
        self.at_line_start = true;
    }

    fn push_block_break(&mut self) {
        self.close_all_inlines();
        if self.just_started_list_item {
            return;
        }
        self.ensure_newlines(2);
    }

    fn push_text(&mut self, text: &str) {
        if self.pre_depth > 0 {
            let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            if !normalized.is_empty() {
                self.out.push_str(&normalized);
                self.at_line_start = normalized.ends_with('\n');
                self.just_started_list_item = false;
            }
            return;
        }

        for ch in text.chars() {
            if ch.is_whitespace() || ch == '\u{00A0}' {
                if !self.at_line_start && !self.just_started_list_item {
                    self.pending_space = true;
                }
            } else {
                self.flush_before_content();
                self.out.push(ch);
                self.at_line_start = false;
            }
        }
    }

    fn start_heading(&mut self, level: usize) {
        self.close_all_inlines();
        if !self.just_started_list_item {
            self.ensure_newlines(2);
        }
        self.flush_line_prefix_if_needed();
        for _ in 0..level {
            self.out.push('#');
        }
        self.out.push(' ');
        self.at_line_start = false;
        self.pending_space = false;
        self.just_started_list_item = false;
    }

    fn start_list_item(&mut self) {
        self.close_all_inlines();
        self.ensure_newlines(1);
        self.flush_line_prefix_if_needed();
        let indent = self.list_depth.saturating_sub(1);
        for _ in 0..indent {
            self.out.push_str("  ");
        }
        self.out.push_str("- ");
        self.at_line_start = false;
        self.pending_space = false;
        self.just_started_list_item = true;
    }

    fn end_list_item(&mut self) {
        self.close_all_inlines();
        self.just_started_list_item = false;
        self.ensure_newlines(1);
    }

    fn push_br(&mut self) {
        if self.pre_depth > 0 {
            self.out.push('\n');
            self.at_line_start = true;
            return;
        }
        self.pending_space = false;
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        self.out.push('\n');
        self.at_line_start = true;
        self.just_started_list_item = false;
    }

    fn push_hr(&mut self) {
        self.close_all_inlines();
        self.just_started_list_item = false;
        self.ensure_newlines(1);
        self.out.push_str("---\n");
        self.at_line_start = true;
        self.pending_space = false;
    }

    fn start_pre(&mut self) {
        self.close_all_inlines();
        self.just_started_list_item = false;
        if self.pre_depth == 0 {
            self.ensure_newlines(2);
            self.out.push_str("```\n");
            self.at_line_start = true;
            self.pending_space = false;
        }
        self.pre_depth += 1;
    }

    fn end_pre(&mut self) {
        if self.pre_depth == 0 {
            return;
        }
        self.pre_depth -= 1;
        if self.pre_depth == 0 {
            if !self.out.ends_with('\n') {
                self.out.push('\n');
            }
            self.out.push_str("```\n\n");
            self.at_line_start = true;
            self.pending_space = false;
        }
    }

    fn finish(mut self) -> String {
        self.close_all_inlines();
        if self.pre_depth > 0 {
            self.pre_depth = 1;
            self.end_pre();
        }
        normalize_markdown(&self.out)
    }
}

fn normalize_markdown(raw: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut empty_run = 0usize;

    for raw_line in raw.split('\n') {
        let trimmed_end = if in_fence {
            raw_line.trim_end_matches('\r')
        } else {
            raw_line.trim_end()
        };

        if trimmed_end.starts_with("```") {
            in_fence = !in_fence;
            empty_run = 0;
            lines.push(trimmed_end);
            continue;
        }

        if in_fence {
            lines.push(trimmed_end);
        } else if trimmed_end.is_empty() {
            empty_run += 1;
            if empty_run <= 1 && !lines.is_empty() {
                lines.push("");
            }
        } else {
            empty_run = 0;
            lines.push(trimmed_end);
        }
    }

    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[derive(Default)]
struct IgnoredSubtrees {
    head: usize,
    script: usize,
    style: usize,
    title: usize,
    noscript: usize,
}

impl IgnoredSubtrees {
    fn is_ignored(&self) -> bool {
        self.head > 0 || self.script > 0 || self.style > 0 || self.title > 0 || self.noscript > 0
    }
}

fn xhtml_to_markdown(bytes: &[u8]) -> String {
    let xml = decode_xml_bytes(bytes);
    let mut reader = tolerant_reader(&xml);
    let mut md = MarkdownBuilder::new();
    let mut ignored = IgnoredSubtrees::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let tag = e.local_name().into_inner().to_ascii_lowercase();
                match tag.as_str() {
                    "head" => {
                        ignored.head += 1;
                        continue;
                    }
                    "script" => {
                        ignored.script += 1;
                        continue;
                    }
                    "style" => {
                        ignored.style += 1;
                        continue;
                    }
                    "title" => {
                        ignored.title += 1;
                        continue;
                    }
                    "noscript" => {
                        ignored.noscript += 1;
                        continue;
                    }
                    "body" => {
                        // Recover cleanly if malformed HTML left `<head>` unclosed.
                        ignored.head = 0;
                        ignored.title = 0;
                        continue;
                    }
                    _ => {}
                }

                if ignored.is_ignored() {
                    continue;
                }

                match tag.as_str() {
                    "h1" => md.start_heading(1),
                    "h2" => md.start_heading(2),
                    "h3" => md.start_heading(3),
                    "h4" => md.start_heading(4),
                    "h5" => md.start_heading(5),
                    "h6" => md.start_heading(6),
                    "p" | "div" | "section" | "article" | "header" | "footer" | "aside"
                    | "main" | "nav" | "figure" | "figcaption" | "tr" => md.push_block_break(),
                    "br" => md.push_br(),
                    "hr" => md.push_hr(),
                    "ul" | "ol" => {
                        md.push_block_break();
                        md.list_depth += 1;
                    }
                    "li" => md.start_list_item(),
                    "blockquote" => {
                        md.push_block_break();
                        md.blockquote_depth += 1;
                    }
                    "pre" => md.start_pre(),
                    "code" => md.push_inline_start(InlineKind::Code),
                    "b" | "strong" => md.push_inline_start(InlineKind::Bold),
                    "i" | "em" => md.push_inline_start(InlineKind::Italic),
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                if ignored.is_ignored() {
                    continue;
                }
                let tag = e.local_name().into_inner().to_ascii_lowercase();
                match tag.as_str() {
                    "br" => md.push_br(),
                    "hr" => md.push_hr(),
                    "p" | "div" => md.push_block_break(),
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let tag = e.local_name().into_inner().to_ascii_lowercase();
                match tag.as_str() {
                    "head" => {
                        ignored.head = ignored.head.saturating_sub(1);
                        continue;
                    }
                    "script" => {
                        ignored.script = ignored.script.saturating_sub(1);
                        continue;
                    }
                    "style" => {
                        ignored.style = ignored.style.saturating_sub(1);
                        continue;
                    }
                    "title" => {
                        ignored.title = ignored.title.saturating_sub(1);
                        continue;
                    }
                    "noscript" => {
                        ignored.noscript = ignored.noscript.saturating_sub(1);
                        continue;
                    }
                    _ => {}
                }

                if ignored.is_ignored() {
                    continue;
                }

                match tag.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "section"
                    | "article" | "header" | "footer" | "aside" | "main" | "nav" | "figure"
                    | "figcaption" | "tr" => md.push_block_break(),
                    "ul" | "ol" => {
                        md.list_depth = md.list_depth.saturating_sub(1);
                        md.push_block_break();
                    }
                    "li" => md.end_list_item(),
                    "blockquote" => {
                        md.close_all_inlines();
                        md.blockquote_depth = md.blockquote_depth.saturating_sub(1);
                        md.ensure_newlines(2);
                    }
                    "pre" => md.end_pre(),
                    "code" => md.push_inline_end(InlineKind::Code),
                    "b" | "strong" => md.push_inline_end(InlineKind::Bold),
                    "i" | "em" => md.push_inline_end(InlineKind::Italic),
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if !ignored.is_ignored() {
                    md.push_text(&t.html_content());
                }
            }
            Ok(Event::CData(c)) => {
                if !ignored.is_ignored() {
                    md.push_text(&c.html_content());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if !ignored.is_ignored() {
                    let mut decoded = String::new();
                    push_entity_ref(&mut decoded, &r);
                    md.push_text(&decoded);
                }
            }
            Ok(Event::Eof) => break,
            Err(quick_xml::Error::IllFormed(_)) => {}
            Err(_) => break,
            _ => {}
        }
    }

    md.finish()
}

fn push_entity_ref(out: &mut String, r: &BytesRef<'_>) {
    if let Ok(Some(ch)) = r.resolve_char_ref() {
        out.push(ch);
    } else if let Some(s) = resolve_html_entity(r.as_ref()) {
        out.push_str(s);
    } else {
        out.push('&');
        out.push_str(r.as_ref());
        out.push(';');
    }
}

fn resolve_html_entity(entity: &str) -> Option<&'static str> {
    if let Some(xml) = quick_xml::escape::resolve_xml_entity(entity) {
        return Some(xml);
    }
    let resolved = match entity {
        "nbsp" => "\u{00A0}",
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "thinsp" => "\u{2009}",
        "shy" => "",
        "zwj" => "\u{200D}",
        "zwnj" => "\u{200C}",
        "ndash" => "\u{2013}",
        "mdash" => "\u{2014}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "sbquo" => "\u{201A}",
        "ldquo" => "\u{201C}",
        "rdquo" => "\u{201D}",
        "bdquo" => "\u{201E}",
        "laquo" => "\u{00AB}",
        "raquo" => "\u{00BB}",
        "lsaquo" => "\u{2039}",
        "rsaquo" => "\u{203A}",
        "hellip" => "\u{2026}",
        "bull" => "\u{2022}",
        "middot" => "\u{00B7}",
        "copy" => "\u{00A9}",
        "reg" => "\u{00AE}",
        "trade" => "\u{2122}",
        "deg" => "\u{00B0}",
        "sect" => "\u{00A7}",
        "para" => "\u{00B6}",
        "dagger" => "\u{2020}",
        "Dagger" => "\u{2021}",
        "times" => "\u{00D7}",
        "divide" => "\u{00F7}",
        "plusmn" => "\u{00B1}",
        "minus" => "\u{2212}",
        "ne" => "\u{2260}",
        "le" => "\u{2264}",
        "ge" => "\u{2265}",
        "approx" => "\u{2248}",
        "infin" => "\u{221E}",
        "euro" => "\u{20AC}",
        "pound" => "\u{00A3}",
        "yen" => "\u{00A5}",
        "cent" => "\u{00A2}",
        "frac12" => "\u{00BD}",
        "frac14" => "\u{00BC}",
        "frac34" => "\u{00BE}",
        "iexcl" => "\u{00A1}",
        "iquest" => "\u{00BF}",
        "Agrave" => "\u{00C0}",
        "Aacute" => "\u{00C1}",
        "Acirc" => "\u{00C2}",
        "Atilde" => "\u{00C3}",
        "Auml" => "\u{00C4}",
        "Aring" => "\u{00C5}",
        "AElig" => "\u{00C6}",
        "Ccedil" => "\u{00C7}",
        "Egrave" => "\u{00C8}",
        "Eacute" => "\u{00C9}",
        "Ecirc" => "\u{00CA}",
        "Euml" => "\u{00CB}",
        "Igrave" => "\u{00CC}",
        "Iacute" => "\u{00CD}",
        "Icirc" => "\u{00CE}",
        "Iuml" => "\u{00CF}",
        "Ntilde" => "\u{00D1}",
        "Ograve" => "\u{00D2}",
        "Oacute" => "\u{00D3}",
        "Ocirc" => "\u{00D4}",
        "Otilde" => "\u{00D5}",
        "Ouml" => "\u{00D6}",
        "Oslash" => "\u{00D8}",
        "Ugrave" => "\u{00D9}",
        "Uacute" => "\u{00DA}",
        "Ucirc" => "\u{00DB}",
        "Uuml" => "\u{00DC}",
        "szlig" => "\u{00DF}",
        "agrave" => "\u{00E0}",
        "aacute" => "\u{00E1}",
        "acirc" => "\u{00E2}",
        "atilde" => "\u{00E3}",
        "auml" => "\u{00E4}",
        "aring" => "\u{00E5}",
        "aelig" => "\u{00E6}",
        "ccedil" => "\u{00E7}",
        "egrave" => "\u{00E8}",
        "eacute" => "\u{00E9}",
        "ecirc" => "\u{00EA}",
        "euml" => "\u{00EB}",
        "igrave" => "\u{00EC}",
        "iacute" => "\u{00ED}",
        "icirc" => "\u{00EE}",
        "iuml" => "\u{00EF}",
        "ntilde" => "\u{00F1}",
        "ograve" => "\u{00F2}",
        "oacute" => "\u{00F3}",
        "ocirc" => "\u{00F4}",
        "otilde" => "\u{00F5}",
        "ouml" => "\u{00F6}",
        "oslash" => "\u{00F8}",
        "ugrave" => "\u{00F9}",
        "uacute" => "\u{00FA}",
        "ucirc" => "\u{00FB}",
        "uuml" => "\u{00FC}",
        "yuml" => "\u{00FF}",
        _ => return None,
    };
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use std::io::{BufWriter, Cursor, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use zip::CompressionMethod;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempTestDir(PathBuf);

    impl TempTestDir {
        fn new(prefix: &str) -> Self {
            let id = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "media-sort-epub-{prefix}-{}-{id}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn solid_png_bytes(r: u8, g: u8, b: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([r, g, b, 255]));
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, image::ImageFormat::Png)
            .expect("encode solid PNG");
        cursor.into_inner()
    }

    fn write_epub_entries(path: &Path, entries: &[(&str, &[u8], CompressionMethod)]) {
        let file = File::create(path).expect("create epub");
        let mut writer = ZipWriter::new(BufWriter::new(file));
        for &(name, data, method) in entries {
            let opts = SimpleFileOptions::default().compression_method(method);
            writer.start_file(name, opts).expect("start epub entry");
            writer.write_all(data).expect("write epub entry");
        }
        writer
            .finish()
            .expect("finish epub")
            .into_inner()
            .expect("flush epub");
    }

    #[test]
    fn epub_index_cache_hits_on_chapter_turns_and_metadata() {
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

        let key = archive_key(&epub_path).expect("epub key");
        assert!(lru_get(epub_index_cache(), &key).is_none());

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

        let cached = lru_get(epub_index_cache(), &key).expect("epub cache populated");
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
}
