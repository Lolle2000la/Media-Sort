use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Condvar, Mutex};

use office_oxide::core::Error as CoreError;
use office_oxide::core::properties::{AppProperties, CoreProperties};
use office_oxide::doc::{DocDocument, DocError};
use office_oxide::docx::DocxError;
use office_oxide::limits::Limits;
use office_oxide::options::{OnIncomplete, ParseOptions};
use office_oxide::ppt::{PptDocument, PptError, TextType};
use office_oxide::pptx::{GraphicContent, PptxDocument, PptxError, Shape, TextBody, TextContent};
use office_oxide::{Document, DocumentFormat, OfficeError};

use super::archive::{extension_lower, is_cfb_file};
use super::xml::finish_markdown_output;
use crate::media::document::{
    DocumentError, DocumentPreview, DocumentUnavailableReason, MAX_ARCHIVE_ENTRY_BYTES,
    MAX_PDF_FILE_BYTES, MAX_SLIDES, MAX_TEXT_PREVIEW_BYTES, MAX_TEXT_PREVIEW_LINES, SlideData,
};

/// Caps concurrent `office_oxide` parses so its internal 16 MiB parser threads stay bounded under
/// rapid scrolling.
const MAX_CONCURRENT_OXIDE_PARSES: usize = 4;

static OXIDE_PARSE_GATE: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

struct OxideParsePermit;

impl OxideParsePermit {
    fn acquire() -> Self {
        let (lock, cvar) = &OXIDE_PARSE_GATE;
        let mut active = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *active >= MAX_CONCURRENT_OXIDE_PARSES {
            active = cvar
                .wait(active)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *active += 1;
        Self
    }
}

impl Drop for OxideParsePermit {
    fn drop(&mut self) {
        let (lock, cvar) = &OXIDE_PARSE_GATE;
        let mut active = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.saturating_sub(1);
        drop(active);
        cvar.notify_one();
    }
}

fn bounded_parse_options() -> ParseOptions {
    let mut limits = Limits::strict();
    limits.part_bytes = MAX_ARCHIVE_ENTRY_BYTES;
    limits.package_bytes = MAX_PDF_FILE_BYTES;
    limits.text_chars = 1_000_000;
    limits.memory_bytes = 128 * 1024 * 1024;

    let mut options = ParseOptions::default();
    options.limits = limits;
    options.on_incomplete = OnIncomplete::Report;
    options.lazy_pictures = true;
    options
}

fn open_oxide_document(
    path: &Path,
) -> Result<Result<Document, DocumentUnavailableReason>, DocumentError> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_PDF_FILE_BYTES {
        return Ok(Err(DocumentUnavailableReason::TooLarge));
    }

    let options = bounded_parse_options();
    let _permit = OxideParsePermit::acquire();
    match Document::open_with(path, &options) {
        Ok(doc) => Ok(Ok(doc)),
        Err(err) => Ok(Err(map_office_error(path, &err))),
    }
}

fn map_office_error(path: &Path, err: &OfficeError) -> DocumentUnavailableReason {
    if is_encrypted_office_error(err) {
        return DocumentUnavailableReason::Encrypted;
    }
    let ext = extension_lower(path);
    if matches!(ext.as_str(), "docx" | "pptx") && is_cfb_file(path) {
        return DocumentUnavailableReason::Encrypted;
    }
    if is_too_large_office_error(err) {
        return DocumentUnavailableReason::TooLarge;
    }
    DocumentUnavailableReason::UnsupportedOrCorrupt(err.to_string())
}

fn is_encrypted_msg(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("encrypted") || lower.contains("password-protected")
}

fn is_encrypted_office_error(err: &OfficeError) -> bool {
    match err {
        OfficeError::Doc(DocError::Encrypted) | OfficeError::Ppt(PptError::Encrypted) => true,
        OfficeError::Docx(DocxError::Core(CoreError::Unsupported(msg)))
        | OfficeError::Pptx(PptxError::Core(CoreError::Unsupported(msg)))
        | OfficeError::Core(CoreError::Unsupported(msg))
        | OfficeError::UnsupportedFormat(msg) => is_encrypted_msg(msg),
        _ => is_encrypted_msg(&err.to_string()),
    }
}

