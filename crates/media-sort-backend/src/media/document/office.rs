use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use calamine::{Data, Reader as _, Sheets};
use quick_xml::XmlVersion;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;
use zip::ZipArchive;

use super::{
    DecodedImage, DocumentError, DocumentPreview, DocumentUnavailableReason,
    MAX_ARCHIVE_ENTRY_BYTES, MAX_PDF_FILE_BYTES, MAX_SHEET_COLS, MAX_SHEET_ROWS, MAX_SLIDES,
    MAX_TEXT_PREVIEW_BYTES, MAX_XML_ENTRY_BYTES, PREVIEW_TARGET_HEIGHT, PREVIEW_TARGET_WIDTH,
    SheetData, SlideData, natural_cmp, os_preview, pdf, read_zip_entry_capped, soffice_pipe,
};

const MAX_SHEETS: usize = 32;
const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Extracts a bounded thumbnail for Office or spreadsheet documents.
///
/// Checks embedded archive thumbnails first (`Thumbnails/thumbnail.png`, `docProps/thumbnail.*`),
/// then falls back to the platform OS thumbnail provider (`os_preview::generate_os_thumbnail`),
/// and finally (when `enable_libreoffice` is enabled) converts via LibreOffice to a cached PDF
/// and renders page 0. Errors are logged at debug level and converted to `Ok(None)` so missing or
/// unsupported Office thumbnails never pollute `media_errors`.
pub fn extract_office_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
    enable_libreoffice: bool,
) -> Result<Option<DecodedImage>, DocumentError> {
    let max_width = max_width.max(1);
    let max_height = max_height.max(1);
    let ext = extension_lower(path);

    if matches!(
        ext.as_str(),
        "docx" | "odt" | "pptx" | "odp" | "xlsx" | "xlsm" | "ods"
    ) && let Some(img) = extract_embedded_zip_thumbnail(path, max_width, max_height)
    {
        return Ok(Some(img));
    }

    match os_preview::generate_os_thumbnail(path, max_width, max_height) {
        Ok(Some(img)) => return Ok(Some(img)),
        Ok(None) => {}
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "OS thumbnail lookup failed for Office document"
            );
        }
    }

    if enable_libreoffice && soffice_pipe::is_libreoffice_available() {
        match soffice_pipe::convert_to_pdf_path(path) {
            Ok(Some(pdf_path)) => match pdf::render_pdf_thumbnail(&pdf_path, max_width, max_height)
            {
                Ok(img) => return Ok(img),
                Err(err) => {
                    tracing::debug!(
                        path = %path.display(),
                        pdf_path = %pdf_path.display(),
                        error = %err,
                        "Failed to render PDF thumbnail from LibreOffice output"
                    );
                }
            },
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %err,
                    "LibreOffice conversion failed while generating Office thumbnail"
                );
            }
        }
    }

    Ok(None)
}

fn extract_embedded_zip_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Option<DecodedImage> {
    const CANDIDATES: [&str; 4] = [
        "Thumbnails/thumbnail.png",
        "docProps/thumbnail.png",
        "docProps/thumbnail.jpeg",
        "docProps/thumbnail.jpg",
    ];

    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(BufReader::new(file)).ok()?;

    let mut matched_names: Vec<String> = Vec::new();
    for candidate in CANDIDATES {
        if let Some(exact) = archive
            .file_names()
            .flatten()
            .find(|name| name.eq_ignore_ascii_case(candidate))
        {
            matched_names.push(exact.into_owned());
        }
    }

    for entry_name in matched_names {
        let Ok(bytes) = read_zip_entry_capped(&mut archive, &entry_name, MAX_ARCHIVE_ENTRY_BYTES)
        else {
            continue;
        };
        if let Ok(decoded) =
            crate::media::format_pipeline::decode_and_resize_bytes(&bytes, max_width, max_height)
        {
            return Some(decoded);
        }
    }
    None
}

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

/// Loads an Office document preview.
///
/// When `enable_libreoffice` is enabled and LibreOffice is available, converts the document to a
/// cached PDF file and renders `page_index` via `pdf::load_pdf_preview`; otherwise (or on
/// conversion failure) falls back to pure-Rust XML/RTF parsing or OS thumbnails.
pub fn load_office_preview(
    path: &Path,
    page_index: usize,
    enable_libreoffice: bool,
) -> Result<DocumentPreview, DocumentError> {
    if enable_libreoffice && soffice_pipe::is_libreoffice_available() {
        match soffice_pipe::convert_to_pdf_path(path) {
            Ok(Some(pdf_path)) => match pdf::load_pdf_preview(&pdf_path, page_index) {
                Ok(preview @ DocumentPreview::PagedImage { .. }) => return Ok(preview),
                Ok(other) => {
                    tracing::warn!(
                        path = %path.display(),
                        preview = ?other,
                        "LibreOffice-converted PDF produced non-paged preview; falling back to pure-Rust parser"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "Failed to render LibreOffice-converted PDF; falling back to pure-Rust parser"
                    );
                }
            },
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "LibreOffice conversion failed; falling back to pure-Rust parser"
                );
            }
        }
    }

    let ext = extension_lower(path);
    match ext.as_str() {
        "docx" => load_docx_preview(path),
        "odt" => load_odt_preview(path),
        "pptx" => load_pptx_preview(path),
        "odp" => load_odp_preview(path),
        "rtf" => load_rtf_preview(path),
        "doc" | "ppt" => {
            if let Some(image) = os_preview::generate_os_thumbnail(
                path,
                PREVIEW_TARGET_WIDTH,
                PREVIEW_TARGET_HEIGHT,
            )? {
                Ok(DocumentPreview::PagedImage {
                    image,
                    page_index: 0,
                    total_pages: 1,
                })
            } else {
                Ok(DocumentPreview::Unavailable {
                    reason: DocumentUnavailableReason::NoPreviewBackend,
                })
            }
        }
        _ => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::NoPreviewBackend,
        }),
    }
}

