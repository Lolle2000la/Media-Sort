use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use calamine::{Data, Reader as _, Sheets};

use super::archive::{extension_lower, is_cfb_file};
use crate::media::document::{
    DocumentError, DocumentPreview, DocumentUnavailableReason, MAX_PDF_FILE_BYTES, MAX_SHEET_COLS,
    MAX_SHEET_ROWS, SheetData,
};

const MAX_SHEETS: usize = 32;

/// Loads a bounded multi-sheet spreadsheet preview for `.xlsx`, `.xlsm`, `.xlsb`, `.xls`, and
/// `.ods` files using `calamine`.
pub fn load_spreadsheet_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_PDF_FILE_BYTES {
        return Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::TooLarge,
        });
    }

    let mut workbook = match open_spreadsheet(path) {
        Ok(wb) => wb,
        Err(err) if is_calamine_password_error(&err) => {
            return Ok(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::Encrypted,
            });
        }
        Err(err) => {
            return Ok(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(err.to_string()),
            });
        }
    };

    let sheet_names = workbook.sheet_names();
    let mut sheets = Vec::new();

    for name in sheet_names.into_iter().take(MAX_SHEETS) {
        let Ok(range) = workbook.worksheet_range(&name) else {
            continue;
        };

        let raw_rows = range.height();
        let raw_cols = range.width();
        let mut truncated = raw_rows > MAX_SHEET_ROWS + 1 || raw_cols > MAX_SHEET_COLS;

        let mut extracted_rows: Vec<Vec<String>> = Vec::new();
        let mut max_cols = 0usize;

        for row in range.rows().take(MAX_SHEET_ROWS + 1) {
            if row.len() > MAX_SHEET_COLS {
                truncated = true;
            }
            let cells: Vec<String> = row.iter().take(MAX_SHEET_COLS).map(format_cell).collect();
            max_cols = max_cols.max(cells.len());
            extracted_rows.push(cells);
        }

        let (headers, rows) = if extracted_rows.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let mut iter = extracted_rows.into_iter();
            let mut hdrs = iter.next().unwrap_or_default();
            hdrs.resize(max_cols, String::new());
            let mut body: Vec<Vec<String>> = iter.collect();
            for row in &mut body {
                row.resize(max_cols, String::new());
            }
            (hdrs, body)
        };

        sheets.push(SheetData {
            name,
            headers,
            rows,
            total_rows: raw_rows.saturating_sub(1),
            total_cols: raw_cols,
            truncated,
        });
    }

    Ok(DocumentPreview::Spreadsheet { sheets })
}

/// Extracts spreadsheet metadata (`Sheets`, `Sheet Names`, `Title`, `Author`, `Subject`,
/// `Created`, or `Encrypted: Yes`) into `fields`.
pub(super) fn extract_spreadsheet_metadata(
    path: &Path,
    ext: &str,
    fields: &mut BTreeMap<String, String>,
    insert_if_nonempty: impl Fn(&mut BTreeMap<String, String>, &str, Option<String>),
) -> Result<(), DocumentError> {
    if matches!(ext, "xlsx" | "xlsm") && is_cfb_file(path) {
        fields.insert("Encrypted".into(), "Yes".into());
        return Ok(());
    }
    match open_spreadsheet(path) {
        Ok(mut workbook) => {
            let names = workbook.sheet_names();
            fields.insert("Sheets".into(), names.len().to_string());
            if !names.is_empty() {
                let shown: Vec<&str> = names.iter().take(10).map(String::as_str).collect();
                let mut summary = shown.join(", ");
                if names.len() > 10 {
                    summary.push_str(", ...");
                }
                fields.insert("Sheet Names".into(), summary);
            }
            if let Ok(props) = workbook.workbook_properties() {
                insert_if_nonempty(fields, "Title", props.title);
                insert_if_nonempty(fields, "Author", props.creator);
                insert_if_nonempty(fields, "Subject", props.subject);
                insert_if_nonempty(fields, "Created", props.created);
            }
            Ok(())
        }
        Err(err) if is_calamine_password_error(&err) => {
            fields.insert("Encrypted".into(), "Yes".into());
            Ok(())
        }
        Err(err) => Err(DocumentError::Message(err.to_string())),
    }
}

fn open_spreadsheet(path: &Path) -> Result<Sheets<BufReader<File>>, calamine::Error> {
    match extension_lower(path).as_str() {
        "xlsx" | "xlsm" | "xlam" => calamine::open_workbook::<calamine::Xlsx<_>, _>(path)
            .map(Sheets::Xlsx)
            .map_err(calamine::Error::Xlsx),
        "xls" | "xla" => calamine::open_workbook::<calamine::Xls<_>, _>(path)
            .map(Sheets::Xls)
            .map_err(calamine::Error::Xls),
        "xlsb" => calamine::open_workbook::<calamine::Xlsb<_>, _>(path)
            .map(Sheets::Xlsb)
            .map_err(calamine::Error::Xlsb),
        "ods" => calamine::open_workbook::<calamine::Ods<_>, _>(path)
            .map(Sheets::Ods)
            .map_err(calamine::Error::Ods),
        _ => calamine::open_workbook_auto(path),
    }
}

fn is_calamine_password_error(err: &calamine::Error) -> bool {
    matches!(
        err,
        calamine::Error::Xlsx(calamine::XlsxError::Password)
            | calamine::Error::Xls(calamine::XlsError::Password)
            | calamine::Error::Xlsb(calamine::XlsbError::Password)
            | calamine::Error::Ods(calamine::OdsError::Password)
    )
}

