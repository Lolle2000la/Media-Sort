use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::media::document::{
    DocumentError, DocumentPreview, DocumentUnavailableReason, MAX_XML_ENTRY_BYTES,
    read_zip_entry_capped,
};

const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Returns the lowercase ASCII file extension of `path`, or an empty string if missing/non-UTF-8.
pub(super) fn extension_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Returns `true` when `path` starts with the OLE2 Compound File Binary (`CFB`) magic header,
/// which encrypted OOXML containers use as an outer wrapper.
pub(super) fn is_cfb_file(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).is_ok() && magic == CFB_MAGIC
}

/// Returns `true` when an ODF ZIP container's `META-INF/manifest.xml` declares `encryption-data`.
pub(super) fn is_odf_encrypted(archive: &mut ZipArchive<BufReader<File>>) -> bool {
    let Ok(bytes) = read_zip_entry_capped(archive, "META-INF/manifest.xml", MAX_XML_ENTRY_BYTES)
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    text.contains("encryption-data")
}

/// Opens a ZIP-based Office archive or returns a user-facing `DocumentPreview::Unavailable`
/// error variant (identifying encrypted OOXML CFB wrappers when `ooxml` is `true`).
pub(super) fn open_zip_or_unavailable(
    path: &Path,
    ooxml: bool,
) -> Result<ZipArchive<BufReader<File>>, DocumentPreview> {
    if ooxml && is_cfb_file(path) {
        return Err(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::Encrypted,
        });
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) => {
            return Err(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(err.to_string()),
            });
        }
    };
    ZipArchive::new(BufReader::new(file)).map_err(|err| DocumentPreview::Unavailable {
        reason: DocumentUnavailableReason::UnsupportedOrCorrupt(format!(
            "Invalid Office archive: {err}"
        )),
    })
}

/// Reads `content.xml` from an ODF archive, returning `Ok(Err(DocumentPreview::Unavailable))`
/// when the archive is encrypted, corrupt, or missing `content.xml`.
pub(super) fn read_odf_content_xml(
    path: &Path,
) -> Result<Result<Vec<u8>, DocumentPreview>, DocumentError> {
    let _ = std::fs::metadata(path)?;
    let mut archive = match open_zip_or_unavailable(path, false) {
        Ok(a) => a,
        Err(preview) => return Ok(Err(preview)),
    };
    if is_odf_encrypted(&mut archive) {
        return Ok(Err(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::Encrypted,
        }));
    }
    match read_zip_entry_capped(&mut archive, "content.xml", MAX_XML_ENTRY_BYTES) {
        Ok(bytes) => Ok(Ok(bytes)),
        Err(err) => Ok(Err(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::UnsupportedOrCorrupt(err.to_string()),
        })),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;

    use super::*;

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ms_office_archive_test_{label}_{}_{seq}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create zip");
        let mut writer = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in entries {
            writer.start_file(*name, opts).expect("start zip entry");
            writer.write_all(data).expect("write zip entry");
        }
        writer.finish().expect("finish zip");
    }

    #[test]
    fn extension_lower_normalizes_and_handles_missing() {
        assert_eq!(extension_lower(Path::new("report.DOCX")), "docx");
        assert_eq!(extension_lower(Path::new("my.file.XlsX")), "xlsx");
        assert_eq!(extension_lower(Path::new("no_ext")), "");
    }

    #[test]
    fn is_cfb_file_detects_magic_and_rejects_short_or_other_files() {
        let dir = TempDir::new("cfb");
        let cfb = dir.0.join("encrypted.docx");
        let mut payload = CFB_MAGIC.to_vec();
        payload.extend_from_slice(b"extra");
        std::fs::write(&cfb, payload).unwrap();
        assert!(is_cfb_file(&cfb));

        let short = dir.0.join("short.docx");
        std::fs::write(&short, &CFB_MAGIC[..4]).unwrap();
        assert!(!is_cfb_file(&short));

        let plain = dir.0.join("plain.docx");
        std::fs::write(&plain, b"PK\x03\x04notcfb").unwrap();
        assert!(!is_cfb_file(&plain));

        assert!(!is_cfb_file(&dir.0.join("missing.docx")));
    }

    #[test]
    fn open_zip_or_unavailable_distinguishes_encrypted_ooxml_and_corrupt() {
        let dir = TempDir::new("open_zip");
        let cfb_path = dir.0.join("enc.docx");
        std::fs::write(&cfb_path, CFB_MAGIC).unwrap();

        match open_zip_or_unavailable(&cfb_path, true) {
            Err(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::Encrypted,
            }) => {}
            other => panic!("expected Encrypted for OOXML CFB, got {}", other.is_ok()),
        }

        // Non-OOXML paths do not treat CFB magic as Encrypted; they report UnsupportedOrCorrupt.
        match open_zip_or_unavailable(&cfb_path, false) {
            Err(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(_),
            }) => {}
            other => panic!("expected UnsupportedOrCorrupt, got {}", other.is_ok()),
        }

        let corrupt = dir.0.join("corrupt.docx");
        std::fs::write(&corrupt, b"not a zip archive").unwrap();
        match open_zip_or_unavailable(&corrupt, true) {
            Err(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(msg),
            }) => assert!(msg.contains("Invalid Office archive")),
            other => panic!("expected UnsupportedOrCorrupt, got {}", other.is_ok()),
        }
    }

    #[test]
    fn odf_encryption_and_content_xml_reading() {
        let dir = TempDir::new("odf");

        let encrypted = dir.0.join("enc.odt");
        write_zip(
            &encrypted,
            &[(
                "META-INF/manifest.xml",
                br#"<manifest><file-entry><encryption-data/></file-entry></manifest>"#,
            )],
        );
        {
            let file = File::open(&encrypted).unwrap();
            let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
            assert!(is_odf_encrypted(&mut archive));
        }
        match read_odf_content_xml(&encrypted).expect("io ok") {
            Err(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::Encrypted,
            }) => {}
            other => panic!("expected Encrypted ODF, got {other:?}"),
        }

        let valid = dir.0.join("ok.odt");
        write_zip(
            &valid,
            &[
                (
                    "META-INF/manifest.xml",
                    br#"<manifest><file-entry full-path="content.xml"/></manifest>"#,
                ),
                ("content.xml", b"<office:document-content/>"),
            ],
        );
        let bytes = read_odf_content_xml(&valid)
            .expect("io ok")
            .expect("content.xml bytes");
        assert_eq!(bytes, b"<office:document-content/>");

        let no_content = dir.0.join("missing_content.odt");
        write_zip(&no_content, &[("mimetype", b"text")]);
        assert!(matches!(
            read_odf_content_xml(&no_content).expect("io ok"),
            Err(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(_)
            })
        ));
    }
}