/// Extracts metadata key/value pairs for Office documents and spreadsheets (`Title`, `Author`,
/// `Subject`, `Created`, `Pages`, `Slides`, `Sheets`).
pub fn extract_office_metadata(path: &Path) -> Result<BTreeMap<String, String>, DocumentError> {
    let mut fields = BTreeMap::new();
    let ext = extension_lower(path);

    match ext.as_str() {
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => {
            if matches!(ext.as_str(), "xlsx" | "xlsm") && is_cfb_file(path) {
                fields.insert("Encrypted".into(), "Yes".into());
                return Ok(fields);
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
                        insert_if_nonempty(&mut fields, "Title", props.title);
                        insert_if_nonempty(&mut fields, "Author", props.creator);
                        insert_if_nonempty(&mut fields, "Subject", props.subject);
                        insert_if_nonempty(&mut fields, "Created", props.created);
                    }
                }
                Err(err) if is_calamine_password_error(&err) => {
                    fields.insert("Encrypted".into(), "Yes".into());
                }
                Err(err) => return Err(DocumentError::Message(err.to_string())),
            }
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

fn extension_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
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

fn is_cfb_file(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).is_ok() && magic == CFB_MAGIC
}

fn is_odf_encrypted(archive: &mut ZipArchive<BufReader<File>>) -> bool {
    let Ok(bytes) = read_zip_entry_capped(archive, "META-INF/manifest.xml", MAX_XML_ENTRY_BYTES)
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    text.contains("encryption-data")
}

fn open_zip_or_unavailable(
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

fn load_docx_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let _ = std::fs::metadata(path)?;
    let mut archive = match open_zip_or_unavailable(path, true) {
        Ok(a) => a,
        Err(preview) => return Ok(preview),
    };
    let xml = match read_zip_entry_capped(&mut archive, "word/document.xml", MAX_XML_ENTRY_BYTES) {
        Ok(bytes) => bytes,
        Err(err) => {
            return Ok(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(err.to_string()),
            });
        }
    };

    let markdown = parse_docx_xml(&xml);
    Ok(DocumentPreview::Markdown {
        markdown,
        chapter_index: None,
        total_chapters: None,
        cover: None,
    })
}

fn read_odf_content_xml(path: &Path) -> Result<Result<Vec<u8>, DocumentPreview>, DocumentError> {
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

fn load_odt_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let xml = match read_odf_content_xml(path)? {
        Ok(bytes) => bytes,
        Err(preview) => return Ok(preview),
    };

    let markdown = parse_odt_xml(&xml);
    Ok(DocumentPreview::Markdown {
        markdown,
        chapter_index: None,
        total_chapters: None,
        cover: None,
    })
}

fn load_pptx_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let _ = std::fs::metadata(path)?;
    let mut archive = match open_zip_or_unavailable(path, true) {
        Ok(a) => a,
        Err(preview) => return Ok(preview),
    };

    let mut slide_entries: Vec<String> = archive
        .file_names()
        .flatten()
        .filter(|name| is_pptx_slide_entry(name))
        .map(std::borrow::Cow::into_owned)
        .collect();
    slide_entries.sort_by(|a, b| natural_cmp(a, b));
    slide_entries.truncate(MAX_SLIDES);

    let mut slides = Vec::with_capacity(slide_entries.len());
    for (idx, entry_name) in slide_entries.iter().enumerate() {
        let Ok(xml) = read_zip_entry_capped(&mut archive, entry_name, MAX_XML_ENTRY_BYTES) else {
            continue;
        };
        let (title, body_paragraphs) = parse_pptx_slide_xml(&xml);
        slides.push(SlideData {
            number: idx + 1,
            title,
            body_paragraphs,
        });
    }

    Ok(DocumentPreview::Slides { slides })
}

fn is_pptx_slide_entry(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("ppt/slides/slide") else {
        return false;
    };
    let Some(stem) = rest.strip_suffix(".xml") else {
        return false;
    };
    !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit())
}

fn load_odp_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let xml = match read_odf_content_xml(path)? {
        Ok(bytes) => bytes,
        Err(preview) => return Ok(preview),
    };

    let slides = parse_odp_xml(&xml);
    Ok(DocumentPreview::Slides { slides })
}

