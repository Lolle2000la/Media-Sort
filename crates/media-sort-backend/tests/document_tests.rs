use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
