use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use media_sort_backend::media::DecodedImage;
use media_sort_backend::media::document::{
    DocumentPreview, DocumentUnavailableReason, generate_document_thumbnail, load_document_preview,
};
use media_sort_backend::metadata::document_meta::extract_document_metadata;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let pid = std::process::id();
        let count = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("mediasort_doc_{prefix}_{pid}_{count}"));
        fs::create_dir_all(&dir).expect("create temp test dir");
        Self { path: dir }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

fn assert_solid_color_close(img: &DecodedImage, expected: [u8; 3]) {
    assert!(img.width > 0 && img.height > 0, "image must be non-empty");
    assert_eq!(
        img.rgba.len(),
        (img.width as usize) * (img.height as usize) * 4,
        "rgba buffer length mismatch"
    );
    let cx = (img.width / 2) as usize;
    let cy = (img.height / 2) as usize;
    let idx = (cy * img.width as usize + cx) * 4;
    let actual = &img.rgba[idx..idx + 4];
    for (c, (&act, &exp)) in actual[..3].iter().zip(expected.iter()).enumerate() {
        let diff = act.abs_diff(exp);
        assert!(
            diff <= 3,
            "center pixel channel {c} mismatch: expected {exp}, got {act} (full={actual:?})"
        );
    }
    assert_eq!(actual[3], 255, "center pixel alpha must be opaque");
}

fn escape_pdf_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '(' => out.push_str(r"\("),
            ')' => out.push_str(r"\)"),
            _ => out.push(ch),
        }
    }
    out
}

fn build_two_page_pdf(title: &str, author: &str) -> Vec<u8> {
    let stream0 = b"1 0 0 rg 10 10 180 180 re f\n";
    let stream1 = b"0 0 1 rg 10 10 180 180 re f\n";

    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");

    let mut offsets = Vec::with_capacity(7);

    offsets.push(out.len());
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 5 0 R >>\nendobj\n",
    );

    offsets.push(out.len());
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 6 0 R >>\nendobj\n",
    );

    offsets.push(out.len());
    out.extend_from_slice(format!("5 0 obj\n<< /Length {} >>\nstream\n", stream0.len()).as_bytes());
    out.extend_from_slice(stream0);
    out.extend_from_slice(b"endstream\nendobj\n");

    offsets.push(out.len());
    out.extend_from_slice(format!("6 0 obj\n<< /Length {} >>\nstream\n", stream1.len()).as_bytes());
    out.extend_from_slice(stream1);
    out.extend_from_slice(b"endstream\nendobj\n");

    offsets.push(out.len());
    let info = format!(
        "7 0 obj\n<< /Title ({}) /Author ({}) >>\nendobj\n",
        escape_pdf_literal(title),
        escape_pdf_literal(author),
    );
    out.extend_from_slice(info.as_bytes());

    let xref_pos = out.len();
    out.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 8 /Root 1 0 R /Info 7 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

