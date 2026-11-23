use quick_xml::events::Event;

use super::super::decode_xml_bytes;
use super::xml::{push_entity_ref, tolerant_reader};

#[derive(Clone, Copy, PartialEq, Eq)]
enum InlineKind {
    Bold,
    Italic,
    Code,
}

impl InlineKind {
    fn marker(self) -> &'static str {
        match self {
            Self::Bold => "**",
            Self::Italic => "*",
            Self::Code => "`",
        }
    }
}

struct InlineSpan {
    kind: InlineKind,
    emitted: bool,
}

struct MarkdownBuilder {
    out: String,
    pending_space: bool,
    at_line_start: bool,
    just_started_list_item: bool,
    blockquote_depth: usize,
    list_depth: usize,
    pre_depth: usize,
    bold_depth: usize,
    italic_depth: usize,
    code_depth: usize,
    active_inlines: Vec<InlineSpan>,
}

impl MarkdownBuilder {
    fn new() -> Self {
        Self {
            out: String::new(),
            pending_space: false,
            at_line_start: true,
            just_started_list_item: false,
            blockquote_depth: 0,
            list_depth: 0,
            pre_depth: 0,
            bold_depth: 0,
            italic_depth: 0,
            code_depth: 0,
            active_inlines: Vec::new(),
        }
    }

    fn flush_line_prefix_if_needed(&mut self) {
        if self.at_line_start && self.blockquote_depth > 0 && self.pre_depth == 0 {
            for _ in 0..self.blockquote_depth {
                self.out.push_str("> ");
            }
            self.at_line_start = false;
        }
    }

    fn flush_before_content(&mut self) {
        self.flush_line_prefix_if_needed();
        if self.pending_space {
            if !self.at_line_start && !self.out.ends_with([' ', '\n']) {
                self.out.push(' ');
            }
            self.pending_space = false;
        }
        for span in &mut self.active_inlines {
            if !span.emitted {
                self.out.push_str(span.kind.marker());
                span.emitted = true;
                self.at_line_start = false;
            }
        }
        self.just_started_list_item = false;
    }

    fn close_all_inlines(&mut self) {
        while let Some(span) = self.active_inlines.pop() {
            if span.emitted {
                self.out.push_str(span.kind.marker());
            }
        }
        self.bold_depth = 0;
        self.italic_depth = 0;
        self.code_depth = 0;
    }

    fn push_inline_start(&mut self, kind: InlineKind) {
        if self.pre_depth > 0 {
            return;
        }
        let depth = match kind {
            InlineKind::Bold => {
                self.bold_depth += 1;
                self.bold_depth
            }
            InlineKind::Italic => {
                self.italic_depth += 1;
                self.italic_depth
            }
            InlineKind::Code => {
                self.code_depth += 1;
                self.code_depth
            }
        };
        if depth == 1 {
            self.active_inlines.push(InlineSpan {
                kind,
                emitted: false,
            });
        }
    }

    fn push_inline_end(&mut self, kind: InlineKind) {
        if self.pre_depth > 0 {
            return;
        }
        let should_close = match kind {
            InlineKind::Bold => {
                self.bold_depth = self.bold_depth.saturating_sub(1);
                self.bold_depth == 0
            }
            InlineKind::Italic => {
                self.italic_depth = self.italic_depth.saturating_sub(1);
                self.italic_depth == 0
            }
            InlineKind::Code => {
                self.code_depth = self.code_depth.saturating_sub(1);
                self.code_depth == 0
            }
        };
        if !should_close {
            return;
        }
        if let Some(pos) = self.active_inlines.iter().rposition(|s| s.kind == kind) {
            let span = self.active_inlines.remove(pos);
            if span.emitted {
                self.out.push_str(span.kind.marker());
            }
        }
    }