fn read_bounded_rtf_bytes(path: &Path) -> Result<(Vec<u8>, bool), DocumentError> {
    let file = File::open(path)?;
    let total_len = file.metadata()?.len();
    let mut raw = Vec::new();
    file.take(MAX_TEXT_PREVIEW_BYTES + 1)
        .read_to_end(&mut raw)?;
    let truncated = raw.len() as u64 > MAX_TEXT_PREVIEW_BYTES || total_len > MAX_TEXT_PREVIEW_BYTES;
    if raw.len() as u64 > MAX_TEXT_PREVIEW_BYTES {
        raw.truncate(MAX_TEXT_PREVIEW_BYTES as usize);
    }
    Ok((raw, truncated))
}

fn load_rtf_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let (raw, truncated) = read_bounded_rtf_bytes(path)?;
    let text = parse_rtf_bytes(&raw);
    Ok(DocumentPreview::PlainText { text, truncated })
}

// ── Shared XML helpers ───────────────────────────────────────────────

fn local_name_start<'a>(e: &'a BytesStart<'a>) -> &'a str {
    e.local_name().into_inner()
}

fn get_attr_local(e: &BytesStart<'_>, target_local: &str) -> Option<String> {
    for attr in e.attributes().with_checks(false) {
        let Ok(attr) = attr else { continue };
        if attr.key.local_name().into_inner() == target_local {
            return attr
                .normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
                .or_else(|| Some(attr.value.into_owned()));
        }
    }
    None
}

fn append_general_ref(buf: &mut String, r: &BytesRef<'_>) {
    if let Ok(Some(ch)) = r.resolve_char_ref() {
        buf.push(ch);
    } else if let Some(resolved) = quick_xml::escape::resolve_predefined_entity(r.as_ref()) {
        buf.push_str(resolved);
    }
}

#[derive(Default)]
struct MarkdownTableState {
    depth: usize,
    rows_emitted: usize,
    row_cells: Vec<String>,
    cell_buf: String,
}

impl MarkdownTableState {
    fn enter_table(&mut self) {
        self.depth += 1;
        if self.depth == 1 {
            self.rows_emitted = 0;
            self.row_cells.clear();
            self.cell_buf.clear();
        }
    }

    fn leave_table(&mut self, out: &mut String) {
        if self.depth == 1 && self.rows_emitted > 0 {
            out.push('\n');
        }
        self.depth = self.depth.saturating_sub(1);
    }

    fn enter_row(&mut self) {
        if self.depth == 1 {
            self.row_cells.clear();
        }
    }