fn is_too_large_office_error(err: &OfficeError) -> bool {
    matches!(
        err,
        OfficeError::Core(CoreError::DecompressionLimit { .. } | CoreError::PackageLimit(_))
            | OfficeError::Docx(DocxError::Core(
                CoreError::DecompressionLimit { .. } | CoreError::PackageLimit(_)
            ))
            | OfficeError::Pptx(PptxError::Core(
                CoreError::DecompressionLimit { .. } | CoreError::PackageLimit(_)
            ))
    )
}

/// Loads a pure-Rust preview for `.docx`, `.doc`, `.pptx`, or `.ppt` via `office_oxide`.
pub(super) fn load_oxide_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let doc = match open_oxide_document(path)? {
        Ok(doc) => doc,
        Err(reason) => return Ok(DocumentPreview::Unavailable { reason }),
    };

    match doc.format() {
        DocumentFormat::Docx | DocumentFormat::Doc => {
            let md = doc.to_markdown();
            let raw = if md.trim().is_empty() {
                doc.plain_text()
            } else {
                md
            };
            let markdown = format_bounded_markdown(&raw);
            Ok(DocumentPreview::Markdown {
                markdown,
                chapter_index: None,
                total_chapters: None,
                cover: None,
            })
        }
        DocumentFormat::Pptx => {
            let slides = doc.as_pptx().map(extract_pptx_slides).unwrap_or_default();
            Ok(DocumentPreview::Slides { slides })
        }
        DocumentFormat::Ppt => {
            let slides = doc.as_ppt().map(extract_ppt_slides).unwrap_or_default();
            Ok(DocumentPreview::Slides { slides })
        }
        _ => Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::NoPreviewBackend,
        }),
    }
}

/// Extracts metadata fields (`Title`, `Author`, `Subject`, `Created`, `Pages`, `Slides`, or
/// `Encrypted`) for `.docx`, `.doc`, `.pptx`, and `.ppt` via `office_oxide`.
pub(super) fn extract_oxide_metadata(
    path: &Path,
) -> Result<BTreeMap<String, String>, DocumentError> {
    let mut fields = BTreeMap::new();
    let doc = match open_oxide_document(path)? {
        Ok(doc) => doc,
        Err(DocumentUnavailableReason::Encrypted) => {
            fields.insert("Encrypted".into(), "Yes".into());
            return Ok(fields);
        }
        Err(DocumentUnavailableReason::TooLarge) => {
            return Err(DocumentError::Message(
                "Office document exceeds size limit".to_string(),
            ));
        }
        Err(DocumentUnavailableReason::UnsupportedOrCorrupt(msg)) => {
            return Err(DocumentError::Message(msg));
        }
        Err(
            DocumentUnavailableReason::NoPreviewBackend
            | DocumentUnavailableReason::BinaryOrUnreadable,
        ) => {
            return Err(DocumentError::Message(
                "Unsupported Office document".to_string(),
            ));
        }
    };

    if let Some(docx) = doc.as_docx() {
        insert_ooxml_props(
            &mut fields,
            docx.core_properties.as_ref(),
            docx.app_properties.as_ref(),
        );
    } else if let Some(pptx) = doc.as_pptx() {
        insert_ooxml_props(
            &mut fields,
            pptx.core_properties.as_ref(),
            pptx.app_properties.as_ref(),
        );
        if !fields.contains_key("Slides") && !pptx.slides.is_empty() {
            fields.insert("Slides".into(), pptx.slides.len().to_string());
        }
    } else if let Some(doc_bin) = doc.as_doc() {
        insert_summary_props(&mut fields, doc_bin);
    } else if let Some(ppt_bin) = doc.as_ppt() {
        insert_ppt_summary_props(&mut fields, ppt_bin);
    }

    Ok(fields)
}

