use std::collections::BTreeMap;

use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};

use crate::media::document::decode_xml_bytes;

fn push_xml_ref(out: &mut String, r: &BytesRef<'_>) {
    if let Ok(Some(ch)) = r.resolve_char_ref() {
        out.push(ch);
    } else if let Some(s) = quick_xml::escape::resolve_xml_entity(r.as_ref()) {
        out.push_str(s);
    } else {
        out.push('&');
        out.push_str(r.as_ref());
        out.push(';');
    }
}

fn comic_info_field_name(local: &str) -> Option<&'static str> {
    match local {
        "Title" => Some("Title"),
        "Series" => Some("Series"),
        "Number" => Some("Number"),
        "Writer" => Some("Writer"),
        "Publisher" => Some("Publisher"),
        "Year" => Some("Year"),
        "Genre" => Some("Genre"),
        _ => None,
    }
}

pub(super) fn parse_comic_info_xml(bytes: &[u8], fields: &mut BTreeMap<String, String>) {
    let xml = decode_xml_bytes(bytes);
    let mut reader = Reader::from_str(&xml);
    let cfg = reader.config_mut();
    cfg.check_end_names = false;
    cfg.allow_dangling_amp = true;
    cfg.allow_unmatched_ends = true;

    let mut depth: usize = 0;
    let mut comic_info_depth: Option<usize> = None;
    let mut active_field: Option<(&'static str, usize)> = None;
    let mut text_buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                depth += 1;
                let local = e.local_name().into_inner();
                if comic_info_depth.is_none() && local.eq_ignore_ascii_case("ComicInfo") {
                    comic_info_depth = Some(depth);
                } else if comic_info_depth.is_some_and(|root| depth == root + 1)
                    && let Some(key) = comic_info_field_name(local)
                {
                    active_field = Some((key, depth));
                    text_buf.clear();
                }
            }
            Ok(Event::End(e)) => {
                if let Some((key, tag_depth)) = active_field
                    && depth == tag_depth
                {
                    let trimmed = text_buf.trim();
                    if !trimmed.is_empty() {
                        fields
                            .entry(key.to_string())
                            .or_insert_with(|| trimmed.to_string());
                    }
                    active_field = None;
                    text_buf.clear();
                }
                if comic_info_depth == Some(depth)
                    && e.local_name()
                        .into_inner()
                        .eq_ignore_ascii_case("ComicInfo")
                {
                    comic_info_depth = None;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(t)) => {
                if active_field.is_some() {
                    text_buf.push_str(&t.xml10_content());
                }
            }
            Ok(Event::CData(c)) => {
                if active_field.is_some() {
                    text_buf.push_str(&c.xml10_content());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if active_field.is_some() {
                    push_xml_ref(&mut text_buf, &r);
                }
            }
            Ok(Event::Eof) => break,
            Err(quick_xml::Error::IllFormed(_)) => {}
            Err(_) => break,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_utf16_be(s: &str) -> Vec<u8> {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in s.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        bytes
    }

    fn encode_utf16_le(s: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in s.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn parses_utf8_comic_info_and_unescapes_entities() {
        let xml = b"\xEF\xBB\xBF<ComicInfo>
            <Title>Dusk &amp; Dawn &#39;99&#x21;</Title>
            <Series>Night &lt;Sky&gt;</Series>
            <Number>12</Number>
            <Writer>Jane Doe</Writer>
            <Publisher>Indie Press</Publisher>
            <Year>2024</Year>
            <Genre>Sci-Fi</Genre>
            <Nested><Title>Ignored Nested</Title></Nested>
        </ComicInfo>";

        let mut fields = BTreeMap::new();
        parse_comic_info_xml(xml, &mut fields);

        assert_eq!(
            fields.get("Title").map(String::as_str),
            Some("Dusk & Dawn '99!")
        );
        assert_eq!(
            fields.get("Series").map(String::as_str),
            Some("Night <Sky>")
        );
        assert_eq!(fields.get("Number").map(String::as_str), Some("12"));
        assert_eq!(fields.get("Writer").map(String::as_str), Some("Jane Doe"));
        assert_eq!(
            fields.get("Publisher").map(String::as_str),
            Some("Indie Press")
        );
        assert_eq!(fields.get("Year").map(String::as_str), Some("2024"));
        assert_eq!(fields.get("Genre").map(String::as_str), Some("Sci-Fi"));
    }

    #[test]
    fn parses_utf16_be_and_le_comic_info() {
        let xml_be = "<ComicInfo><Title>UTF16 BE</Title><Year>2025</Year></ComicInfo>";
        let mut be_fields = BTreeMap::new();
        parse_comic_info_xml(&encode_utf16_be(xml_be), &mut be_fields);
        assert_eq!(be_fields.get("Title").map(String::as_str), Some("UTF16 BE"));
        assert_eq!(be_fields.get("Year").map(String::as_str), Some("2025"));

        let xml_le = "<ComicInfo><Title>UTF16 LE</Title><Series>Vol</Series></ComicInfo>";
        let mut le_fields = BTreeMap::new();
        parse_comic_info_xml(&encode_utf16_le(xml_le), &mut le_fields);
        assert_eq!(le_fields.get("Title").map(String::as_str), Some("UTF16 LE"));
        assert_eq!(le_fields.get("Series").map(String::as_str), Some("Vol"));
    }

    #[test]
    fn malformed_xml_falls_back_gracefully() {
        let malformed =
            b"<ComicInfo><Title>Good & Bad &unknown;</Title><Writer>Still Parsed</Mismatched><Number>5";
        let mut fields = BTreeMap::new();
        parse_comic_info_xml(malformed, &mut fields);

        assert_eq!(
            fields.get("Title").map(String::as_str),
            Some("Good & Bad &unknown;")
        );
        assert_eq!(
            fields.get("Writer").map(String::as_str),
            Some("Still Parsed")
        );
        assert!(!fields.contains_key("Number"));
    }
}