    fn leave_row(&mut self, out: &mut String) {
        if self.depth != 1 || self.row_cells.is_empty() {
            return;
        }
        if self.row_cells.iter().all(|c| c.is_empty()) {
            return;
        }
        out.push('|');
        for cell in &self.row_cells {
            out.push(' ');
            out.push_str(cell);
            out.push_str(" |");
        }
        out.push('\n');
        if self.rows_emitted == 0 {
            out.push('|');
            for _ in &self.row_cells {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
        self.rows_emitted += 1;
    }

    fn enter_cell(&mut self) {
        if self.depth == 1 {
            self.cell_buf.clear();
        }
    }

    fn leave_cell(&mut self) {
        if self.depth == 1 {
            let escaped = self.cell_buf.trim().replace('|', r"\|");
            self.row_cells.push(escaped);
            self.cell_buf.clear();
        }
    }

    fn push_paragraph_to_cell(&mut self, text: &str) {
        let collapsed = text.replace(['\r', '\n'], " ");
        let trimmed = collapsed.trim();
        if trimmed.is_empty() {
            return;
        }
        if !self.cell_buf.is_empty() {
            self.cell_buf.push(' ');
        }
        self.cell_buf.push_str(trimmed);
    }
}

fn finish_markdown_output(mut out: String, truncated: bool) -> String {
    while out.ends_with('\n') {
        out.pop();
    }
    if truncated {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("---\n*(Preview truncated)*");
    }
    out
}

fn docx_heading_level(style: &str) -> Option<usize> {
    let lower = style.trim().to_ascii_lowercase();
    if lower == "title" {
        return Some(1);
    }
    if lower == "subtitle" {
        return Some(2);
    }
    let rest = lower.strip_prefix("heading")?.trim();
    let lvl = rest.parse::<usize>().ok()?;
    (1..=6).contains(&lvl).then_some(lvl)
}

fn emit_markdown_paragraph(
    out: &mut String,
    table: &mut MarkdownTableState,
    raw_text: &str,
    heading_level: Option<usize>,
    is_list_item: bool,
    list_indent: usize,
    prev_was_list: &mut bool,
) {
    let trimmed = raw_text.trim();
    if trimmed.is_empty() {
        return;
    }
    if table.depth > 0 {
        table.push_paragraph_to_cell(trimmed);
        return;
    }
    if *prev_was_list && !is_list_item {
        out.push('\n');
    }
    if let Some(lvl) = heading_level {
        for _ in 0..lvl {
            out.push('#');
        }
        out.push(' ');
        out.push_str(&trimmed.replace(['\r', '\n', '\t'], " "));
        out.push_str("\n\n");
        *prev_was_list = false;
    } else if is_list_item {
        for _ in 0..list_indent.min(4) {
            out.push_str("  ");
        }
        out.push_str("- ");
        out.push_str(trimmed);
        out.push('\n');
        *prev_was_list = true;
    } else {
        out.push_str(trimmed);
        out.push_str("\n\n");
        *prev_was_list = false;
    }
}

fn handle_docx_tag(
    e: &BytesStart<'_>,
    in_p_pr: bool,
    heading_level: &mut Option<usize>,
    is_list_item: &mut bool,
    para_buf: &mut String,
) {
    match local_name_start(e) {
        "pStyle" if in_p_pr => {
            if let Some(val) = get_attr_local(e, "val") {
                *heading_level = docx_heading_level(&val);
            }
        }
        "numPr" if in_p_pr => *is_list_item = true,
        "br" | "cr" => para_buf.push('\n'),
        "tab" => para_buf.push('\t'),
        _ => {}
    }
}

fn parse_docx_xml(xml: &[u8]) -> String {
    let mut reader = Reader::from_reader(xml);
    let mut out = String::new();
    let mut truncated = false;

    let mut table = MarkdownTableState::default();
    let mut para_buf = String::new();
    let mut heading_level: Option<usize> = None;
    let mut is_list_item = false;
    let mut prev_was_list = false;
    let mut in_p_pr = false;
    let mut in_t = false;
    let mut skip_depth = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) => {
                let name = local_name_start(&e);
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                if matches!(name, "instrText" | "delText" | "Fallback") {
                    skip_depth = 1;
                    continue;
                }
                match name {
                    "tbl" => table.enter_table(),
                    "tr" => table.enter_row(),
                    "tc" => table.enter_cell(),
                    "p" => {
                        para_buf.clear();
                        heading_level = None;
                        is_list_item = false;
                    }
                    "pPr" => in_p_pr = true,
                    "t" => in_t = true,
                    _ => handle_docx_tag(
                        &e,
                        in_p_pr,
                        &mut heading_level,
                        &mut is_list_item,
                        &mut para_buf,
                    ),
                }
            }
            Ok(Event::Empty(e)) => {
                if skip_depth > 0 {
                    continue;
                }
                handle_docx_tag(
                    &e,
                    in_p_pr,
                    &mut heading_level,
                    &mut is_list_item,
                    &mut para_buf,
                );
            }
            Ok(Event::End(e)) => {
                let name = e.local_name().into_inner();
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                match name {
                    "t" => in_t = false,
                    "pPr" => in_p_pr = false,
                    "p" => {
                        emit_markdown_paragraph(
                            &mut out,
                            &mut table,
                            &para_buf,
                            heading_level,
                            is_list_item,
                            0,
                            &mut prev_was_list,
                        );
                        para_buf.clear();
                        if out.len() as u64 >= MAX_TEXT_PREVIEW_BYTES {
                            truncated = true;
                            break;
                        }
                    }
                    "tc" => table.leave_cell(),
                    "tr" => {
                        table.leave_row(&mut out);
                        if out.len() as u64 >= MAX_TEXT_PREVIEW_BYTES {
                            truncated = true;
                            break;
                        }
                    }
                    "tbl" => {
                        if prev_was_list {
                            out.push('\n');
                            prev_was_list = false;
                        }
                        table.leave_table(&mut out);
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_t && skip_depth == 0 => {
                para_buf.push_str(&t.xml10_content());
            }
            Ok(Event::CData(c)) if in_t && skip_depth == 0 => {
                para_buf.push_str(&c.xml10_content());
            }
            Ok(Event::GeneralRef(r)) if in_t && skip_depth == 0 => {
                append_general_ref(&mut para_buf, &r);
            }
            _ => {}
        }
    }

    finish_markdown_output(out, truncated)
}

fn is_odf_skip_element(local: &str) -> bool {
    matches!(
        local,
        "automatic-styles"
            | "font-face-decls"
            | "styles"
            | "master-styles"
            | "scripts"
            | "meta"
            | "annotation"
            | "tracked-changes"
            | "note-citation"
    )
}

fn handle_odf_inline(e: &BytesStart<'_>, block_buf: &mut String) {
    match local_name_start(e) {
        "line-break" => block_buf.push('\n'),
        "tab" => block_buf.push('\t'),
        "s" => {
            let count = get_attr_local(e, "c")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .min(64);
            for _ in 0..count {
                block_buf.push(' ');
            }
        }
        _ => {}
    }
}

fn parse_odt_xml(xml: &[u8]) -> String {
    let mut reader = Reader::from_reader(xml);
    let mut out = String::new();
    let mut truncated = false;

    let mut table = MarkdownTableState::default();
    let mut block_buf = String::new();
    let mut in_block = false;
    let mut heading_level: Option<usize> = None;
    let mut list_depth = 0usize;
    let mut pending_list_bullet = false;
    let mut current_is_list_item = false;
    let mut prev_was_list = false;
    let mut skip_depth = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) => {
                let name = local_name_start(&e);
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                if is_odf_skip_element(name) {
                    skip_depth = 1;
                    continue;
                }
                match name {
                    "table" => table.enter_table(),
                    "table-row" => table.enter_row(),
                    "table-cell" => table.enter_cell(),
                    "list" => list_depth += 1,
                    "list-item" => pending_list_bullet = true,
                    "h" => {
                        in_block = true;
                        block_buf.clear();
                        let lvl = get_attr_local(&e, "outline-level")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 6);
                        heading_level = Some(lvl);
                        current_is_list_item = false;
                    }
                    "p" => {
                        in_block = true;
                        block_buf.clear();
                        heading_level = None;
                        current_is_list_item = pending_list_bullet;
                        pending_list_bullet = false;
                    }
                    _ if in_block => handle_odf_inline(&e, &mut block_buf),
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                if skip_depth == 0 && in_block {
                    handle_odf_inline(&e, &mut block_buf);
                }
            }
            Ok(Event::End(e)) => {
                let name = e.local_name().into_inner();
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                match name {
                    "h" | "p" => {
                        in_block = false;
                        emit_markdown_paragraph(
                            &mut out,
                            &mut table,
                            &block_buf,
                            heading_level,
                            current_is_list_item,
                            list_depth.saturating_sub(1),
                            &mut prev_was_list,
                        );
                        block_buf.clear();
                        heading_level = None;
                        current_is_list_item = false;
                        if out.len() as u64 >= MAX_TEXT_PREVIEW_BYTES {
                            truncated = true;
                            break;
                        }
                    }
                    "list-item" => pending_list_bullet = false,
                    "list" => list_depth = list_depth.saturating_sub(1),
                    "table-cell" => table.leave_cell(),
                    "table-row" => {
                        table.leave_row(&mut out);
                        if out.len() as u64 >= MAX_TEXT_PREVIEW_BYTES {
                            truncated = true;
                            break;
                        }
                    }
                    "table" => {
                        if prev_was_list {
                            out.push('\n');
                            prev_was_list = false;
                        }
                        table.leave_table(&mut out);
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_block && skip_depth == 0 => {
                let s = t.xml10_content();
                let cleaned = s.replace(['\r', '\n'], "");
                block_buf.push_str(&cleaned);
            }
            Ok(Event::CData(c)) if in_block && skip_depth == 0 => {
                block_buf.push_str(&c.xml10_content());
            }
            Ok(Event::GeneralRef(r)) if in_block && skip_depth == 0 => {
                append_general_ref(&mut block_buf, &r);
            }
            _ => {}
        }
    }

    finish_markdown_output(out, truncated)
}

fn classify_pptx_placeholder(e: &BytesStart<'_>, shape_is_title: &mut bool, shape_skip: &mut bool) {
    if let Some(ph_type) = get_attr_local(e, "type") {
        match ph_type.as_str() {
            "title" | "ctrTitle" => *shape_is_title = true,
            "sldNum" | "ftr" | "dt" | "hdr" => *shape_skip = true,
            _ => {}
        }
    }
}

fn parse_pptx_slide_xml(xml: &[u8]) -> (Option<String>, Vec<String>) {
    let mut reader = Reader::from_reader(xml);
    let mut title: Option<String> = None;
    let mut body_paragraphs: Vec<String> = Vec::new();

    let mut in_shape = false;
    let mut shape_is_title = false;
    let mut shape_skip = false;
    let mut shape_paras: Vec<String> = Vec::new();
    let mut in_para = false;
    let mut in_t = false;
    let mut para_buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) => match local_name_start(&e) {
                "sp" if !in_shape => {
                    in_shape = true;
                    shape_is_title = false;
                    shape_skip = false;
                    shape_paras.clear();
                }
                "ph" if in_shape => {
                    classify_pptx_placeholder(&e, &mut shape_is_title, &mut shape_skip);
                }
                "p" if in_shape && !shape_skip => {
                    in_para = true;
                    para_buf.clear();
                }
                "t" if in_para => in_t = true,
                "br" if in_para => para_buf.push('\n'),
                _ => {}
            },
            Ok(Event::Empty(e)) => match local_name_start(&e) {
                "ph" if in_shape => {
                    classify_pptx_placeholder(&e, &mut shape_is_title, &mut shape_skip);
                }
                "br" if in_para => para_buf.push('\n'),
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().into_inner() {
                "t" => in_t = false,
                "p" if in_para => {
                    in_para = false;
                    let trimmed = para_buf.trim();
                    if !trimmed.is_empty() {
                        shape_paras.push(trimmed.to_string());
                    }
                    para_buf.clear();
                }
                "sp" if in_shape => {
                    in_shape = false;
                    if !shape_skip && !shape_paras.is_empty() {
                        if shape_is_title && title.is_none() {
                            title = Some(shape_paras.join(" "));
                        } else {
                            body_paragraphs.append(&mut shape_paras);
                        }
                    }
                    shape_paras.clear();
                }
                _ => {}
            },
            Ok(Event::Text(t)) if in_t && !shape_skip => {
                para_buf.push_str(&t.xml10_content());
            }
            Ok(Event::CData(c)) if in_t && !shape_skip => {
                para_buf.push_str(&c.xml10_content());
            }
            Ok(Event::GeneralRef(r)) if in_t && !shape_skip => {
                append_general_ref(&mut para_buf, &r);
            }
            _ => {}
        }
    }

