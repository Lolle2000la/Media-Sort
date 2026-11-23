use std::path::Path;

use media_sort_core::natural_sort::natural_cmp;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use super::archive::open_zip_or_unavailable;
use super::xml::{append_general_ref, get_attr_local, local_name_start};
use crate::media::document::{
    DocumentError, DocumentPreview, MAX_SLIDES, MAX_XML_ENTRY_BYTES, SlideData,
    read_zip_entry_capped,
};

/// Loads pure-Rust slide text previews from a `.pptx` archive (`ppt/slides/slide*.xml`).
pub(super) fn load_pptx_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn is_pptx_slide_entry_filters_non_slide_paths() {
        assert!(is_pptx_slide_entry("ppt/slides/slide1.xml"));
        assert!(is_pptx_slide_entry("ppt/slides/slide12.xml"));
        assert!(!is_pptx_slide_entry("ppt/slides/_rels/slide1.xml.rels"));
        assert!(!is_pptx_slide_entry("ppt/slides/slide.xml"));
        assert!(!is_pptx_slide_entry("ppt/slides/slide1a.xml"));
        assert!(!is_pptx_slide_entry("ppt/slideLayouts/slideLayout1.xml"));
    }

    #[test]
    fn pptx_skips_footer_datetime_header_and_preserves_line_breaks() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
               xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
          <p:cSld><p:spTree>
            <p:sp>
              <p:nvSpPr><p:nvPr><p:ph type="ftr"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>FooterText</a:t></a:r></a:p></p:txBody>
            </p:sp>
            <p:sp>
              <p:nvSpPr><p:nvPr><p:ph type="dt"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>2026-01-01</a:t></a:r></a:p></p:txBody>
            </p:sp>
            <p:sp>
              <p:nvSpPr><p:nvPr><p:ph type="hdr"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>HeaderText</a:t></a:r></a:p></p:txBody>
            </p:sp>
            <p:sp>
              <p:nvSpPr><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>Line A</a:t></a:r><a:br/><a:r><a:t>Line B</a:t></a:r></a:p></p:txBody>
            </p:sp>
          </p:spTree></p:cSld>
        </p:sld>"#;

        let (title, body) = parse_pptx_slide_xml(xml);
        assert!(title.is_none());
        assert_eq!(body, vec!["Line A\nLine B".to_string()]);
    }
}
