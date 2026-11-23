use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use super::archive::read_odf_content_xml;
use super::xml::{
    MarkdownTableState, append_general_ref, emit_markdown_paragraph, finish_markdown_output,
    get_attr_local, handle_odf_inline, is_odf_skip_element, local_name_start,
};
use crate::media::document::{DocumentError, DocumentPreview, MAX_TEXT_PREVIEW_BYTES};

/// Loads a pure-Rust Markdown preview from an `.odt` archive (`content.xml`).
pub(super) fn load_odt_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parses_odt_nested_lists_tables_and_skips_annotations_and_tracked_changes() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                                 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
                                 xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">
          <office:body><office:text>
            <text:tracked-changes><text:changed-region><text:deletion><text:p>OldDeleted</text:p></text:deletion></text:changed-region></text:tracked-changes>
            <text:list>
              <text:list-item>
                <text:p>Outer<office:annotation><text:p>ReviewerNote</text:p></office:annotation></text:p>
                <text:list>
                  <text:list-item><text:p>Nested</text:p></text:list-item>
                </text:list>
              </text:list-item>
            </text:list>
            <table:table>
              <table:table-row>
                <table:table-cell><text:p>Key</text:p></table:table-cell>
                <table:table-cell><text:p>Val</text:p></table:table-cell>
              </table:table-row>
              <table:table-row>
                <table:table-cell><text:p>K1</text:p></table:table-cell>
                <table:table-cell><text:p>V1</text:p></table:table-cell>
              </table:table-row>
            </table:table>
          </office:text></office:body>
        </office:document-content>"#;

        let md = parse_odt_xml(xml);
        assert!(md.contains("- Outer\n  - Nested"));
        assert!(md.contains("| Key | Val |\n| --- | --- |\n| K1 | V1 |"));
        assert!(!md.contains("OldDeleted"));
        assert!(!md.contains("ReviewerNote"));
    }
}