#[test]
fn test_pdf_thumbnail_preview_metadata_and_corrupt() {
    let dir = TempDir::new("pdf");
    let pdf_path = dir.path().join("two_page.pdf");
    let pdf_bytes = build_two_page_pdf("Test PDF Title", "Ada Lovelace");
    fs::write(&pdf_path, pdf_bytes).expect("write valid pdf");

    let thumb = generate_document_thumbnail(&pdf_path, 128, 128)
        .expect("pdf thumbnail result")
        .expect("pdf thumbnail image");
    assert!(thumb.width <= 128 && thumb.height <= 128);
    assert_solid_color_close(&thumb, [255, 0, 0]);

    let page0 = load_document_preview(&pdf_path, 0, false).expect("pdf page 0 preview");
    match page0 {
        DocumentPreview::PagedImage {
            image,
            page_index,
            total_pages,
        } => {
            assert_eq!(page_index, 0);
            assert_eq!(total_pages, 2);
            assert_solid_color_close(&image, [255, 0, 0]);
        }
        other => panic!("expected PagedImage for PDF page 0, got {other:?}"),
    }

    let page1 = load_document_preview(&pdf_path, 1, false).expect("pdf page 1 preview");
    match page1 {
        DocumentPreview::PagedImage {
            image,
            page_index,
            total_pages,
        } => {
            assert_eq!(page_index, 1);
            assert_eq!(total_pages, 2);
            assert_solid_color_close(&image, [0, 0, 255]);
        }
        other => panic!("expected PagedImage for PDF page 1, got {other:?}"),
    }

    let meta = extract_document_metadata(&pdf_path).expect("extract pdf metadata");
    let doc = &meta["Document"];
    assert_eq!(doc.get("Kind").map(String::as_str), Some("PDF"));
    assert_eq!(doc.get("Pages").map(String::as_str), Some("2"));
    assert_eq!(doc.get("Title").map(String::as_str), Some("Test PDF Title"));
    assert_eq!(doc.get("Author").map(String::as_str), Some("Ada Lovelace"));

    let corrupt_path = dir.path().join("corrupt.pdf");
    fs::write(&corrupt_path, b"not a valid pdf document").expect("write corrupt pdf");
    assert!(
        generate_document_thumbnail(&corrupt_path, 128, 128).is_err(),
        "corrupt PDF thumbnail must error cleanly"
    );
    assert!(
        load_document_preview(&corrupt_path, 0, false).is_err(),
        "corrupt PDF preview must error cleanly"
    );
    let corrupt_meta = extract_document_metadata(&corrupt_path).expect("corrupt pdf metadata");
    assert_eq!(
        corrupt_meta["Document"].get("Kind").map(String::as_str),
        Some("PDF")
    );
    assert!(!corrupt_meta["Document"].contains_key("Pages"));
}

#[test]
fn test_repeated_multipage_pdf_page_turns() {
    let dir = TempDir::new("repeat_turns");

    // PDF repeated turns across cached pages 0 <-> 1
    let pdf_path = dir.path().join("turns.pdf");
    fs::write(&pdf_path, build_two_page_pdf("Repeat PDF", "Grace Hopper")).expect("write pdf");
    for (req, expected_rgb) in [
        (0usize, [255u8, 0, 0]),
        (1, [0, 0, 255]),
        (0, [255, 0, 0]),
        (1, [0, 0, 255]),
    ] {
        match load_document_preview(&pdf_path, req, false).expect("pdf repeat preview") {
            DocumentPreview::PagedImage {
                image,
                page_index,
                total_pages,
            } => {
                assert_eq!((page_index, total_pages), (req, 2));
                assert_solid_color_close(&image, expected_rgb);
            }
            other => panic!("expected PagedImage for PDF page {req}, got {other:?}"),
        }
    }
}

#[test]
fn test_csv_and_tsv_spreadsheet_previews() {
    let dir = TempDir::new("csv_tsv");
    let csv_path = dir.path().join("people.csv");
    fs::write(
        &csv_path,
        "name,age,role\nAlice,30,Engineer\nBob,25,Designer\n",
    )
    .expect("write csv");

    let csv_preview = load_document_preview(&csv_path, 0, false).expect("csv preview");
    match csv_preview {
        DocumentPreview::Spreadsheet { sheets } => {
            assert_eq!(sheets.len(), 1);
            let s = &sheets[0];
            assert_eq!(s.name, "people.csv");
            assert_eq!(s.headers, vec!["name", "age", "role"]);
            assert_eq!(
                s.rows,
                vec![
                    vec!["Alice", "30", "Engineer"],
                    vec!["Bob", "25", "Designer"],
                ]
            );
            assert_eq!(s.total_rows, 2);
            assert_eq!(s.total_cols, 3);
            assert!(!s.truncated);
        }
        other => panic!("expected Spreadsheet for CSV, got {other:?}"),
    }

    let tsv_path = dir.path().join("items.tsv");
    fs::write(&tsv_path, "id\titem\tqty\n1\tWidget\t42\n").expect("write tsv");

    let tsv_preview = load_document_preview(&tsv_path, 0, false).expect("tsv preview");
    match tsv_preview {
        DocumentPreview::Spreadsheet { sheets } => {
            assert_eq!(sheets.len(), 1);
            let s = &sheets[0];
            assert_eq!(s.name, "items.tsv");
            assert_eq!(s.headers, vec!["id", "item", "qty"]);
            assert_eq!(s.rows, vec![vec!["1", "Widget", "42"]]);
            assert_eq!(s.total_rows, 1);
            assert_eq!(s.total_cols, 3);
            assert!(!s.truncated);
        }
        other => panic!("expected Spreadsheet for TSV, got {other:?}"),
    }
}

