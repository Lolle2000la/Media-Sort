use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::{
    DocumentError, DocumentPreview, DocumentUnavailableReason, MAX_SHEET_COLS, MAX_SHEET_ROWS,
    MAX_TEXT_PREVIEW_BYTES, MAX_TEXT_PREVIEW_LINES, SheetData,
};

enum TextReadResult {
    Text { text: String, truncated: bool },
    Binary,
}

fn read_bounded_text(path: &Path) -> Result<TextReadResult, DocumentError> {
    let file = File::open(path)?;
    let total_len = file.metadata()?.len();
    let mut raw = Vec::new();
    file.take(MAX_TEXT_PREVIEW_BYTES + 1)
        .read_to_end(&mut raw)?;
    let truncated_bytes =
        raw.len() as u64 > MAX_TEXT_PREVIEW_BYTES || total_len > MAX_TEXT_PREVIEW_BYTES;
    if raw.len() as u64 > MAX_TEXT_PREVIEW_BYTES {
        raw.truncate(MAX_TEXT_PREVIEW_BYTES as usize);
    }

    if raw.contains(&0) {
        return Ok(TextReadResult::Binary);
    }

    let raw_slice = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&raw);
    let decoded = match std::str::from_utf8(raw_slice) {
        Ok(s) => s.to_string(),
        Err(err) => {
            // If truncated right in the middle of a multi-byte UTF-8 sequence at buffer end, trim the incomplete tail.
            let valid_up_to = err.valid_up_to();
            if truncated_bytes && raw_slice.len().saturating_sub(valid_up_to) <= 4 {
                std::str::from_utf8(&raw_slice[..valid_up_to])
                    .unwrap_or("")
                    .to_string()
            } else {
                let lossy = String::from_utf8_lossy(raw_slice);
                let replacements = lossy.chars().filter(|&c| c == '\u{FFFD}').count();
                let chars = lossy.chars().count().max(1);
                if replacements * 10 > chars {
                    return Ok(TextReadResult::Binary);
                }
                lossy.into_owned()
            }
        }
    };

    let mut out = String::new();
    let mut lines = 0usize;
    let mut truncated_lines = false;
    for (idx, line) in decoded.lines().enumerate() {
        if idx >= MAX_TEXT_PREVIEW_LINES {
            truncated_lines = true;
            break;
        }
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(line);
        lines = idx + 1;
    }
    let _ = lines;
    Ok(TextReadResult::Text {
        text: out,
        truncated: truncated_bytes || truncated_lines,
    })
}

/// Loads Markdown, CSV/TSV spreadsheet view, or plain-text preview for `path`.
pub fn load_text_preview(path: &Path) -> Result<DocumentPreview, DocumentError> {
    let text = match read_bounded_text(path)? {
        TextReadResult::Binary => {
            return Ok(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::BinaryOrUnreadable,
            });
        }
        TextReadResult::Text { text, truncated } => (text, truncated),
    };

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "md" | "markdown" => {
            let mut markdown = text.0;
            if text.1 {
                markdown.push_str("\n\n---\n*(Preview truncated)*");
            }
            Ok(DocumentPreview::Markdown {
                markdown,
                chapter_index: None,
                total_chapters: None,
                cover: None,
            })
        }
        "csv" | "tsv" => parse_delimited_preview(
            &text.0,
            if ext == "tsv" { b'\t' } else { b',' },
            text.1,
            path,
        ),
        _ => Ok(DocumentPreview::PlainText {
            text: text.0,
            truncated: text.1,
        }),
    }
}

fn parse_delimited_preview(
    content: &str,
    delimiter: u8,
    bytes_truncated: bool,
    path: &Path,
) -> Result<DocumentPreview, DocumentError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(content.as_bytes());

    let mut all_rows: Vec<Vec<String>> = Vec::new();
    let mut max_cols = 0usize;
    let mut total_rows = 0usize;
    let mut total_cols = 0usize;
    let mut truncated = bytes_truncated;

    for record_res in reader.records() {
        let Ok(record) = record_res else {
            continue;
        };
        total_rows += 1;
        total_cols = total_cols.max(record.len());
        if all_rows.len() < MAX_SHEET_ROWS + 1 {
            let row: Vec<String> = record
                .iter()
                .take(MAX_SHEET_COLS)
                .map(|c| c.to_string())
                .collect();
            if record.len() > MAX_SHEET_COLS {
                truncated = true;
            }
            max_cols = max_cols.max(row.len());
            all_rows.push(row);
        } else {
            truncated = true;
        }
    }

    let (headers, rows) = if all_rows.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let mut iter = all_rows.into_iter();
        let mut hdrs = iter.next().unwrap_or_default();
        hdrs.resize(max_cols, String::new());
        let mut body: Vec<Vec<String>> = iter.collect();
        for row in &mut body {
            row.resize(max_cols, String::new());
        }
        (hdrs, body)
    };

    let sheet_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Table".to_string());

    Ok(DocumentPreview::Spreadsheet {
        sheets: vec![SheetData {
            name: sheet_name,
            headers,
            rows,
            total_rows: total_rows.saturating_sub(1),
            total_cols,
            truncated,
        }],
    })
}

/// Extracts line/word/encoding summary for TXT/Markdown/CSV files.
pub fn extract_text_metadata(path: &Path) -> Result<BTreeMap<String, String>, DocumentError> {
    let mut fields = BTreeMap::new();
    match read_bounded_text(path)? {
        TextReadResult::Binary => {
            fields.insert("Encoding".into(), "Binary".into());
        }
        TextReadResult::Text { text, truncated } => {
            let lines = text.lines().count();
            let words = text.split_whitespace().count();
            let suffix = if truncated { "+" } else { "" };
            fields.insert("Lines".into(), format!("{lines}{suffix}"));
            fields.insert("Words".into(), format!("{words}{suffix}"));
            fields.insert("Encoding".into(), "UTF-8".into());
        }
    }
    Ok(fields)
}