    fn ensure_newlines(&mut self, count: usize) {
        self.pending_space = false;
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        if self.out.is_empty() {
            self.at_line_start = true;
            return;
        }
        let trailing_newlines = self.out.bytes().rev().take_while(|&b| b == b'\n').count();
        for _ in trailing_newlines..count {
            self.out.push('\n');
        }
        self.at_line_start = true;
    }

    fn push_block_break(&mut self) {
        self.close_all_inlines();
        if self.just_started_list_item {
            return;
        }
        self.ensure_newlines(2);
    }

    fn push_text(&mut self, text: &str) {
        if self.pre_depth > 0 {
            let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            if !normalized.is_empty() {
                self.out.push_str(&normalized);
                self.at_line_start = normalized.ends_with('\n');
                self.just_started_list_item = false;
            }
            return;
        }

        for ch in text.chars() {
            if ch.is_whitespace() || ch == '\u{00A0}' {
                if !self.at_line_start && !self.just_started_list_item {
                    self.pending_space = true;
                }
            } else {
                self.flush_before_content();
                self.out.push(ch);
                self.at_line_start = false;
            }
        }
    }

    fn start_heading(&mut self, level: usize) {
        self.close_all_inlines();
        if !self.just_started_list_item {
            self.ensure_newlines(2);
        }
        self.flush_line_prefix_if_needed();
        for _ in 0..level {
            self.out.push('#');
        }
        self.out.push(' ');
        self.at_line_start = false;
        self.pending_space = false;
        self.just_started_list_item = false;
    }

    fn start_list_item(&mut self) {
        self.close_all_inlines();
        self.ensure_newlines(1);
        self.flush_line_prefix_if_needed();
        let indent = self.list_depth.saturating_sub(1);
        for _ in 0..indent {
            self.out.push_str("  ");
        }
        self.out.push_str("- ");
        self.at_line_start = false;
        self.pending_space = false;
        self.just_started_list_item = true;
    }

    fn end_list_item(&mut self) {
        self.close_all_inlines();
        self.just_started_list_item = false;
        self.ensure_newlines(1);
    }

    fn push_br(&mut self) {
        if self.pre_depth > 0 {
            self.out.push('\n');
            self.at_line_start = true;
            return;
        }
        self.pending_space = false;
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        self.out.push('\n');
        self.at_line_start = true;
        self.just_started_list_item = false;
    }

    fn push_hr(&mut self) {
        self.close_all_inlines();
        self.just_started_list_item = false;
        self.ensure_newlines(1);
        self.out.push_str("---\n");
        self.at_line_start = true;
        self.pending_space = false;
    }

    fn start_pre(&mut self) {
        self.close_all_inlines();
        self.just_started_list_item = false;
        if self.pre_depth == 0 {
            self.ensure_newlines(2);
            self.out.push_str("```\n");
            self.at_line_start = true;
            self.pending_space = false;
        }
        self.pre_depth += 1;
    }

    fn end_pre(&mut self) {
        if self.pre_depth == 0 {
            return;
        }
        self.pre_depth -= 1;
        if self.pre_depth == 0 {
            if !self.out.ends_with('\n') {
                self.out.push('\n');
            }
            self.out.push_str("```\n\n");
            self.at_line_start = true;
            self.pending_space = false;
        }
    }

    fn finish(mut self) -> String {
        self.close_all_inlines();
        if self.pre_depth > 0 {
            self.pre_depth = 1;
            self.end_pre();
        }
        normalize_markdown(&self.out)
    }
}