fn format_cell(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => {
            if !f.is_finite() {
                f.to_string()
            } else if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{f:.0}")
            } else {
                f.to_string()
            }
        }
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => {
            if dt.is_duration() {
                let total_secs = (dt.as_f64() * 86_400.0).round() as i64;
                let sign = if total_secs < 0 { "-" } else { "" };
                let abs = total_secs.unsigned_abs();
                let h = abs / 3600;
                let m = (abs % 3600) / 60;
                let s = abs % 60;
                format!("{sign}{h:02}:{m:02}:{s:02}")
            } else {
                let (y, m, d, hh, mm, ss, _) = dt.to_ymd_hms_milli();
                let serial = dt.as_f64();
                if (0.0..1.0).contains(&serial) {
                    format!("{hh:02}:{mm:02}:{ss:02}")
                } else if hh == 0 && mm == 0 && ss == 0 {
                    format!("{y:04}-{m:02}-{d:02}")
                } else {
                    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
                }
            }
        }
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    use calamine::{CellErrorType, ExcelDateTime, ExcelDateTimeType};
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;

    use super::*;

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ms_office_sheet_test_{label}_{}_{seq}",
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

    #[test]
    fn format_cell_handles_floats_nan_dates_times_and_errors() {
        assert_eq!(format_cell(&Data::Empty), "");
        assert_eq!(format_cell(&Data::String("abc".into())), "abc");
        assert_eq!(format_cell(&Data::Int(-7)), "-7");
        assert_eq!(format_cell(&Data::Bool(true)), "true");

        assert_eq!(format_cell(&Data::Float(42.0)), "42");
        assert_eq!(format_cell(&Data::Float(3.25)), "3.25");
        assert_eq!(format_cell(&Data::Float(f64::NAN)), "NaN");
        assert_eq!(format_cell(&Data::Float(f64::INFINITY)), "inf");

        // Serial 0.5 -> time-only 12:00:00
        let time_only = ExcelDateTime::new(0.5, ExcelDateTimeType::DateTime, false);
        assert_eq!(format_cell(&Data::DateTime(time_only)), "12:00:00");

        // Serial 45323.0 -> 2024-02-01 (date-only)
        let date_only = ExcelDateTime::new(45323.0, ExcelDateTimeType::DateTime, false);
        assert_eq!(format_cell(&Data::DateTime(date_only)), "2024-02-01");

        // Serial 45323.5 -> 2024-02-01 12:00:00
        let date_time = ExcelDateTime::new(45323.5, ExcelDateTimeType::DateTime, false);
        assert_eq!(
            format_cell(&Data::DateTime(date_time)),
            "2024-02-01 12:00:00"
        );

        // Duration 1.5 days -> 36:00:00
        let dur = ExcelDateTime::new(1.5, ExcelDateTimeType::TimeDelta, false);
        assert_eq!(format_cell(&Data::DateTime(dur)), "36:00:00");

        assert_eq!(format_cell(&Data::Error(CellErrorType::Div0)), "#DIV/0!");
    }

    #[test]
    fn is_calamine_password_error_matches_all_password_variants() {
        assert!(is_calamine_password_error(&calamine::Error::Xlsx(
            calamine::XlsxError::Password
        )));
        assert!(is_calamine_password_error(&calamine::Error::Xls(
            calamine::XlsError::Password
        )));
        assert!(is_calamine_password_error(&calamine::Error::Xlsb(
            calamine::XlsbError::Password
        )));
        assert!(is_calamine_password_error(&calamine::Error::Ods(
            calamine::OdsError::Password
        )));
        assert!(!is_calamine_password_error(&calamine::Error::Msg("other")));
    }

    #[test]
    fn extract_spreadsheet_metadata_truncates_more_than_ten_sheet_names() {
        let dir = TempDir::new("sheets_meta");
        let ods_path = dir.0.join("many.ods");

        let mut tables = String::new();
        for idx in 1..=12 {
            tables.push_str(&format!(
                r#"<table:table table:name="Sheet{idx}"><table:table-row><table:table-cell office:value-type="string"><text:p>ok</text:p></table:table-cell></table:table-row></table:table>"#
            ));
        }
        let content_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
  xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
  xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
  xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
  office:version="1.2">
  <office:body><office:spreadsheet>{tables}</office:spreadsheet></office:body>
</office:document-content>"#
        );
        let manifest_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/>
  <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#;

        let file = File::create(&ods_path).expect("create ods");
        let mut writer = zip::ZipWriter::new(file);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file("mimetype", stored).unwrap();
        writer
            .write_all(b"application/vnd.oasis.opendocument.spreadsheet")
            .unwrap();
        writer
            .start_file("META-INF/manifest.xml", deflated)
            .unwrap();
        writer.write_all(manifest_xml).unwrap();
        writer.start_file("content.xml", deflated).unwrap();
        writer.write_all(content_xml.as_bytes()).unwrap();
        writer.finish().unwrap();

        let mut fields = BTreeMap::new();
        extract_spreadsheet_metadata(&ods_path, "ods", &mut fields, |map, k, v| {
            if let Some(s) = v {
                let t = s.trim();
                if !t.is_empty() {
                    map.insert(k.to_string(), t.to_string());
                }
            }
        })
        .expect("ods metadata");

        assert_eq!(fields.get("Sheets").map(String::as_str), Some("12"));
        assert_eq!(
            fields.get("Sheet Names").map(String::as_str),
            Some(
                "Sheet1, Sheet2, Sheet3, Sheet4, Sheet5, Sheet6, Sheet7, Sheet8, Sheet9, Sheet10, ..."
            )
        );
    }
}