fn insert_trimmed(fields: &mut BTreeMap<String, String>, key: &str, val: Option<&str>) {
    if let Some(v) = val {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            fields.insert(key.to_string(), trimmed.to_string());
        }
    }
}

fn insert_ooxml_props(
    fields: &mut BTreeMap<String, String>,
    core: Option<&CoreProperties>,
    app: Option<&AppProperties>,
) {
    if let Some(core) = core {
        insert_trimmed(fields, "Title", core.title.as_deref());
        insert_trimmed(fields, "Author", core.creator.as_deref());
        insert_trimmed(fields, "Subject", core.subject.as_deref());
        insert_trimmed(fields, "Created", core.created.as_deref());
    }
    if let Some(app) = app {
        if let Some(pages) = app.pages {
            fields.insert("Pages".into(), pages.to_string());
        }
        if let Some(slides) = app.slides {
            fields.insert("Slides".into(), slides.to_string());
        }
    }
}

fn insert_summary_props(fields: &mut BTreeMap<String, String>, doc: &DocDocument) {
    if let Some(summary) = doc.summary_properties() {
        insert_trimmed(fields, "Title", summary.title.as_deref());
        insert_trimmed(fields, "Author", summary.author.as_deref());
        insert_trimmed(fields, "Subject", summary.subject.as_deref());
        insert_trimmed(fields, "Created", summary.created.as_deref());
    }
}

fn insert_ppt_summary_props(fields: &mut BTreeMap<String, String>, ppt: &PptDocument) {
    if let Some(summary) = ppt.summary_properties() {
        insert_trimmed(fields, "Title", summary.title.as_deref());
        insert_trimmed(fields, "Author", summary.author.as_deref());
        insert_trimmed(fields, "Subject", summary.subject.as_deref());
        insert_trimmed(fields, "Created", summary.created.as_deref());
    }
    if !ppt.slides.is_empty() {
        fields.insert("Slides".into(), ppt.slides.len().to_string());
    }
}

fn format_bounded_markdown(raw: &str) -> String {
    let mut normalized = String::new();
    let mut blank_run = 0usize;
    let mut line_count = 0usize;
    let mut truncated = false;

    for raw_line in raw.lines() {
        let line = raw_line.trim_end();
        if line.is_empty() {
            if normalized.is_empty() || blank_run >= 1 {
                continue;
            }
            blank_run += 1;
        } else {
            blank_run = 0;
        }

        if line_count >= MAX_TEXT_PREVIEW_LINES {
            truncated = true;
            break;
        }

        let extra_bytes = usize::from(!normalized.is_empty()) + line.len();
        if (normalized.len() + extra_bytes) as u64 > MAX_TEXT_PREVIEW_BYTES {
            if normalized.is_empty() {
                let prefix_end = line.floor_char_boundary(MAX_TEXT_PREVIEW_BYTES as usize);
                normalized.push_str(&line[..prefix_end]);
            }
            truncated = true;
            break;
        }

        if !normalized.is_empty() {
            normalized.push('\n');
        }
        normalized.push_str(line);
        line_count += 1;
    }

    finish_markdown_output(normalized, truncated)
}

fn is_chrome_placeholder(ph_type: Option<&str>) -> bool {
    matches!(ph_type, Some("sldNum" | "ftr" | "dt" | "hdr"))
}

fn is_title_placeholder(ph_type: Option<&str>) -> bool {
    matches!(ph_type, Some("title" | "ctrTitle"))
}

fn text_body_paragraphs(body: &TextBody) -> Vec<String> {
    let mut paras = Vec::new();
    for para in &body.paragraphs {
        let mut buf = String::new();
        for content in &para.content {
            match content {
                TextContent::Run(run) => buf.push_str(&run.text),
                TextContent::LineBreak => buf.push('\n'),
                TextContent::Field(field) => buf.push_str(&field.text),
            }
        }
        let trimmed = buf.trim();
        if !trimmed.is_empty() {
            paras.push(trimmed.to_string());
        }
    }
    paras
}

