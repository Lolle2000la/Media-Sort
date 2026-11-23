use quick_xml::XmlVersion;
use quick_xml::events::{BytesRef, BytesStart};
use quick_xml::reader::Reader;

/// Creates a permissive `quick_xml` reader tuned for real-world malformed EPUB XML/XHTML.
pub(super) fn tolerant_reader(xml: &str) -> Reader<&[u8]> {
    let mut reader = Reader::from_str(xml);
    let cfg = reader.config_mut();
    cfg.check_end_names = false;
    cfg.allow_dangling_amp = true;
    cfg.allow_unmatched_ends = true;
    reader
}

/// Returns the normalized/unescaped value of the first attribute matching `name` by
/// case-insensitive local name (ignoring XML namespace prefixes).
pub(super) fn attr_value_local(start: &BytesStart<'_>, name: &str) -> Option<String> {
    for attr_res in start.attributes().with_checks(false) {
        let Ok(attr) = attr_res else {
            continue;
        };
        if attr
            .key
            .local_name()
            .into_inner()
            .eq_ignore_ascii_case(name)
        {
            let val = match attr.normalized_value(XmlVersion::Implicit1_0) {
                Ok(v) => v.into_owned(),
                Err(_) => quick_xml::escape::unescape_with(&attr.value, resolve_html_entity)
                    .map(|v| v.into_owned())
                    .unwrap_or_else(|_| attr.value.into_owned()),
            };
            return Some(val);
        }
    }
    None
}

/// Resolves an XML/HTML entity or numeric character reference into `out`, preserving
/// unrecognized references verbatim.
pub(super) fn push_entity_ref(out: &mut String, r: &BytesRef<'_>) {
    if let Ok(Some(ch)) = r.resolve_char_ref() {
        out.push(ch);
    } else if let Some(s) = resolve_html_entity(r.as_ref()) {
        out.push_str(s);
    } else {
        out.push('&');
        out.push_str(r.as_ref());
        out.push(';');
    }
}