    (title, body_paragraphs)
}

fn parse_odp_xml(xml: &[u8]) -> Vec<SlideData> {
    let mut reader = Reader::from_reader(xml);
    let mut slides: Vec<SlideData> = Vec::new();

    let mut in_page = false;
    let mut page_title: Option<String> = None;
    let mut page_body: Vec<String> = Vec::new();

    let mut in_frame = false;
    let mut frame_is_title = false;
    let mut frame_skip = false;
    let mut frame_paras: Vec<String> = Vec::new();

    let mut in_block = false;
    let mut block_buf = String::new();
    let mut skip_depth = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) => {
                let name = local_name_start(&e);
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                if is_odf_skip_element(name) || name == "notes" {
                    skip_depth = 1;
                    continue;
                }
                match name {
                    "page" if !in_page => {
                        in_page = true;
                        page_title = None;
                        page_body.clear();
                    }
                    "frame" if in_page && !in_frame => {
                        in_frame = true;
                        frame_is_title = false;
                        frame_skip = false;
                        frame_paras.clear();
                        if let Some(class) = get_attr_local(&e, "class") {
                            match class.as_str() {
                                "title" => frame_is_title = true,
                                "page-number" | "footer" | "date-time" | "header" => {
                                    frame_skip = true;
                                }
                                _ => {}
                            }
                        }
                    }
                    "p" | "h" if in_page && !frame_skip => {
                        in_block = true;
                        block_buf.clear();
                    }
                    _ if in_block => handle_odf_inline(&e, &mut block_buf),
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                if skip_depth == 0 && in_block {
                    handle_odf_inline(&e, &mut block_buf);
                }
            }
            Ok(Event::End(e)) => {
                let name = e.local_name().into_inner();
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                match name {
                    "p" | "h" if in_block => {
                        in_block = false;
                        let trimmed = block_buf.trim();
                        if !trimmed.is_empty() {
                            if in_frame {
                                frame_paras.push(trimmed.to_string());
                            } else {
                                page_body.push(trimmed.to_string());
                            }
                        }
                        block_buf.clear();
                    }
                    "frame" if in_frame => {
                        in_frame = false;
                        if !frame_skip && !frame_paras.is_empty() {
                            if frame_is_title && page_title.is_none() {
                                page_title = Some(frame_paras.join(" "));
                            } else {
                                page_body.append(&mut frame_paras);
                            }
                        }
                        frame_paras.clear();
                    }
                    "page" if in_page => {
                        in_page = false;
                        slides.push(SlideData {
                            number: slides.len() + 1,
                            title: page_title.take(),
                            body_paragraphs: std::mem::take(&mut page_body),
                        });
                        if slides.len() >= MAX_SLIDES {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_block && skip_depth == 0 => {
                let cleaned = t.xml10_content().replace(['\r', '\n'], "");
                block_buf.push_str(&cleaned);
            }
            Ok(Event::CData(c)) if in_block && skip_depth == 0 => {
                block_buf.push_str(&c.xml10_content());
            }
            Ok(Event::GeneralRef(r)) if in_block && skip_depth == 0 => {
                append_general_ref(&mut block_buf, &r);
            }
            _ => {}
        }
    }

    slides
}

// ── Metadata XML parsers ─────────────────────────────────────────────

fn insert_if_nonempty(map: &mut BTreeMap<String, String>, key: &str, val: Option<String>) {
    if let Some(v) = val {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            map.insert(key.to_string(), trimmed.to_string());
        }
    }
}

fn parse_simple_xml_tags(xml: &[u8], mut on_tag: impl FnMut(&str, &str)) {
    let mut reader = Reader::from_reader(xml);
    let mut current_tag: Option<String> = None;
    let mut buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) => {
                current_tag = Some(local_name_start(&e).to_string());
                buf.clear();
            }
            Ok(Event::End(e)) => {
                let closed = e.local_name().into_inner();
                if current_tag.as_deref() == Some(closed) {
                    let trimmed = buf.trim();
                    if !trimmed.is_empty() {
                        on_tag(closed, trimmed);
                    }
                }
                current_tag = None;
                buf.clear();
            }
            Ok(Event::Text(t)) if current_tag.is_some() => {
                buf.push_str(&t.xml10_content());
            }
            Ok(Event::CData(c)) if current_tag.is_some() => {
                buf.push_str(&c.xml10_content());
            }
            Ok(Event::GeneralRef(r)) if current_tag.is_some() => {
                append_general_ref(&mut buf, &r);
            }
            _ => {}
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

// ── RTF parser ───────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct RtfGroupState {
    skip: bool,
    uc: usize,
}