fn normalize_markdown(raw: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut empty_run = 0usize;

    for raw_line in raw.split('\n') {
        let trimmed_end = if in_fence {
            raw_line.trim_end_matches('\r')
        } else {
            raw_line.trim_end()
        };

        if trimmed_end.starts_with("```") {
            in_fence = !in_fence;
            empty_run = 0;
            lines.push(trimmed_end);
            continue;
        }

        if in_fence {
            lines.push(trimmed_end);
        } else if trimmed_end.is_empty() {
            empty_run += 1;
            if empty_run <= 1 && !lines.is_empty() {
                lines.push("");
            }
        } else {
            empty_run = 0;
            lines.push(trimmed_end);
        }
    }

    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[derive(Default)]
struct IgnoredSubtrees {
    head: usize,
    script: usize,
    style: usize,
    title: usize,
    noscript: usize,
}

impl IgnoredSubtrees {
    fn is_ignored(&self) -> bool {
        self.head > 0 || self.script > 0 || self.style > 0 || self.title > 0 || self.noscript > 0
    }
}

/// Converts an EPUB XHTML/HTML chapter byte slice (handling UTF-8/UTF-16 BOMs and HTML entities)
/// into normalized Markdown.
pub(super) fn xhtml_to_markdown(bytes: &[u8]) -> String {
    let xml = decode_xml_bytes(bytes);
    let mut reader = tolerant_reader(&xml);
    let mut md = MarkdownBuilder::new();
    let mut ignored = IgnoredSubtrees::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let tag = e.local_name().into_inner().to_ascii_lowercase();
                match tag.as_str() {
                    "head" => {
                        ignored.head += 1;
                        continue;
                    }
                    "script" => {
                        ignored.script += 1;
                        continue;
                    }
                    "style" => {
                        ignored.style += 1;
                        continue;
                    }
                    "title" => {
                        ignored.title += 1;
                        continue;
                    }
                    "noscript" => {
                        ignored.noscript += 1;
                        continue;
                    }
                    "body" => {
                        // Recover cleanly if malformed HTML left `<head>` unclosed.
                        ignored.head = 0;
                        ignored.title = 0;
                        continue;
                    }
                    _ => {}
                }

                if ignored.is_ignored() {
                    continue;
                }

                match tag.as_str() {
                    "h1" => md.start_heading(1),
                    "h2" => md.start_heading(2),
                    "h3" => md.start_heading(3),
                    "h4" => md.start_heading(4),
                    "h5" => md.start_heading(5),
                    "h6" => md.start_heading(6),
                    "p" | "div" | "section" | "article" | "header" | "footer" | "aside"
                    | "main" | "nav" | "figure" | "figcaption" | "tr" => md.push_block_break(),
                    "br" => md.push_br(),
                    "hr" => md.push_hr(),
                    "ul" | "ol" => {
                        md.push_block_break();
                        md.list_depth += 1;
                    }
                    "li" => md.start_list_item(),
                    "blockquote" => {
                        md.push_block_break();
                        md.blockquote_depth += 1;
                    }
                    "pre" => md.start_pre(),
                    "code" => md.push_inline_start(InlineKind::Code),
                    "b" | "strong" => md.push_inline_start(InlineKind::Bold),
                    "i" | "em" => md.push_inline_start(InlineKind::Italic),
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                if ignored.is_ignored() {
                    continue;
                }
                let tag = e.local_name().into_inner().to_ascii_lowercase();
                match tag.as_str() {
                    "br" => md.push_br(),
                    "hr" => md.push_hr(),
                    "p" | "div" => md.push_block_break(),
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let tag = e.local_name().into_inner().to_ascii_lowercase();
                match tag.as_str() {
                    "head" => {
                        ignored.head = ignored.head.saturating_sub(1);
                        continue;
                    }
                    "script" => {
                        ignored.script = ignored.script.saturating_sub(1);
                        continue;
                    }
                    "style" => {
                        ignored.style = ignored.style.saturating_sub(1);
                        continue;
                    }
                    "title" => {
                        ignored.title = ignored.title.saturating_sub(1);
                        continue;
                    }
                    "noscript" => {
                        ignored.noscript = ignored.noscript.saturating_sub(1);
                        continue;
                    }
                    _ => {}
                }

                if ignored.is_ignored() {
                    continue;
                }

                match tag.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "section"
                    | "article" | "header" | "footer" | "aside" | "main" | "nav" | "figure"
                    | "figcaption" | "tr" => md.push_block_break(),
                    "ul" | "ol" => {
                        md.list_depth = md.list_depth.saturating_sub(1);
                        md.push_block_break();
                    }
                    "li" => md.end_list_item(),
                    "blockquote" => {
                        md.close_all_inlines();
                        md.blockquote_depth = md.blockquote_depth.saturating_sub(1);
                        md.ensure_newlines(2);
                    }
                    "pre" => md.end_pre(),
                    "code" => md.push_inline_end(InlineKind::Code),
                    "b" | "strong" => md.push_inline_end(InlineKind::Bold),
                    "i" | "em" => md.push_inline_end(InlineKind::Italic),
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if !ignored.is_ignored() {
                    md.push_text(&t.html_content());
                }
            }
            Ok(Event::CData(c)) => {
                if !ignored.is_ignored() {
                    md.push_text(&c.html_content());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if !ignored.is_ignored() {
                    let mut decoded = String::new();
                    push_entity_ref(&mut decoded, &r);
                    md.push_text(&decoded);
                }
            }
            Ok(Event::Eof) => break,
            Err(quick_xml::Error::IllFormed(_)) => {}
            Err(_) => break,
            _ => {}
        }
    }

    md.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_headings_paragraphs_nested_inlines_lists_blockquote_br_and_hr() {
        let html = br#"<html><body>
            <h1>Main Title</h1>
            <h2>Sub Title</h2>
            <p>Hello <strong>bold <em>and italic</em> <strong>still</strong></strong> and <code>inline()</code>.</p>
            <ul>
                <li>First item<ul><li>Nested item</li></ul></li>
                <li>Second item</li>
            </ul>
            <ol><li>Ordered</li></ol>
            <blockquote><p>Quoted<br/>line</p></blockquote>
            <hr/>
            <p>After rule</p>
        </body></html>"#;

        let md = xhtml_to_markdown(html);
        assert_eq!(
            md,
            "# Main Title\n\n## Sub Title\n\nHello **bold *and italic* still** and `inline()`.\n\n- First item\n\n  - Nested item\n\n- Second item\n\n- Ordered\n\n> Quoted\n> line\n\n---\n\nAfter rule"
        );
    }

    #[test]
    fn preserves_pre_whitespace_and_ignores_inline_markers_inside_fence() {
        let html = b"<pre>  let x = 1;  \r\n\r\n    <strong>indented</strong>  </pre>";
        let md = xhtml_to_markdown(html);
        assert_eq!(md, "```\n  let x = 1;  \n\n    indented  \n```");
    }

    #[test]
    fn ignored_subtrees_skip_metadata_and_body_recovers_unclosed_head() {
        let html = b"<html><head><title>Hidden</title><style>body{}</style><script>x()</script><noscript>no</noscript><body><p>Visible</p></body></html>";
        let md = xhtml_to_markdown(html);
        assert_eq!(md, "Visible");
    }

    #[test]
    fn normalize_markdown_collapses_blank_lines_only_outside_fences() {
        let raw = "\n\nTitle   \n\n\n\nLine\t \n```\n  keep  \n\n\n  spaces  \n```\n\n\n";
        assert_eq!(
            normalize_markdown(raw),
            "Title\n\nLine\n```\n  keep  \n\n\n  spaces  \n```"
        );
    }

    #[test]
    fn decodes_utf16_be_and_le_boms_and_html_entities() {
        let text = "<html><body><p>Caf&eacute;&nbsp;&mdash;&#x2019;&#65;</p></body></html>";

        let mut be = vec![0xFE, 0xFF];
        for unit in text.encode_utf16() {
            be.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(xhtml_to_markdown(&be), "Caf\u{00E9} \u{2014}\u{2019}A");

        let mut le = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            le.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(xhtml_to_markdown(&le), "Caf\u{00E9} \u{2014}\u{2019}A");
    }
}
