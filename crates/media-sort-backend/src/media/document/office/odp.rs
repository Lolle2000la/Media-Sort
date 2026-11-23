use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use super::archive::read_odf_content_xml;
use super::xml::{
    append_general_ref, get_attr_local, handle_odf_inline, is_odf_skip_element, local_name_start,
};
use crate::media::document::{DocumentError, DocumentPreview, MAX_SLIDES, SlideData};

/// Loads pure-Rust slide text previews from an `.odp` archive (`content.xml`).
pub(super) fn load_odp_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let xml = match read_odf_content_xml(path)? {
        Ok(bytes) => bytes,
        Err(preview) => return Ok(preview),
    };

    let slides = parse_odp_xml(&xml);
    Ok(DocumentPreview::Slides { slides })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_odp_slides_and_skips_notes_and_chrome_placeholders() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <office:document-content
            xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
            xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
            xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
            xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
          <office:body><office:presentation>
            <draw:page draw:name="page1">
              <draw:frame presentation:class="title"><draw:text-box><text:p>Intro</text:p></draw:text-box></draw:frame>
              <draw:frame presentation:class="outline"><draw:text-box><text:p>Bullet 1</text:p></draw:text-box></draw:frame>
              <draw:frame presentation:class="footer"><draw:text-box><text:p>Confidential</text:p></draw:text-box></draw:frame>
              <draw:frame presentation:class="page-number"><draw:text-box><text:p>1</text:p></draw:text-box></draw:frame>
              <draw:frame presentation:class="date-time"><draw:text-box><text:p>2026</text:p></draw:text-box></draw:frame>
              <draw:frame presentation:class="header"><draw:text-box><text:p>Hdr</text:p></draw:text-box></draw:frame>
              <presentation:notes><draw:frame><draw:text-box><text:p>Speaker note</text:p></draw:text-box></draw:frame></presentation:notes>
            </draw:page>
            <draw:page draw:name="page2">
              <draw:frame presentation:class="title"><draw:text-box><text:p>Second</text:p></draw:text-box></draw:frame>
            </draw:page>
          </office:presentation></office:body>
        </office:document-content>"#;

        let slides = parse_odp_xml(xml);
        assert_eq!(slides.len(), 2);
        assert_eq!(slides[0].number, 1);
        assert_eq!(slides[0].title.as_deref(), Some("Intro"));
        assert_eq!(slides[0].body_paragraphs, vec!["Bullet 1".to_string()]);
        assert_eq!(slides[1].number, 2);
        assert_eq!(slides[1].title.as_deref(), Some("Second"));
        assert!(slides[1].body_paragraphs.is_empty());
    }

    #[test]
    fn odp_caps_slides_at_max_slides() {
        let mut xml = String::from(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:presentation>"#,
        );
        for i in 1..=(MAX_SLIDES + 5) {
            xml.push_str(&format!(
                r#"<draw:page><draw:frame presentation:class="title"><draw:text-box><text:p>Slide {i}</text:p></draw:text-box></draw:frame></draw:page>"#
            ));
        }
        xml.push_str("</office:presentation></office:body></office:document-content>");

        let slides = parse_odp_xml(xml.as_bytes());
        assert_eq!(slides.len(), MAX_SLIDES);
        assert_eq!(slides[0].number, 1);
        assert_eq!(slides[MAX_SLIDES - 1].number, MAX_SLIDES);
    }
}