fn is_rtf_skip_destination(word: &str) -> bool {
    matches!(
        word,
        "fonttbl"
            | "colortbl"
            | "stylesheet"
            | "info"
            | "pict"
            | "object"
            | "header"
            | "footer"
            | "headerl"
            | "headerr"
            | "headerf"
            | "footerl"
            | "footerr"
            | "footerf"
            | "footnote"
            | "fldinst"
            | "listtable"
            | "listoverridetable"
            | "revtbl"
            | "rsidtbl"
            | "xmlnstbl"
            | "themedata"
            | "colorschememapping"
            | "latentstyles"
            | "datastore"
            | "generator"
            | "mmathPr"
    )
}

fn decode_cp1252_byte(b: u8) -> char {
    const HIGH_80_9F: [char; 32] = [
        '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}',
        '\u{017D}', '\u{FFFD}', '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}',
        '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
    ];
    if (0x80..=0x9F).contains(&b) {
        HIGH_80_9F[(b - 0x80) as usize]
    } else {
        char::from(b)
    }
}

fn push_rtf_char(
    out: &mut String,
    stack: &[RtfGroupState],
    skip_fallback_chars: &mut usize,
    ch: char,
) {
    if !stack.last().is_some_and(|s| s.skip) {
        if *skip_fallback_chars > 0 {
            *skip_fallback_chars -= 1;
        } else {
            out.push(ch);
        }
    }
}