fn collect_pptx_shapes(
    shapes: &[Shape],
    title: &mut Option<String>,
    body_paragraphs: &mut Vec<String>,
) {
    for shape in shapes {
        match shape {
            Shape::AutoShape(auto) => {
                let ph = auto.placeholder.as_ref().and_then(|p| p.ph_type.as_deref());
                if is_chrome_placeholder(ph) {
                    continue;
                }
                if let Some(ref tb) = auto.text_body {
                    let paras = text_body_paragraphs(tb);
                    if paras.is_empty() {
                        continue;
                    }
                    if is_title_placeholder(ph) && title.is_none() {
                        *title = Some(paras.join(" "));
                    } else {
                        body_paragraphs.extend(paras);
                    }
                }
            }
            Shape::Group(grp) => {
                collect_pptx_shapes(&grp.children, title, body_paragraphs);
            }
            Shape::GraphicFrame(gf) => match &gf.content {
                GraphicContent::Table(table) => {
                    for row in &table.rows {
                        let cells: Vec<String> = row
                            .cells
                            .iter()
                            .filter(|c| !c.h_merge && !c.v_merge)
                            .map(|cell| {
                                cell.text_body
                                    .as_ref()
                                    .map(|tb| text_body_paragraphs(tb).join(" "))
                                    .unwrap_or_default()
                            })
                            .collect();
                        if cells.iter().any(|c| !c.is_empty()) {
                            body_paragraphs.push(cells.join(" | "));
                        }
                    }
                }
                GraphicContent::Text(lines) => {
                    for line in lines.iter() {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            body_paragraphs.push(trimmed.to_string());
                        }
                    }
                }
                GraphicContent::OleObject(_) | GraphicContent::Unknown => {}
            },
            Shape::Picture(_) | Shape::Connector(_) => {}
        }
    }
}

fn extract_pptx_slide(idx: usize, slide: &office_oxide::pptx::Slide) -> SlideData {
    if let Some(ref err) = slide.parse_error {
        return SlideData {
            number: idx + 1,
            title: (!slide.name.trim().is_empty()).then(|| slide.name.trim().to_string()),
            body_paragraphs: vec![format!("[Unreadable slide: {err}]")],
        };
    }

    let mut title = None;
    let mut body_paragraphs = Vec::new();
    collect_pptx_shapes(&slide.shapes, &mut title, &mut body_paragraphs);

    if let Some(ref notes) = slide.notes {
        let note_paras = text_body_paragraphs(notes);
        if !note_paras.is_empty() {
            body_paragraphs.push(format!("Notes: {}", note_paras.join("\n")));
        }
    }

    SlideData {
        number: idx + 1,
        title,
        body_paragraphs,
    }
}

fn extract_pptx_slides(pptx: &PptxDocument) -> Vec<SlideData> {
    pptx.slides
        .iter()
        .take(MAX_SLIDES)
        .enumerate()
        .map(|(idx, slide)| extract_pptx_slide(idx, slide))
        .collect()
}

fn split_ppt_run_paragraphs(raw: &str) -> Vec<String> {
    raw.split('\r')
        .map(|seg| seg.replace('\u{b}', "\n"))
        .map(|seg| seg.trim().to_string())
        .filter(|seg| !seg.is_empty())
        .collect()
}

