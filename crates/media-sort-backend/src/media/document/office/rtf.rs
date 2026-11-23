use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::media::document::{DocumentError, DocumentPreview, MAX_TEXT_PREVIEW_BYTES};

#[derive(Clone, Copy)]
struct RtfGroupState {
    skip: bool,
    uc: usize,
}

/// Reads up to `MAX_TEXT_PREVIEW_BYTES` from an RTF file, reporting whether it was truncated.
pub(super) fn read_bounded_rtf_bytes(path: &Path) -> Result<(Vec<u8>, bool), DocumentError> {
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

/// Loads a bounded plain-text preview from an `.rtf` file without invoking LibreOffice.
pub(super) fn load_rtf_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let (raw, truncated) = read_bounded_rtf_bytes(path)?;
    let text = parse_rtf_bytes(&raw);
    Ok(DocumentPreview::PlainText { text, truncated })
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

/// Extracts readable plain text from raw RTF bytes, handling destination skipping, `\binN`,
/// `\uc`/`\uN` Unicode surrogates, and CP-1252 hex escapes.
pub(super) fn parse_rtf_bytes(raw: &[u8]) -> String {
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
                if skip_fallback_chars == 0 {
                    pending_surrogate = None;
                }
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

/// Extracts `Title`, `Author`, and `Subject` from an RTF `{\info ...}` block using raw byte
/// offsets so non-UTF-8 bytes never shift slice indices.
pub(super) fn parse_rtf_info_metadata(raw: &[u8], fields: &mut BTreeMap<String, String>) {
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
    fn parses_rtf_stripping_headers_and_decoding_escapes() {
        let rtf = br#"{\rtf1\ansi{\fonttbl{\f0 Arial;}}{\*\generator Foo}Hello\par Caf\'e9 \u8212\'97 world}"#;
        let text = parse_rtf_bytes(rtf);
        assert_eq!(text, "Hello\nCaf\u{00E9} \u{2014} world");
    }

    #[test]
    fn parses_rtf_bin_surrogates_uc_and_depth_cap() {
        // `\bin4` skips 4 raw bytes (`{}\A`) including braces that would otherwise alter stack.
        let rtf_bin = br#"{\rtf1 Start \bin4 {}\AEnd}"#;
        assert_eq!(parse_rtf_bytes(rtf_bin), "Start End");

        // UTF-16 surrogate pair for U+1F600 (0xD83D = -10179, 0xDE00 = -8704) with `\uc0` / `\uc2`.
        let rtf_utf16 = br#"{\rtf1\uc2 \u-10179??\u-8704?? \uc0 \u233 ok}"#;
        assert_eq!(parse_rtf_bytes(rtf_utf16), "\u{1F600} \u{00E9}ok");

        // Deep nesting past MAX_STACK_DEPTH (256) does not overflow or panic.
        let mut deep = Vec::new();
        deep.extend(std::iter::repeat_n(b'{', 300));
        deep.extend_from_slice(br"\rtf1 Deep");
        deep.extend(std::iter::repeat_n(b'}', 300));
        assert_eq!(parse_rtf_bytes(&deep), "Deep");
    }

    #[test]
    fn parses_rtf_info_metadata_and_normalizes_plain_text() {
        let rtf =
            br#"{\rtf1{\info{\title  My \'93Report\'94 }{\author   Alice }{\subject   }}Body}"#;
        let mut fields = BTreeMap::new();
        parse_rtf_info_metadata(rtf, &mut fields);
        assert_eq!(
            fields.get("Title").map(String::as_str),
            Some("My \u{201C}Report\u{201D}")
        );
        assert_eq!(fields.get("Author").map(String::as_str), Some("Alice"));
        assert!(!fields.contains_key("Subject"));

        assert_eq!(
            normalize_plain_text("  \nLine 1   \n\n\n\nLine 2  \n\n".to_string()),
            "Line 1\n\nLine 2"
        );
    }
}
