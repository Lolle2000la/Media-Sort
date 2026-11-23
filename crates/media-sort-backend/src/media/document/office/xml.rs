use quick_xml::XmlVersion;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;

/// Returns the local XML element name (without namespace prefix) from a start/empty tag.
pub(super) fn local_name_start<'a>(e: &'a BytesStart<'a>) -> &'a str {
    e.local_name().into_inner()
}

/// Finds the first attribute on `e` whose local name matches `target_local` and returns its
/// normalized value.
pub(super) fn get_attr_local(e: &BytesStart<'_>, target_local: &str) -> Option<String> {
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

/// Resolves numeric character references and predefined XML entities (`&amp;`, `&lt;`, etc.) into
/// `buf`.
pub(super) fn append_general_ref(buf: &mut String, r: &BytesRef<'_>) {
    if let Ok(Some(ch)) = r.resolve_char_ref() {
        buf.push(ch);
    } else if let Some(resolved) = quick_xml::escape::resolve_predefined_entity(r.as_ref()) {
        buf.push_str(resolved);
    }
}

/// Tracks outer-table state while converting DOCX/ODT tables into GitHub-flavored Markdown tables.
#[derive(Default)]
pub(super) struct MarkdownTableState {
    depth: usize,
    rows_emitted: usize,
    row_cells: Vec<String>,
    cell_buf: String,
}

impl MarkdownTableState {
    pub(super) fn enter_table(&mut self) {
        self.depth += 1;
        if self.depth == 1 {
            self.rows_emitted = 0;
            self.row_cells.clear();
            self.cell_buf.clear();
        }
    }

    pub(super) fn leave_table(&mut self, out: &mut String) {
        if self.depth == 1 && self.rows_emitted > 0 {
            out.push('\n');
        }
        self.depth = self.depth.saturating_sub(1);
    }

    pub(super) fn enter_row(&mut self) {
        if self.depth == 1 {
            self.row_cells.clear();
        }
    }

    pub(super) fn leave_row(&mut self, out: &mut String) {
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

    pub(super) fn enter_cell(&mut self) {
        if self.depth == 1 {
            self.cell_buf.clear();
        }
    }

    pub(super) fn leave_cell(&mut self) {
        if self.depth == 1 {
            let escaped = self.cell_buf.trim().replace('|', r"\|");
            self.row_cells.push(escaped);
            self.cell_buf.clear();
        }
    }

    pub(super) fn push_paragraph_to_cell(&mut self, text: &str) {
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

/// Strips trailing newlines and appends a truncation notice when `truncated` is `true`.
pub(super) fn finish_markdown_output(mut out: String, truncated: bool) -> String {
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

/// Emits a heading, list item, plain paragraph, or table-cell paragraph into `out`.
pub(super) fn emit_markdown_paragraph(
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

/// Returns `true` for ODF XML subtrees whose text should not appear in body previews.
pub(super) fn is_odf_skip_element(local: &str) -> bool {
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

/// Appends inline ODF whitespace/line-break elements (`line-break`, `tab`, `s`) into `block_buf`.
pub(super) fn handle_odf_inline(e: &BytesStart<'_>, block_buf: &mut String) {
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

/// Parses leaf XML elements from `xml` and invokes `on_tag(local_name, trimmed_text)` for each
/// non-empty element.
pub(super) fn parse_simple_xml_tags(xml: &[u8], mut on_tag: impl FnMut(&str, &str)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_table_state_handles_nested_empty_escaping_and_header_separator() {
        let mut out = String::new();
        let mut table = MarkdownTableState::default();

        table.enter_table();

        // Empty row is ignored and does not count as the header row.
        table.enter_row();
        table.enter_cell();
        table.leave_cell();
        table.enter_cell();
        table.push_paragraph_to_cell("   \n ");
        table.leave_cell();
        table.leave_row(&mut out);
        assert!(out.is_empty());

        // First non-empty row emits header + separator; pipes are escaped and nested table
        // content merges into the outer cell without triggering nested row emissions.
        table.enter_row();
        table.enter_cell();
        table.push_paragraph_to_cell("A|B");
        table.leave_cell();
        table.enter_cell();
        table.enter_table();
        table.enter_row();
        table.enter_cell();
        table.push_paragraph_to_cell("Inner 1");
        table.leave_cell();
        table.leave_row(&mut out);
        table.leave_table(&mut out);
        table.push_paragraph_to_cell("Outer 2");
        table.leave_cell();
        table.leave_row(&mut out);

        // Second non-empty row does not repeat the separator.
        table.enter_row();
        table.enter_cell();
        table.push_paragraph_to_cell("X");
        table.leave_cell();
        table.enter_cell();
        table.push_paragraph_to_cell("Y");
        table.leave_cell();
        table.leave_row(&mut out);

        table.leave_table(&mut out);

        assert_eq!(
            out,
            "| A\\|B | Inner 1 Outer 2 |\n| --- | --- |\n| X | Y |\n\n"
        );
    }

    #[test]
    fn emit_markdown_paragraph_formats_headings_lists_and_tables() {
        let mut out = String::new();
        let mut table = MarkdownTableState::default();
        let mut prev_was_list = false;

        emit_markdown_paragraph(
            &mut out,
            &mut table,
            "My\nTitle",
            Some(2),
            false,
            0,
            &mut prev_was_list,
        );
        emit_markdown_paragraph(
            &mut out,
            &mut table,
            "Item A",
            None,
            true,
            0,
            &mut prev_was_list,
        );
        emit_markdown_paragraph(
            &mut out,
            &mut table,
            "Deep item",
            None,
            true,
            10,
            &mut prev_was_list,
        );
        emit_markdown_paragraph(
            &mut out,
            &mut table,
            "After list",
            None,
            false,
            0,
            &mut prev_was_list,
        );

        assert_eq!(
            out,
            "## My Title\n\n- Item A\n        - Deep item\n\nAfter list\n\n"
        );
    }

    #[test]
    fn finish_markdown_output_trims_and_marks_truncation() {
        assert_eq!(
            finish_markdown_output("hello\n\n\n".to_string(), false),
            "hello"
        );
        assert_eq!(
            finish_markdown_output("hello\n".to_string(), true),
            "hello\n\n---\n*(Preview truncated)*"
        );
        assert_eq!(
            finish_markdown_output(String::new(), true),
            "---\n*(Preview truncated)*"
        );
    }

    #[test]
    fn handle_odf_inline_clamps_spaces_and_handles_tabs_and_breaks() {
        let xml = br#"<root><text:s/><text:s text:c="3"/><text:s text:c="250"/><text:tab/><text:line-break/></root>"#;
        let mut reader = Reader::from_reader(xml.as_slice());
        let mut buf = String::new();
        loop {
            match reader.read_event() {
                Ok(Event::Eof) | Err(_) => break,
                Ok(Event::Empty(e)) => handle_odf_inline(&e, &mut buf),
                _ => {}
            }
        }
        assert_eq!(buf, format!("{}\t\n", " ".repeat(1 + 3 + 64)));
    }

    #[test]
    fn parse_simple_xml_tags_extracts_trimmed_values_and_entities() {
        let xml = br#"<root><dc:title>  Hello &amp; World  </dc:title><empty>   </empty><n>42</n></root>"#;
        let mut collected = Vec::new();
        parse_simple_xml_tags(xml, |tag, val| {
            collected.push((tag.to_string(), val.to_string()));
        });
        assert_eq!(
            collected,
            vec![
                ("title".to_string(), "Hello & World".to_string()),
                ("n".to_string(), "42".to_string()),
            ]
        );
    }
}