fn extract_ppt_slide(idx: usize, slide: &office_oxide::ppt::SlideText) -> SlideData {
    let mut title: Option<String> = None;
    let mut body_paragraphs: Vec<String> = Vec::new();
    let mut notes_paras: Vec<String> = Vec::new();

    for run in &slide.text_runs {
        if is_chrome_placeholder(run.placeholder_role.as_deref()) {
            continue;
        }
        let paras = split_ppt_run_paragraphs(&run.text);
        if paras.is_empty() {
            continue;
        }
        match run.text_type {
            TextType::Title | TextType::CenterTitle if title.is_none() => {
                title = Some(paras.join(" "));
            }
            TextType::Notes => notes_paras.extend(paras),
            _ => body_paragraphs.extend(paras),
        }
    }

    for table in &slide.tables {
        for row in &table.rows {
            let cells: Vec<String> = row
                .iter()
                .map(|cell_runs| {
                    cell_runs
                        .iter()
                        .flat_map(|r| split_ppt_run_paragraphs(&r.text))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            if cells.iter().any(|c| !c.is_empty()) {
                body_paragraphs.push(cells.join(" | "));
            }
        }
    }

    if !notes_paras.is_empty() {
        body_paragraphs.push(format!("Notes: {}", notes_paras.join("\n")));
    }

    SlideData {
        number: idx + 1,
        title,
        body_paragraphs,
    }
}

fn extract_ppt_slides(ppt: &PptDocument) -> Vec<SlideData> {
    ppt.slides
        .iter()
        .take(MAX_SLIDES)
        .enumerate()
        .map(|(idx, slide)| extract_ppt_slide(idx, slide))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ms_oxide_test_{label}_{}_{seq}",
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
    fn concurrency_gate_caps_in_flight_permits() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));

        let threads: Vec<_> = (0..10)
            .map(|_| {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                let completed = Arc::clone(&completed);
                std::thread::spawn(move || {
                    let _permit = OxideParsePermit::acquire();
                    let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(cur, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    active.fetch_sub(1, Ordering::SeqCst);
                    completed.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();

        for t in threads {
            t.join().expect("gate test thread");
        }

        assert_eq!(completed.load(Ordering::SeqCst), 10);
        assert!(peak.load(Ordering::SeqCst) <= MAX_CONCURRENT_OXIDE_PARSES);
    }

    #[test]
    fn format_bounded_markdown_normalizes_blank_lines_and_truncates() {
        let input = "\n\n# Title\n\n\n\nParagraph 1   \n\nParagraph 2\n\n";
        assert_eq!(
            format_bounded_markdown(input),
            "# Title\n\nParagraph 1\n\nParagraph 2"
        );

        let huge_lines = (0..200)
            .map(|i| format!("Line {i}: {}", "X".repeat(2048)))
            .collect::<Vec<_>>()
            .join("\n");
        let truncated = format_bounded_markdown(&huge_lines);
        assert!(truncated.ends_with("---\n*(Preview truncated)*"));
        assert!(truncated.len() as u64 <= MAX_TEXT_PREVIEW_BYTES + 64);

        let too_many_lines = (0..(MAX_TEXT_PREVIEW_LINES + 50))
            .map(|i| format!("Row {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let line_truncated = format_bounded_markdown(&too_many_lines);
        assert!(line_truncated.ends_with("---\n*(Preview truncated)*"));
    }

    #[test]
    fn load_oxide_preview_handles_encrypted_and_corrupt_files() {
        let dir = TempDir::new("errors");

        // Encrypted OOXML wrapper (CFB magic on .docx).
        let enc_docx = dir.0.join("encrypted.docx");
        std::fs::write(&enc_docx, [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]).unwrap();
        assert!(matches!(
            load_oxide_preview(&enc_docx).expect("encrypted docx ok"),
            DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::Encrypted
            }
        ));

        // Corrupt .doc and .ppt return UnsupportedOrCorrupt.
        let bad_doc = dir.0.join("corrupt.doc");
        std::fs::write(&bad_doc, b"not a valid compound binary doc").unwrap();
        assert!(matches!(
            load_oxide_preview(&bad_doc).expect("corrupt doc ok"),
            DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(_)
            }
        ));

        let bad_ppt = dir.0.join("corrupt.ppt");
        std::fs::write(&bad_ppt, b"not a valid compound binary ppt").unwrap();
        assert!(matches!(
            load_oxide_preview(&bad_ppt).expect("corrupt ppt ok"),
            DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(_)
            }
        ));

        // Missing file returns Err(DocumentError::Io).
        let missing = dir.0.join("missing.docx");
        assert!(load_oxide_preview(&missing).is_err());
    }

    #[test]
    fn extract_oxide_metadata_reports_encrypted_and_corrupt() {
        let dir = TempDir::new("meta_errors");

        let enc_pptx = dir.0.join("encrypted.pptx");
        std::fs::write(&enc_pptx, [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]).unwrap();
        let meta = extract_oxide_metadata(&enc_pptx).expect("encrypted pptx metadata");
        assert_eq!(meta.get("Encrypted").map(String::as_str), Some("Yes"));

        let bad_doc = dir.0.join("bad.doc");
        std::fs::write(&bad_doc, b"garbage").unwrap();
        assert!(extract_oxide_metadata(&bad_doc).is_err());
    }

    #[test]
    fn extract_pptx_and_ppt_slide_helpers_filter_chrome_and_include_notes() {
        use office_oxide::ppt::{SlideText, TextRun};
        use office_oxide::pptx::{
            AutoShape, PlaceholderInfo, Slide, TextBody, TextContent, TextParagraph,
            TextRun as PptxRun,
        };

        let make_body = |text: &str| TextBody {
            paragraphs: vec![TextParagraph {
                content: vec![TextContent::Run(PptxRun {
                    text: text.to_string(),
                    ..Default::default()
                })],
                ..Default::default()
            }],
        };

        let make_shape = |id: u32, ph_type: &str, text: &str| {
            Shape::AutoShape(AutoShape {
                id,
                name: format!("shape{id}"),
                alt_text: None,
                position: None,
                placeholder: Some(PlaceholderInfo {
                    ph_type: Some(ph_type.to_string()),
                    idx: None,
                }),
                text_body: Some(make_body(text)),
                hyperlink: None,
            })
        };

        let slide1 = Slide {
            name: "s1".to_string(),
            shapes: vec![
                make_shape(1, "title", "Q4 Review"),
                make_shape(2, "ftr", "Confidential"),
                make_shape(3, "body", "Revenue grew"),
            ],
            notes: Some(std::sync::Arc::new(make_body("Speak slowly"))),
            ..Default::default()
        };
        let extracted_pptx = extract_pptx_slide(0, &slide1);
        assert_eq!(extracted_pptx.number, 1);
        assert_eq!(extracted_pptx.title.as_deref(), Some("Q4 Review"));
        assert_eq!(
            extracted_pptx.body_paragraphs,
            vec![
                "Revenue grew".to_string(),
                "Notes: Speak slowly".to_string()
            ]
        );

        let slide_text = SlideText {
            text_runs: vec![
                TextRun {
                    text_type: TextType::CenterTitle,
                    text: "Legacy Title".to_string(),
                    placeholder_role: Some("ctrTitle".to_string()),
                    ..Default::default()
                },
                TextRun {
                    text_type: TextType::Other,
                    text: "99".to_string(),
                    placeholder_role: Some("sldNum".to_string()),
                    ..Default::default()
                },
                TextRun {
                    text_type: TextType::Body,
                    text: "Line 1\rLine 2\u{b}Soft".to_string(),
                    ..Default::default()
                },
                TextRun {
                    text_type: TextType::Notes,
                    text: "Speaker memo".to_string(),
                    ..Default::default()
                },
            ],
            tables: Vec::new(),
            image_refs: Vec::new(),
            ole_object_refs: Vec::new(),
            hidden: false,
        };
        let extracted_ppt = extract_ppt_slide(0, &slide_text);
        assert_eq!(extracted_ppt.number, 1);
        assert_eq!(extracted_ppt.title.as_deref(), Some("Legacy Title"));
        assert_eq!(
            extracted_ppt.body_paragraphs,
            vec![
                "Line 1".to_string(),
                "Line 2\nSoft".to_string(),
                "Notes: Speaker memo".to_string()
            ]
        );
    }
}