/// Resolves standard XML and common HTML4/XHTML named character entities.
pub(super) fn resolve_html_entity(entity: &str) -> Option<&'static str> {
    if let Some(xml) = quick_xml::escape::resolve_xml_entity(entity) {
        return Some(xml);
    }
    let resolved = match entity {
        "nbsp" => "\u{00A0}",
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "thinsp" => "\u{2009}",
        "shy" => "",
        "zwj" => "\u{200D}",
        "zwnj" => "\u{200C}",
        "ndash" => "\u{2013}",
        "mdash" => "\u{2014}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "sbquo" => "\u{201A}",
        "ldquo" => "\u{201C}",
        "rdquo" => "\u{201D}",
        "bdquo" => "\u{201E}",
        "laquo" => "\u{00AB}",
        "raquo" => "\u{00BB}",
        "lsaquo" => "\u{2039}",
        "rsaquo" => "\u{203A}",
        "hellip" => "\u{2026}",
        "bull" => "\u{2022}",
        "middot" => "\u{00B7}",
        "copy" => "\u{00A9}",
        "reg" => "\u{00AE}",
        "trade" => "\u{2122}",
        "deg" => "\u{00B0}",
        "sect" => "\u{00A7}",
        "para" => "\u{00B6}",
        "dagger" => "\u{2020}",
        "Dagger" => "\u{2021}",
        "times" => "\u{00D7}",
        "divide" => "\u{00F7}",
        "plusmn" => "\u{00B1}",
        "minus" => "\u{2212}",
        "ne" => "\u{2260}",
        "le" => "\u{2264}",
        "ge" => "\u{2265}",
        "approx" => "\u{2248}",
        "infin" => "\u{221E}",
        "euro" => "\u{20AC}",
        "pound" => "\u{00A3}",
        "yen" => "\u{00A5}",
        "cent" => "\u{00A2}",
        "frac12" => "\u{00BD}",
        "frac14" => "\u{00BC}",
        "frac34" => "\u{00BE}",
        "iexcl" => "\u{00A1}",
        "iquest" => "\u{00BF}",
        "Agrave" => "\u{00C0}",
        "Aacute" => "\u{00C1}",
        "Acirc" => "\u{00C2}",
        "Atilde" => "\u{00C3}",
        "Auml" => "\u{00C4}",
        "Aring" => "\u{00C5}",
        "AElig" => "\u{00C6}",
        "Ccedil" => "\u{00C7}",
        "Egrave" => "\u{00C8}",
        "Eacute" => "\u{00C9}",
        "Ecirc" => "\u{00CA}",
        "Euml" => "\u{00CB}",
        "Igrave" => "\u{00CC}",
        "Iacute" => "\u{00CD}",
        "Icirc" => "\u{00CE}",
        "Iuml" => "\u{00CF}",
        "Ntilde" => "\u{00D1}",
        "Ograve" => "\u{00D2}",
        "Oacute" => "\u{00D3}",
        "Ocirc" => "\u{00D4}",
        "Otilde" => "\u{00D5}",
        "Ouml" => "\u{00D6}",
        "Oslash" => "\u{00D8}",
        "Ugrave" => "\u{00D9}",
        "Uacute" => "\u{00DA}",
        "Ucirc" => "\u{00DB}",
        "Uuml" => "\u{00DC}",
        "szlig" => "\u{00DF}",
        "agrave" => "\u{00E0}",
        "aacute" => "\u{00E1}",
        "acirc" => "\u{00E2}",
        "atilde" => "\u{00E3}",
        "auml" => "\u{00E4}",
        "aring" => "\u{00E5}",
        "aelig" => "\u{00E6}",
        "ccedil" => "\u{00E7}",
        "egrave" => "\u{00E8}",
        "eacute" => "\u{00E9}",
        "ecirc" => "\u{00EA}",
        "euml" => "\u{00EB}",
        "igrave" => "\u{00EC}",
        "iacute" => "\u{00ED}",
        "icirc" => "\u{00EE}",
        "iuml" => "\u{00EF}",
        "ntilde" => "\u{00F1}",
        "ograve" => "\u{00F2}",
        "oacute" => "\u{00F3}",
        "ocirc" => "\u{00F4}",
        "otilde" => "\u{00F5}",
        "ouml" => "\u{00F6}",
        "oslash" => "\u{00F8}",
        "ugrave" => "\u{00F9}",
        "uacute" => "\u{00FA}",
        "ucirc" => "\u{00FB}",
        "uuml" => "\u{00FC}",
        "yuml" => "\u{00FF}",
        _ => return None,
    };
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use quick_xml::events::Event;

    use super::*;

    #[test]
    fn attr_value_local_is_case_insensitive_and_ignores_namespace() {
        let mut reader =
            tolerant_reader(r#"<opf:meta OPF:Scheme="ISBN" ns:CustomAttr="A &amp; &mdash; B"/>"#);
        let Event::Empty(start) = reader.read_event().expect("empty tag") else {
            panic!("expected Empty element");
        };

        assert_eq!(attr_value_local(&start, "scheme").as_deref(), Some("ISBN"));
        assert_eq!(attr_value_local(&start, "SCHEME").as_deref(), Some("ISBN"));
        assert_eq!(
            attr_value_local(&start, "customattr").as_deref(),
            Some("A & \u{2014} B")
        );
        assert_eq!(attr_value_local(&start, "missing"), None);
    }

    #[test]
    fn entity_resolution_handles_xml_html_numeric_and_unknown_refs() {
        assert_eq!(resolve_html_entity("amp"), Some("&"));
        assert_eq!(resolve_html_entity("lt"), Some("<"));
        assert_eq!(resolve_html_entity("nbsp"), Some("\u{00A0}"));
        assert_eq!(resolve_html_entity("mdash"), Some("\u{2014}"));
        assert_eq!(resolve_html_entity("auml"), Some("\u{00E4}"));
        assert_eq!(resolve_html_entity("shy"), Some(""));
        assert_eq!(resolve_html_entity("nonexistent"), None);

        let xml = "<p>&amp;&nbsp;&mdash;&#65;&#x1F600;&unknown;</p>";
        let mut reader = tolerant_reader(xml);
        let mut out = String::new();
        loop {
            match reader.read_event().expect("read event") {
                Event::Text(t) => out.push_str(&t.html_content()),
                Event::GeneralRef(r) => push_entity_ref(&mut out, &r),
                Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(out, "&\u{00A0}\u{2014}A\u{1F600}&unknown;");
    }

    #[test]
    fn tolerant_reader_accepts_dangling_amp_and_mismatched_end_tags() {
        let xml = "<div><span>AT&T & co</div>";
        let mut reader = tolerant_reader(xml);
        let mut texts = Vec::new();
        let mut ends = Vec::new();
        loop {
            match reader.read_event().expect("tolerant read") {
                Event::Text(t) => texts.push(t.html_content().into_owned()),
                Event::End(e) => {
                    ends.push(e.local_name().into_inner().to_owned());
                }
                Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(texts.concat(), "AT&T & co");
        assert_eq!(ends, vec!["div"]);
    }
}
