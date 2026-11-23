use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use zip::ZipArchive;

use super::archive::{extension_lower, is_cfb_file, is_odf_encrypted};
use super::rtf::{parse_rtf_info_metadata, read_bounded_rtf_bytes};
use super::spreadsheet::extract_spreadsheet_metadata;
use super::xml::{get_attr_local, local_name_start, parse_simple_xml_tags};
use crate::media::document::{DocumentError, MAX_XML_ENTRY_BYTES, read_zip_entry_capped};

/// Extracts metadata key/value pairs for Office documents and spreadsheets (`Title`, `Author`,
/// `Subject`, `Created`, `Pages`, `Slides`, `Sheets`).
pub fn extract_office_metadata(path: &Path) -> Result<BTreeMap<String, String>, DocumentError> {
    let mut fields = BTreeMap::new();
    let ext = extension_lower(path);

    match ext.as_str() {
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => {
            extract_spreadsheet_metadata(path, &ext, &mut fields, insert_if_nonempty)?;
        }
        "docx" | "pptx" => {
            if is_cfb_file(path) {
                fields.insert("Encrypted".into(), "Yes".into());
                return Ok(fields);
            }
            let file = File::open(path)?;
            let mut archive = ZipArchive::new(BufReader::new(file))
                .map_err(|e| DocumentError::Message(format!("Invalid Office archive: {e}")))?;
            if let Ok(core_xml) =
                read_zip_entry_capped(&mut archive, "docProps/core.xml", MAX_XML_ENTRY_BYTES)
            {
                parse_ooxml_core_metadata(&core_xml, &mut fields);
            }
            if let Ok(app_xml) =
                read_zip_entry_capped(&mut archive, "docProps/app.xml", MAX_XML_ENTRY_BYTES)
            {
                parse_ooxml_app_metadata(&app_xml, &mut fields);
            }
        }
        "odt" | "odp" => {
            let file = File::open(path)?;
            let mut archive = ZipArchive::new(BufReader::new(file))
                .map_err(|e| DocumentError::Message(format!("Invalid ODF archive: {e}")))?;
            if is_odf_encrypted(&mut archive) {
                fields.insert("Encrypted".into(), "Yes".into());
            }
            if let Ok(meta_xml) =
                read_zip_entry_capped(&mut archive, "meta.xml", MAX_XML_ENTRY_BYTES)
            {
                parse_odf_metadata(&meta_xml, &mut fields);
            }
        }
        "rtf" => {
            if let Ok(bytes) = read_bounded_rtf_bytes(path) {
                parse_rtf_info_metadata(&bytes.0, &mut fields);
            }
        }
        _ => {}
    }

    Ok(fields)
}

fn insert_if_nonempty(map: &mut BTreeMap<String, String>, key: &str, val: Option<String>) {
    if let Some(v) = val {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            map.insert(key.to_string(), trimmed.to_string());
        }
    }
}

fn parse_ooxml_core_metadata(xml: &[u8], fields: &mut BTreeMap<String, String>) {
    parse_simple_xml_tags(xml, |tag, val| match tag {
        "title" => {
            fields.insert("Title".into(), val.to_string());
        }
        "creator" => {
            fields.insert("Author".into(), val.to_string());
        }
        "subject" => {
            fields.insert("Subject".into(), val.to_string());
        }
        "created" => {
            fields.insert("Created".into(), val.to_string());
        }
        _ => {}
    });
}

fn parse_ooxml_app_metadata(xml: &[u8], fields: &mut BTreeMap<String, String>) {
    parse_simple_xml_tags(xml, |tag, val| match tag {
        "Pages" => {
            fields.insert("Pages".into(), val.to_string());
        }
        "Slides" => {
            fields.insert("Slides".into(), val.to_string());
        }
        _ => {}
    });
}

