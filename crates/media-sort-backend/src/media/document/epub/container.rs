use std::collections::HashSet;
use std::fs::File;
use std::io::BufReader;

use quick_xml::events::Event;
use zip::ZipArchive;

use super::super::{MAX_XML_ENTRY_BYTES, decode_xml_bytes};
use super::archive::{EpubFailure, read_zip_path_capped, resolve_href};
use super::opf::{ParsedEpub, parse_opf};
use super::xml::{attr_value_local, tolerant_reader};

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

/// Parses `META-INF/container.xml`, optional `META-INF/encryption.xml`, and the root OPF package.
pub(super) fn parse_epub_package(
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

#[cfg(test)]
mod tests {
    use zip::CompressionMethod;

    use super::super::archive::open_epub_zip;
    use super::super::test_support::{TempTestDir, write_epub_entries};
    use super::*;

    #[test]
    fn parse_container_opf_path_prefers_oebps_media_type_with_fallback_and_rejects_traversal() {
        let preferred = br#"<container version="1.0"><rootfiles>
            <rootfile full-path="alt/other.xml" media-type="text/xml"/>
            <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
        </rootfiles></container>"#;
        assert_eq!(
            parse_container_opf_path(preferred).expect("preferred rootfile"),
            ("OEBPS/content.opf".to_string(), None)
        );

        let fallback = br#"<container version="1.0"><rootfiles>
            <rootfile full-path="OPS/my%20book.opf"/>
        </rootfiles></container>"#;
        assert_eq!(
            parse_container_opf_path(fallback).expect("fallback rootfile"),
            (
                "OPS/my book.opf".to_string(),
                Some("OPS/my%20book.opf".to_string())
            )
        );

        let traversal = br#"<container version="1.0"><rootfiles>
            <rootfile full-path="../outside.opf" media-type="application/oebps-package+xml"/>
        </rootfiles></container>"#;
        assert!(matches!(
            parse_container_opf_path(traversal),
            Err(EpubFailure::Corrupt(_))
        ));

        let empty = br#"<container version="1.0"><rootfiles/></container>"#;
        assert!(matches!(
            parse_container_opf_path(empty),
            Err(EpubFailure::Corrupt(_))
        ));
    }

    #[test]
    fn parse_encrypted_uris_ignores_font_obfuscation_and_records_drm() {
        let encryption_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
            xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData>
    <enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
    <enc:CipherData><enc:CipherReference URI="OEBPS/fonts/idpf.woff"/></enc:CipherData>
  </enc:EncryptedData>
  <enc:EncryptedData>
    <enc:EncryptionMethod Algorithm="http://ns.adobe.com/pdf/enc#RC"/>
    <enc:CipherData><enc:CipherReference URI="OEBPS/fonts/adobe.otf"/></enc:CipherData>
  </enc:EncryptedData>
  <enc:EncryptedData>
    <enc:EncryptionMethod Algorithm="http://example.org/custom#embedding"/>
    <enc:CipherData><enc:CipherReference URI="OEBPS/fonts/custom.ttf"/></enc:CipherData>
  </enc:EncryptedData>
  <enc:EncryptedData>
    <enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
    <enc:CipherData><enc:CipherReference URI="OEBPS/my%20chapter.xhtml"/></enc:CipherData>
  </enc:EncryptedData>
</encryption>"#;

        let uris = parse_encrypted_uris(encryption_xml);
        assert!(!uris.contains("OEBPS/fonts/idpf.woff"));
        assert!(!uris.contains("OEBPS/fonts/adobe.otf"));
        assert!(!uris.contains("OEBPS/fonts/custom.ttf"));
        assert!(uris.contains("OEBPS/my chapter.xhtml"));
        assert!(uris.contains("OEBPS/my%20chapter.xhtml"));
    }

    #[test]
    fn parse_epub_package_rejects_encrypted_opf_and_uses_percent_encoded_fallback() {
        let dir = TempTestDir::new("container_pkg");

        // Encrypted OPF rejection
        let encrypted_path = dir.path().join("encrypted_opf.epub");
        let container_xml = br#"<container version="1.0"><rootfiles>
            <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
        </rootfiles></container>"#;
        let encryption_xml = br#"<encryption xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
            <enc:EncryptedData>
                <enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
                <enc:CipherData><enc:CipherReference URI="OEBPS/content.opf"/></enc:CipherData>
            </enc:EncryptedData>
        </encryption>"#;
        write_epub_entries(
            &encrypted_path,
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
                (
                    "OEBPS/content.opf",
                    b"<package/>",
                    CompressionMethod::Deflated,
                ),
            ],
        );
        let mut enc_archive = open_epub_zip(&encrypted_path).expect("open encrypted epub");
        assert!(matches!(
            parse_epub_package(&mut enc_archive),
            Err(EpubFailure::Encrypted)
        ));

        // OEBPS/ resolution and literal percent-encoded fallback OPF path
        let fallback_path = dir.path().join("fallback_opf.epub");
        let fallback_container = br#"<container version="1.0"><rootfiles>
            <rootfile full-path="OEBPS/my%20book.opf" media-type="application/oebps-package+xml"/>
        </rootfiles></container>"#;
        let opf = br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
            <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
                <dc:title>Fallback OPF</dc:title>
            </metadata>
            <manifest>
                <item id="ch1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
            </manifest>
            <spine><itemref idref="ch1"/></spine>
        </package>"#;
        write_epub_entries(
            &fallback_path,
            &[
                (
                    "META-INF/container.xml",
                    fallback_container,
                    CompressionMethod::Deflated,
                ),
                ("OEBPS/my%20book.opf", opf, CompressionMethod::Deflated),
            ],
        );
        let mut fb_archive = open_epub_zip(&fallback_path).expect("open fallback epub");
        let parsed = parse_epub_package(&mut fb_archive).expect("parse fallback opf");
        assert_eq!(parsed.metadata.title.as_deref(), Some("Fallback OPF"));
        assert_eq!(parsed.manifest["ch1"].resolved_path, "OEBPS/text/ch1.xhtml");
    }
}