#[test]
fn test_markdown_plain_text_nul_binary_and_metadata() {
    let dir = TempDir::new("text_md");

    let md_path = dir.path().join("readme.md");
    fs::write(&md_path, "# Title\n\nSome markdown content.\n").expect("write md");
    assert!(
        generate_document_thumbnail(&md_path, 128, 128)
            .expect("md thumbnail result")
            .is_none()
    );
    let md_preview = load_document_preview(&md_path, 0, false).expect("md preview");
    match md_preview {
        DocumentPreview::Markdown {
            markdown,
            chapter_index,
            total_chapters,
            cover,
        } => {
            assert_eq!(markdown, "# Title\n\nSome markdown content.");
            assert!(chapter_index.is_none());
            assert!(total_chapters.is_none());
            assert!(cover.is_none());
        }
        other => panic!("expected Markdown preview, got {other:?}"),
    }
    let md_meta = extract_document_metadata(&md_path).expect("md metadata");
    assert_eq!(
        md_meta["Document"].get("Kind").map(String::as_str),
        Some("Markdown")
    );
    assert_eq!(
        md_meta["Document"].get("Lines").map(String::as_str),
        Some("3")
    );
    assert_eq!(
        md_meta["Document"].get("Words").map(String::as_str),
        Some("5")
    );
    assert_eq!(
        md_meta["Document"].get("Encoding").map(String::as_str),
        Some("UTF-8")
    );

    let txt_path = dir.path().join("notes.txt");
    fs::write(&txt_path, "Hello world\nSecond line here\n").expect("write txt");
    assert!(
        generate_document_thumbnail(&txt_path, 128, 128)
            .expect("txt thumbnail result")
            .is_none()
    );
    let txt_preview = load_document_preview(&txt_path, 0, false).expect("txt preview");
    match txt_preview {
        DocumentPreview::PlainText { text, truncated } => {
            assert!(!truncated);
            assert_eq!(text, "Hello world\nSecond line here");
        }
        other => panic!("expected PlainText preview, got {other:?}"),
    }
    let txt_meta = extract_document_metadata(&txt_path).expect("txt metadata");
    assert_eq!(
        txt_meta["Document"].get("Kind").map(String::as_str),
        Some("Text")
    );
    assert_eq!(
        txt_meta["Document"].get("Lines").map(String::as_str),
        Some("2")
    );
    assert_eq!(
        txt_meta["Document"].get("Words").map(String::as_str),
        Some("5")
    );
    assert_eq!(
        txt_meta["Document"].get("Encoding").map(String::as_str),
        Some("UTF-8")
    );

    let nul_path = dir.path().join("fake.txt");
    fs::write(&nul_path, b"ELF\x00\x01\x02binary payload").expect("write binary fake.txt");
    let nul_preview = load_document_preview(&nul_path, 0, false).expect("nul txt preview");
    assert!(
        matches!(
            nul_preview,
            DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::BinaryOrUnreadable
            }
        ),
        "expected BinaryOrUnreadable for NUL-byte txt, got {nul_preview:?}"
    );
    let nul_meta = extract_document_metadata(&nul_path).expect("nul txt metadata");
    assert_eq!(
        nul_meta["Document"].get("Kind").map(String::as_str),
        Some("Text")
    );
    assert_eq!(
        nul_meta["Document"].get("Encoding").map(String::as_str),
        Some("Binary")
    );
}