fn parse_odf_metadata(xml: &[u8], fields: &mut BTreeMap<String, String>) {
    let mut initial_creator: Option<String> = None;

    parse_simple_xml_tags(xml, |tag, val| match tag {
        "title" => {
            fields.insert("Title".into(), val.to_string());
        }
        "creator" => {
            fields.insert("Author".into(), val.to_string());
        }
        "initial-creator" => {
            initial_creator = Some(val.to_string());
        }
        "subject" => {
            fields.insert("Subject".into(), val.to_string());
        }
        "creation-date" => {
            fields.insert("Created".into(), val.to_string());
        }
        _ => {}
    });

    if !fields.contains_key("Author")
        && let Some(creator) = initial_creator
    {
        fields.insert("Author".into(), creator);
    }

    let mut reader = Reader::from_reader(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if local_name_start(&e) == "document-statistic" =>
            {
                if let Some(pages) = get_attr_local(&e, "page-count") {
                    insert_if_nonempty(fields, "Pages", Some(pages));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ooxml_core_and_app_metadata_with_trimming() {
        let core_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                           xmlns:dc="http://purl.org/dc/elements/1.1/"
                           xmlns:dcterms="http://purl.org/dc/terms/">
          <dc:title>  Annual Plan  </dc:title>
          <dc:creator>  Ada Lovelace </dc:creator>
          <dc:subject>   </dc:subject>
          <dcterms:created> 2026-03-01T10:00:00Z </dcterms:created>
        </cp:coreProperties>"#;

        let app_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">
          <Pages> 12 </Pages>
          <Slides> 5 </Slides>
        </Properties>"#;

        let mut fields = BTreeMap::new();
        parse_ooxml_core_metadata(core_xml, &mut fields);
        parse_ooxml_app_metadata(app_xml, &mut fields);

        assert_eq!(fields.get("Title").map(String::as_str), Some("Annual Plan"));
        assert_eq!(
            fields.get("Author").map(String::as_str),
            Some("Ada Lovelace")
        );
        assert!(!fields.contains_key("Subject"));
        assert_eq!(
            fields.get("Created").map(String::as_str),
            Some("2026-03-01T10:00:00Z")
        );
        assert_eq!(fields.get("Pages").map(String::as_str), Some("12"));
        assert_eq!(fields.get("Slides").map(String::as_str), Some("5"));
    }

    #[test]
    fn parses_odf_metadata_with_initial_creator_fallback_and_trimming() {
        let meta_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                              xmlns:dc="http://purl.org/dc/elements/1.1/"
                              xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0">
          <office:meta>
            <dc:title>  Design Doc  </dc:title>
            <meta:initial-creator>  First Author </meta:initial-creator>
            <dc:subject> Architecture </dc:subject>
            <meta:creation-date> 2026-01-15 </meta:creation-date>
            <meta:document-statistic meta:page-count="  9  "/>
          </office:meta>
        </office:document-meta>"#;

        let mut fields = BTreeMap::new();
        parse_odf_metadata(meta_xml, &mut fields);

        assert_eq!(fields.get("Title").map(String::as_str), Some("Design Doc"));
        assert_eq!(
            fields.get("Author").map(String::as_str),
            Some("First Author")
        );
        assert_eq!(
            fields.get("Subject").map(String::as_str),
            Some("Architecture")
        );
        assert_eq!(
            fields.get("Created").map(String::as_str),
            Some("2026-01-15")
        );
        assert_eq!(fields.get("Pages").map(String::as_str), Some("9"));

        // Explicit `dc:creator` takes precedence over `meta:initial-creator`.
        let with_creator = br#"<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0"><office:meta><meta:initial-creator>Initial</meta:initial-creator><dc:creator>Final</dc:creator></office:meta></office:document-meta>"#;
        let mut fields2 = BTreeMap::new();
        parse_odf_metadata(with_creator, &mut fields2);
        assert_eq!(fields2.get("Author").map(String::as_str), Some("Final"));
    }

    #[test]
    fn insert_if_nonempty_trims_and_ignores_blank_values() {
        let mut map = BTreeMap::new();
        insert_if_nonempty(&mut map, "A", None);
        insert_if_nonempty(&mut map, "B", Some("   \t\n ".into()));
        insert_if_nonempty(&mut map, "C", Some("  value  ".into()));
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("C").map(String::as_str), Some("value"));
    }
}