fn parse_rtf_bytes(raw: &[u8]) -> String {
    const MAX_STACK_DEPTH: usize = 256;

    let mut stack = vec![RtfGroupState { skip: false, uc: 1 }];
    let mut out = String::new();
    let mut i = 0usize;
    let mut skip_fallback_chars = 0usize;
    let mut pending_surrogate: Option<u16> = None;

    while i < raw.len() {
        let b = raw[i];
        match b {
            b'{' => {
                pending_surrogate = None;
                skip_fallback_chars = 0;
                if stack.len() < MAX_STACK_DEPTH {
                    let top = *stack
                        .last()
                        .unwrap_or(&RtfGroupState { skip: false, uc: 1 });
                    stack.push(top);
                }
                i += 1;
            }
            b'}' => {
                pending_surrogate = None;
                skip_fallback_chars = 0;
                if stack.len() > 1 {
                    stack.pop();
                }
                i += 1;
            }
            b'\r' | b'\n' => {
                i += 1;
            }
            b'\\' => {
                i += 1;
                if i >= raw.len() {
                    break;
                }
                let next = raw[i];
                match next {
                    b'\\' | b'{' | b'}' => {
                        push_rtf_char(&mut out, &stack, &mut skip_fallback_chars, char::from(next));
                        i += 1;
                    }
                    b'*' => {
                        if let Some(top) = stack.last_mut() {
                            top.skip = true;
                        }
                        i += 1;
                    }
                    b'~' => {
                        push_rtf_char(&mut out, &stack, &mut skip_fallback_chars, ' ');
                        i += 1;
                    }
                    b'_' => {
                        push_rtf_char(&mut out, &stack, &mut skip_fallback_chars, '-');
                        i += 1;
                    }
                    b'-' => {
                        i += 1;
                    }
                    b'\'' => {
                        i += 1;
                        if i + 1 < raw.len()
                            && let (Some(hi), Some(lo)) =
                                (hex_nibble(raw[i]), hex_nibble(raw[i + 1]))
                        {
                            let ch = decode_cp1252_byte((hi << 4) | lo);
                            push_rtf_char(&mut out, &stack, &mut skip_fallback_chars, ch);
                            i += 2;
                        }
                    }
                    c if c.is_ascii_alphabetic() => {
                        let word_start = i;
                        while i < raw.len() && raw[i].is_ascii_alphabetic() {
                            i += 1;
                        }
                        let word = std::str::from_utf8(&raw[word_start..i]).unwrap_or("");
                        let param_start = i;
                        if i < raw.len() && (raw[i] == b'-' || raw[i].is_ascii_digit()) {
                            if raw[i] == b'-' {
                                i += 1;
                            }
                            while i < raw.len() && raw[i].is_ascii_digit() {
                                i += 1;
                            }
                        }
                        let param: Option<i64> = if i > param_start {
                            std::str::from_utf8(&raw[param_start..i])
                                .ok()
                                .and_then(|s| s.parse().ok())
                        } else {
                            None
                        };
                        // A single trailing space delimits the control word.
                        if i < raw.len() && raw[i] == b' ' {
                            i += 1;
                        }

                        if word == "bin"
                            && let Some(n) = param
                            && n > 0
                        {
                            i = i.saturating_add(n as usize).min(raw.len());
                            continue;
                        }

                        if is_rtf_skip_destination(word) {
                            if let Some(top) = stack.last_mut() {
                                top.skip = true;
                            }
                            continue;
                        }

                        if word == "uc"
                            && let Some(n) = param
                            && let Some(top) = stack.last_mut()
                        {
                            top.uc = usize::try_from(n).unwrap_or(0);
                            continue;
                        }

                        if stack.last().is_some_and(|s| s.skip) {
                            continue;
                        }

                        if word == "u"
                            && let Some(n) = param
                        {
                            let unit = if n < 0 { (n + 65_536) as u16 } else { n as u16 };
                            if (0xD800..=0xDBFF).contains(&unit) {
                                pending_surrogate = Some(unit);
                            } else if (0xDC00..=0xDFFF).contains(&unit)
                                && let Some(hi) = pending_surrogate.take()
                            {
                                let code = 0x10000
                                    + ((u32::from(hi - 0xD800) << 10) | u32::from(unit - 0xDC00));
                                if let Some(ch) = char::from_u32(code) {
                                    out.push(ch);
                                }
                            } else {
                                pending_surrogate = None;
                                if let Some(ch) = char::from_u32(u32::from(unit)) {
                                    out.push(ch);
                                }
                            }
                            skip_fallback_chars = stack.last().map_or(1, |s| s.uc);
                            continue;
                        }

                        pending_surrogate = None;
                        match word {
                            "par" | "line" | "row" => out.push('\n'),
                            "tab" | "cell" => out.push('\t'),
                            "emdash" => out.push('\u{2014}'),
                            "endash" => out.push('\u{2013}'),
                            "bullet" => out.push('\u{2022}'),
                            "lquote" => out.push('\u{2018}'),
                            "rquote" => out.push('\u{2019}'),
                            "ldblquote" => out.push('\u{201C}'),
                            "rdblquote" => out.push('\u{201D}'),
                            _ => {}
                        }
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
            _ => {
                pending_surrogate = None;
                push_rtf_char(
                    &mut out,
                    &stack,
                    &mut skip_fallback_chars,
                    decode_cp1252_byte(b),
                );
                i += 1;
            }
        }
    }

    normalize_plain_text(out)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_rtf_info_metadata(raw: &[u8], fields: &mut BTreeMap<String, String>) {
    // Only scan for `{\info ...}` in the bounded RTF header slice using raw byte offsets so
    // non-UTF8 bytes never shift slice indices.
    let Some(info_start) = find_bytes(raw, br"{\info") else {
        return;
    };
    let slice = &raw[info_start..];
    for (tag, label) in [
        (br"{\title ".as_slice(), "Title"),
        (br"{\author ".as_slice(), "Author"),
        (br"{\subject ".as_slice(), "Subject"),
    ] {
        if let Some(pos) = find_bytes(slice, tag) {
            let val_start = pos + tag.len();
            if let Some(end_rel) = slice[val_start..].iter().position(|&b| b == b'}') {
                let val_rtf = &slice[val_start..val_start + end_rel];
                let parsed = parse_rtf_bytes(val_rtf);
                let trimmed = parsed.trim();
                if !trimmed.is_empty() {
                    fields.insert(label.into(), trimmed.to_string());
                }
            }
        }
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn normalize_plain_text(input: String) -> String {
    let mut out = String::with_capacity(input.len());
    let mut blank_run = 0usize;
    for line in input.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docx_headings_lists_and_tables() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
          <w:body>
            <w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Report &amp; Summary</w:t></w:r></w:p>
            <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Section</w:t></w:r></w:p>
            <w:p><w:pPr><w:numPr/></w:pPr><w:r><w:t>First bullet</w:t></w:r></w:p>
            <w:p><w:r><w:t>Plain text</w:t><w:instrText>PAGE</w:instrText></w:r></w:p>
            <w:tbl>
              <w:tr><w:tc><w:p><w:r><w:t>Col A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Col B</w:t></w:r></w:p></w:tc></w:tr>
              <w:tr><w:tc><w:p><w:r><w:t>1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>2</w:t></w:r></w:p></w:tc></w:tr>
            </w:tbl>
          </w:body>
        </w:document>"#;

        let md = parse_docx_xml(xml);
        assert!(md.contains("# Report & Summary"));
        assert!(md.contains("## Section"));
        assert!(md.contains("- First bullet"));
        assert!(md.contains("Plain text"));
        assert!(!md.contains("PAGE"));
        assert!(md.contains("| Col A | Col B |\n| --- | --- |\n| 1 | 2 |"));
    }

    #[test]
    fn parses_odt_headings_spaces_and_lists() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                                 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
          <office:automatic-styles><text:p>IGNORED</text:p></office:automatic-styles>
          <office:body><office:text>
            <text:h text:outline-level="2">Overview</text:h>
            <text:p>Hello<text:s text:c="2"/>world</text:p>
            <text:list><text:list-item><text:p>Item one</text:p></text:list-item></text:list>
          </office:text></office:body>
        </office:document-content>"#;

        let md = parse_odt_xml(xml);
        assert!(md.contains("## Overview"));
        assert!(md.contains("Hello  world"));
        assert!(md.contains("- Item one"));
        assert!(!md.contains("IGNORED"));
    }

    #[test]
    fn parses_pptx_slide_title_and_body() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
               xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
          <p:cSld><p:spTree>
            <p:sp>
              <p:nvSpPr><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>Deck Title</a:t></a:r></a:p></p:txBody>
            </p:sp>
            <p:sp>
              <p:nvSpPr><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>Key point</a:t></a:r></a:p></p:txBody>
            </p:sp>
            <p:sp>
              <p:nvSpPr><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>12</a:t></a:r></a:p></p:txBody>
            </p:sp>
          </p:spTree></p:cSld>
        </p:sld>"#;

        let (title, body) = parse_pptx_slide_xml(xml);
        assert_eq!(title.as_deref(), Some("Deck Title"));
        assert_eq!(body, vec!["Key point".to_string()]);
    }

    #[test]
    fn parses_rtf_stripping_headers_and_decoding_escapes() {
        let rtf = br#"{\rtf1\ansi{\fonttbl{\f0 Arial;}}{\*\generator Foo}Hello\par Caf\'e9 \u8212\'97 world}"#;
        let text = parse_rtf_bytes(rtf);
        assert_eq!(text, "Hello\nCaf\u{00E9} \u{2014} world");
    }
}
