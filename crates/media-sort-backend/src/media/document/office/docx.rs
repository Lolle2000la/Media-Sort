use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use super::archive::open_zip_or_unavailable;
use super::xml::{
    MarkdownTableState, append_general_ref, emit_markdown_paragraph, finish_markdown_output,
    get_attr_local, local_name_start,
};
use crate::media::document::{
    DocumentError, DocumentPreview, DocumentUnavailableReason, MAX_TEXT_PREVIEW_BYTES,
    MAX_XML_ENTRY_BYTES, read_zip_entry_capped,
};

/// Loads a pure-Rust Markdown preview from a `.docx` archive (`word/document.xml`).
pub(super) fn load_docx_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
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
    fn docx_heading_level_maps_styles_and_bounds() {
        assert_eq!(docx_heading_level("Title"), Some(1));
        assert_eq!(docx_heading_level("  subtitle "), Some(2));
        assert_eq!(docx_heading_level("Heading1"), Some(1));
        assert_eq!(docx_heading_level("heading 6"), Some(6));
        assert_eq!(docx_heading_level("Heading0"), None);
        assert_eq!(docx_heading_level("Heading7"), None);
        assert_eq!(docx_heading_level("Normal"), None);
    }

    #[test]
    fn docx_skips_del_text_and_fallback_and_handles_breaks_and_tabs() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                    xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">
          <w:body>
            <w:p>
              <w:r><w:t>Keep</w:t><w:tab/><w:t>Here</w:t><w:br/><w:t>Line2</w:t><w:cr/><w:t>Line3</w:t></w:r>
              <w:del><w:r><w:delText>Deleted</w:delText></w:r></w:del>
              <mc:AlternateContent><mc:Fallback><w:r><w:t>FallbackText</w:t></w:r></mc:Fallback></mc:AlternateContent>
            </w:p>
          </w:body>
        </w:document>"#;

        let md = parse_docx_xml(xml);
        assert_eq!(md, "Keep\tHere\nLine2\nLine3");
        assert!(!md.contains("Deleted"));
        assert!(!md.contains("FallbackText"));
    }

    #[test]
    fn docx_truncates_at_max_text_preview_bytes() {
        let chunk = "A".repeat(4096);
        let mut xml = String::from(
            r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
        );
        for _ in 0..80 {
            xml.push_str("<w:p><w:r><w:t>");
            xml.push_str(&chunk);
            xml.push_str("</w:t></w:r></w:p>");
        }
        xml.push_str("</w:body></w:document>");

        let md = parse_docx_xml(xml.as_bytes());
        assert!(md.ends_with("---\n*(Preview truncated)*"));
    }
}
