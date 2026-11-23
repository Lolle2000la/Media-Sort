use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use quick_xml::events::{BytesStart, Event};

use super::super::decode_xml_bytes;
use super::archive::resolve_href;
use super::xml::{attr_value_local, push_entity_ref, tolerant_reader};

#[derive(Debug, Clone)]
pub(super) struct ManifestItem {
    pub(super) resolved_path: String,
    pub(super) fallback_raw_path: Option<String>,
    pub(super) media_type: String,
    pub(super) is_cover_image: bool,
}

#[derive(Debug, Default)]
pub(super) struct EpubMetadata {
    pub(super) title: Option<String>,
    pub(super) creators: Vec<String>,
    pub(super) publisher: Option<String>,
    pub(super) language: Option<String>,
    pub(super) date: Option<String>,
    pub(super) date_is_publication: bool,
    pub(super) identifier: Option<String>,
    pub(super) identifier_is_unique: bool,
    pub(super) cover_id: Option<String>,
}

#[derive(Debug)]
pub(super) struct ParsedEpub {
    pub(super) metadata: EpubMetadata,
    pub(super) manifest: HashMap<String, ManifestItem>,
    pub(super) manifest_order: Vec<String>,
    pub(super) spine: Vec<String>,
    pub(super) encrypted_uris: HashSet<String>,
}

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

/// Parses an EPUB OPF package document into metadata, sanitized manifest items, and spine IDs.
pub(super) fn parse_opf(
    bytes: &[u8],
    opf_dir: &str,
    encrypted_uris: HashSet<String>,
) -> ParsedEpub {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_opf2_and_opf3_cover_hints_and_unique_identifier() {
        let opf2 = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:title>OPF2 Book</dc:title>
    <dc:identifier id="bookid">urn:uuid:1111</dc:identifier>
    <meta name="cover" content="cover-item"/>
  </metadata>
  <manifest>
    <item id="cover-item" href="img/cover.jpg" media-type="image/jpeg"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#;

        let parsed2 = parse_opf(opf2, "OEBPS", HashSet::new());
        assert_eq!(parsed2.metadata.title.as_deref(), Some("OPF2 Book"));
        assert_eq!(parsed2.metadata.cover_id.as_deref(), Some("cover-item"));
        assert_eq!(
            parsed2.metadata.identifier.as_deref(),
            Some("urn:uuid:1111")
        );
        assert!(parsed2.metadata.identifier_is_unique);
        assert_eq!(
            parsed2.manifest["cover-item"].resolved_path,
            "OEBPS/img/cover.jpg"
        );
        assert!(!parsed2.manifest["cover-item"].is_cover_image);

        let opf3 = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>OPF3 Book</dc:title>
    <dc:identifier id="uid">urn:uuid:3333</dc:identifier>
  </metadata>
  <manifest>
    <item id="c" href="cover.png" media-type="image/png" properties="nav cover-image"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#;

        let parsed3 = parse_opf(opf3, "", HashSet::new());
        assert_eq!(parsed3.metadata.cover_id, None);
        assert!(parsed3.manifest["c"].is_cover_image);
        assert_eq!(
            parsed3.metadata.identifier.as_deref(),
            Some("urn:uuid:3333")
        );
    }

    #[test]
    fn handles_duplicate_manifest_ids_spine_filtering_and_traversal_rejection() {
        let opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata/>
  <manifest>
    <item id="ch1" href="old.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch1" href="new.xhtml" media-type="application/xhtml+xml"/>
    <item id="evil" href="../../etc/passwd" media-type="application/xhtml+xml"/>
    <item id="ch2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
    <itemref idref="evil"/>
    <itemref idref="nonexistent"/>
    <itemref idref="ch2"/>
  </spine>
</package>"#;

        let parsed = parse_opf(opf, "OEBPS", HashSet::new());
        assert_eq!(parsed.manifest_order, vec!["ch1", "ch2"]);
        assert_eq!(parsed.manifest["ch1"].resolved_path, "OEBPS/new.xhtml");
        assert!(!parsed.manifest.contains_key("evil"));
        assert_eq!(parsed.spine, vec!["ch1", "ch2"]);
    }

    #[test]
    fn deduplicates_creators_and_prioritizes_publication_date_and_unique_or_isbn_identifier() {
        let opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="main-id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:title>  The   Great &amp;  Book </dc:title>
    <dc:creator>  Ada
      Lovelace  </dc:creator>
    <dc:creator>Ada Lovelace</dc:creator>
    <dc:creator>Charles Babbage</dc:creator>
    <dc:publisher>Acme Press</dc:publisher>
    <dc:language>en</dc:language>
    <dc:date opf:event="creation">2020-01-01</dc:date>
    <dc:date opf:event="publication">2021-06-15</dc:date>
    <dc:date opf:event="modification">2024-02-02</dc:date>
    <dc:identifier>non-unique-id</dc:identifier>
    <dc:identifier opf:scheme="ISBN">978-0-12-345678-9</dc:identifier>
    <dc:identifier id="other">secondary</dc:identifier>
  </metadata>
  <manifest/>
  <spine/>
</package>"#;

        let parsed = parse_opf(opf, "", HashSet::new());
        assert_eq!(parsed.metadata.title.as_deref(), Some("The Great & Book"));
        assert_eq!(
            parsed.metadata.creators,
            vec!["Ada Lovelace", "Charles Babbage"]
        );
        assert_eq!(parsed.metadata.publisher.as_deref(), Some("Acme Press"));
        assert_eq!(parsed.metadata.language.as_deref(), Some("en"));
        assert_eq!(parsed.metadata.date.as_deref(), Some("2021-06-15"));
        assert_eq!(
            parsed.metadata.identifier.as_deref(),
            Some("978-0-12-345678-9")
        );
    }
}
