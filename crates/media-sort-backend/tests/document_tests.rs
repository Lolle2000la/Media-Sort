use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Write};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use media_sort_backend::media::DecodedImage;
use media_sort_backend::media::document::{
    DocumentPreview, DocumentUnavailableReason, generate_document_thumbnail, load_document_preview,
};
use media_sort_backend::metadata::document_meta::extract_document_metadata;
use media_sort_backend::process::{
    find_executable_on_path, is_sandboxed, new_hidden_command, path_starts_with_dash,
    run_command_capped,
};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

const EXTERNAL_TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CLI_STDIO_BYTES: u64 = 64 * 1024;
const OVERSIZED_ZIP_ENTRY_BYTES: usize = 33 * 1024 * 1024;

#[cfg(target_os = "windows")]
const SEVENZ_BIN: &str = "7z.exe";
#[cfg(not(target_os = "windows"))]
const SEVENZ_BIN: &str = "7z";
#[cfg(target_os = "windows")]
const RAR_BIN: &str = "rar.exe";
#[cfg(not(target_os = "windows"))]
const RAR_BIN: &str = "rar";

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

fn solid_png_bytes(r: u8, g: u8, b: u8) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([r, g, b, 255]));
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode solid PNG");
    cursor.into_inner()
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

fn write_zip_entries(path: &Path, entries: &[(&str, &[u8], CompressionMethod)]) {
    let file = File::create(path).expect("create zip file");
    let mut zip = ZipWriter::new(BufWriter::new(file));
    for &(name, data, method) in entries {
        let options = SimpleFileOptions::default().compression_method(method);
        zip.start_file(name, options).expect("start zip entry");
        zip.write_all(data).expect("write zip entry");
    }
    let writer = zip.finish().expect("finish zip archive");
    let _file = writer.into_inner().expect("flush zip file");
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

fn run_external_tool(bin: &Path, args: &[&OsStr], work_dir: &Path, timeout: Duration) {
    assert!(!is_sandboxed(), "must not run host CLI inside sandbox");
    assert!(
        !path_starts_with_dash(bin) && !path_starts_with_dash(work_dir),
        "refusing '-' prefixed binary or working directory path"
    );

    let mut cmd = new_hidden_command(bin);
    cmd.args(args).current_dir(work_dir);

    let label = bin.display().to_string();
    let out = run_command_capped(
        &mut cmd,
        timeout,
        MAX_CLI_STDIO_BYTES,
        MAX_CLI_STDIO_BYTES,
        &label,
    )
    .expect("run external test helper tool");

    assert!(
        out.status.success(),
        "external tool {label} failed ({}): stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
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
fn test_cbz_natural_sort_and_comic_info_metadata() {
    let dir = TempDir::new("cbz");
    let cbz_path = dir.path().join("issue.cbz");

    let red = solid_png_bytes(255, 0, 0);
    let green = solid_png_bytes(0, 255, 0);
    let blue = solid_png_bytes(0, 0, 255);
    let comic_info = br#"<?xml version="1.0" encoding="UTF-8"?>
<ComicInfo>
  <Title>Natural Order Issue</Title>
  <Series>Test Series</Series>
  <Number>7</Number>
  <Writer>Jane Doe</Writer>
</ComicInfo>"#;

    // Insert page10, page2, page1 deliberately out of natural order.
    write_zip_entries(
        &cbz_path,
        &[
            ("page10.png", &blue, CompressionMethod::Deflated),
            ("page2.png", &green, CompressionMethod::Deflated),
            ("page1.png", &red, CompressionMethod::Deflated),
            ("ComicInfo.xml", comic_info, CompressionMethod::Deflated),
        ],
    );

    let thumb = generate_document_thumbnail(&cbz_path, 128, 128)
        .expect("cbz thumbnail result")
        .expect("cbz thumbnail image");
    assert_solid_color_close(&thumb, [255, 0, 0]);

    let expected_pages = [(0usize, [255, 0, 0]), (1, [0, 255, 0]), (2, [0, 0, 255])];
    for (idx, color) in expected_pages {
        let preview = load_document_preview(&cbz_path, idx, false).expect("cbz preview");
        match preview {
            DocumentPreview::PagedImage {
                image,
                page_index,
                total_pages,
            } => {
                assert_eq!(page_index, idx);
                assert_eq!(total_pages, 3);
                assert_solid_color_close(&image, color);
            }
            other => panic!("expected PagedImage at index {idx}, got {other:?}"),
        }
    }

    let meta = extract_document_metadata(&cbz_path).expect("cbz metadata");
    let doc = &meta["Document"];
    assert_eq!(doc.get("Kind").map(String::as_str), Some("Comic Archive"));
    assert_eq!(doc.get("Pages").map(String::as_str), Some("3"));
    assert_eq!(doc.get("Series").map(String::as_str), Some("Test Series"));
    assert_eq!(
        doc.get("Title").map(String::as_str),
        Some("Natural Order Issue")
    );
    assert_eq!(doc.get("Number").map(String::as_str), Some("7"));
    assert_eq!(doc.get("Writer").map(String::as_str), Some("Jane Doe"));
}

#[test]
fn test_cbt_thumbnail_and_multipage_preview() {
    let dir = TempDir::new("cbt");
    let cbt_path = dir.path().join("pages.cbt");

    let red = solid_png_bytes(240, 20, 20);
    let green = solid_png_bytes(20, 220, 20);

    {
        let file = File::create(&cbt_path).expect("create cbt file");
        let mut builder = tar::Builder::new(file);
        for (name, bytes) in [("page10.png", &green), ("page2.png", &red)] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, name, Cursor::new(bytes.as_slice()))
                .expect("append tar entry");
        }
        builder.finish().expect("finish tar archive");
    }

    let thumb = generate_document_thumbnail(&cbt_path, 128, 128)
        .expect("cbt thumbnail result")
        .expect("cbt thumbnail image");
    assert_solid_color_close(&thumb, [240, 20, 20]);

    for (idx, color) in [(0usize, [240, 20, 20]), (1, [20, 220, 20])] {
        let preview = load_document_preview(&cbt_path, idx, false).expect("cbt preview");
        match preview {
            DocumentPreview::PagedImage {
                image,
                page_index,
                total_pages,
            } => {
                assert_eq!(page_index, idx);
                assert_eq!(total_pages, 2);
                assert_solid_color_close(&image, color);
            }
            other => panic!("expected PagedImage for CBT page {idx}, got {other:?}"),
        }
    }

    let meta = extract_document_metadata(&cbt_path).expect("cbt metadata");
    assert_eq!(meta["Document"].get("Pages").map(String::as_str), Some("2"));
}

#[test]
fn test_cb7_invalid_and_real_when_7z_available() {
    let dir = TempDir::new("cb7");
    let bad_cb7 = dir.path().join("invalid.cb7");
    fs::write(&bad_cb7, b"not a valid 7z comic archive").expect("write invalid cb7");

    assert!(
        generate_document_thumbnail(&bad_cb7, 128, 128).is_err(),
        "invalid cb7 thumbnail must return error"
    );
    assert!(
        !matches!(
            load_document_preview(&bad_cb7, 0, false),
            Ok(DocumentPreview::PagedImage { .. })
        ),
        "invalid cb7 must not produce PagedImage"
    );
    let bad_meta = extract_document_metadata(&bad_cb7).expect("invalid cb7 metadata");
    assert_eq!(
        bad_meta["Document"].get("Kind").map(String::as_str),
        Some("Comic Archive")
    );

    if is_sandboxed() {
        return;
    }
    let Some(sevenz) = find_executable_on_path(&[SEVENZ_BIN], false) else {
        return;
    };

    let stage = dir.path().join("stage");
    fs::create_dir(&stage).expect("create cb7 staging dir");
    fs::write(stage.join("page1.png"), solid_png_bytes(230, 30, 30)).expect("write page1");
    fs::write(stage.join("page2.png"), solid_png_bytes(30, 30, 230)).expect("write page2");
    fs::write(
        stage.join("ComicInfo.xml"),
        br#"<ComicInfo><Title>SevenZ Issue</Title><Series>CB7 Suite</Series></ComicInfo>"#,
    )
    .expect("write ComicInfo.xml");

    let args: [&OsStr; 8] = [
        OsStr::new("a"),
        OsStr::new("-t7z"),
        OsStr::new("-y"),
        OsStr::new("--"),
        OsStr::new("real.cb7"),
        OsStr::new("page1.png"),
        OsStr::new("page2.png"),
        OsStr::new("ComicInfo.xml"),
    ];
    run_external_tool(&sevenz, &args, &stage, EXTERNAL_TOOL_TIMEOUT);
    let real_cb7 = stage.join("real.cb7");

    let thumb = generate_document_thumbnail(&real_cb7, 128, 128)
        .expect("real cb7 thumbnail result")
        .expect("real cb7 thumbnail image");
    assert_solid_color_close(&thumb, [230, 30, 30]);

    for (idx, color) in [(0usize, [230, 30, 30]), (1, [30, 30, 230])] {
        let preview = load_document_preview(&real_cb7, idx, false).expect("real cb7 preview");
        match preview {
            DocumentPreview::PagedImage {
                image,
                page_index,
                total_pages,
            } => {
                assert_eq!(page_index, idx);
                assert_eq!(total_pages, 2);
                assert_solid_color_close(&image, color);
            }
            other => panic!("expected PagedImage for real cb7 page {idx}, got {other:?}"),
        }
    }

    let real_meta = extract_document_metadata(&real_cb7).expect("real cb7 metadata");
    assert_eq!(
        real_meta["Document"].get("Pages").map(String::as_str),
        Some("2")
    );
    assert_eq!(
        real_meta["Document"].get("Title").map(String::as_str),
        Some("SevenZ Issue")
    );
    assert_eq!(
        real_meta["Document"].get("Series").map(String::as_str),
        Some("CB7 Suite")
    );
}

#[test]
fn test_cbr_corrupt_empty_and_real_when_rar_available() {
    let dir = TempDir::new("cbr");
    let empty_cbr = dir.path().join("empty.cbr");
    let corrupt_cbr = dir.path().join("corrupt.cbr");
    fs::write(&empty_cbr, b"").expect("write empty cbr");
    fs::write(&corrupt_cbr, b"not a valid rar archive").expect("write corrupt cbr");

    for bad in [&empty_cbr, &corrupt_cbr] {
        let thumb_res = generate_document_thumbnail(bad, 128, 128);
        assert!(
            !matches!(thumb_res, Ok(Some(_))),
            "corrupt/empty cbr must not yield a thumbnail: {thumb_res:?}"
        );
        let prev_res = load_document_preview(bad, 0, false);
        assert!(
            !matches!(prev_res, Ok(DocumentPreview::PagedImage { .. })),
            "corrupt/empty cbr must not yield a page: {prev_res:?}"
        );
        let meta = extract_document_metadata(bad).expect("cbr metadata wrapper");
        assert_eq!(
            meta["Document"].get("Kind").map(String::as_str),
            Some("Comic Archive")
        );
    }

    if is_sandboxed() {
        return;
    }
    let Some(rar_bin) = find_executable_on_path(&[RAR_BIN], false) else {
        return;
    };

    let stage = dir.path().join("stage");
    fs::create_dir(&stage).expect("create cbr staging dir");
    fs::write(stage.join("page1.png"), solid_png_bytes(220, 40, 40)).expect("write page1");
    fs::write(stage.join("page2.png"), solid_png_bytes(40, 200, 40)).expect("write page2");

    let args: [&OsStr; 7] = [
        OsStr::new("a"),
        OsStr::new("-ep"),
        OsStr::new("-idq"),
        OsStr::new("--"),
        OsStr::new("real.cbr"),
        OsStr::new("page1.png"),
        OsStr::new("page2.png"),
    ];
    run_external_tool(&rar_bin, &args, &stage, EXTERNAL_TOOL_TIMEOUT);
    let real_cbr = stage.join("real.cbr");

    let thumb = generate_document_thumbnail(&real_cbr, 128, 128)
        .expect("real cbr thumbnail result")
        .expect("real cbr thumbnail image");
    assert_solid_color_close(&thumb, [220, 40, 40]);

    let preview = load_document_preview(&real_cbr, 1, false).expect("real cbr preview");
    match preview {
        DocumentPreview::PagedImage {
            image,
            page_index,
            total_pages,
        } => {
            assert_eq!(page_index, 1);
            assert_eq!(total_pages, 2);
            assert_solid_color_close(&image, [40, 200, 40]);
        }
        other => panic!("expected PagedImage for real cbr page 1, got {other:?}"),
    }
}

#[test]
fn test_zip_bomb_entry_exceeding_cap_rejected() {
    let dir = TempDir::new("zip_bomb");
    let bomb_cbz = dir.path().join("bomb.cbz");

    {
        let file = File::create(&bomb_cbz).expect("create bomb cbz");
        let mut zip = ZipWriter::new(BufWriter::new(file));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("bomb.png", options)
            .expect("start bomb entry");
        let chunk = [0u8; 64 * 1024];
        let mut remaining = OVERSIZED_ZIP_ENTRY_BYTES;
        while remaining > 0 {
            let n = remaining.min(chunk.len());
            zip.write_all(&chunk[..n]).expect("write zero chunk");
            remaining -= n;
        }
        let writer = zip.finish().expect("finish bomb zip");
        let _file = writer.into_inner().expect("flush bomb file");
    }

    assert!(
        generate_document_thumbnail(&bomb_cbz, 128, 128).is_err(),
        "oversized archive entry must be rejected during thumbnail generation"
    );

    let preview = load_document_preview(&bomb_cbz, 0, false).expect("bomb preview result");
    assert!(
        matches!(
            preview,
            DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::TooLarge
            }
        ),
        "expected TooLarge for zip bomb, got {preview:?}"
    );
}

#[test]
fn test_repeated_multipage_pdf_cbz_and_cbt_page_turns() {
    let dir = TempDir::new("repeat_turns");

    // 1. PDF repeated turns across cached pages 0 <-> 1
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

    // 2. CBZ repeated turns across cached index 0 -> 1 -> 2 -> 0 -> 2
    let cbz_path = dir.path().join("turns.cbz");
    let red = solid_png_bytes(245, 10, 10);
    let green = solid_png_bytes(10, 240, 10);
    let blue = solid_png_bytes(10, 10, 245);
    write_zip_entries(
        &cbz_path,
        &[
            ("page10.png", &blue, CompressionMethod::Deflated),
            ("page1.png", &red, CompressionMethod::Deflated),
            ("page2.png", &green, CompressionMethod::Deflated),
        ],
    );
    let cbz_expected = [
        (0usize, [245u8, 10, 10]),
        (1, [10, 240, 10]),
        (2, [10, 10, 245]),
        (0, [245, 10, 10]),
        (2, [10, 10, 245]),
    ];
    for (req, color) in cbz_expected {
        match load_document_preview(&cbz_path, req, false).expect("cbz repeat preview") {
            DocumentPreview::PagedImage {
                image,
                page_index,
                total_pages,
            } => {
                assert_eq!((page_index, total_pages), (req, 3));
                assert_solid_color_close(&image, color);
            }
            other => panic!("expected PagedImage for CBZ page {req}, got {other:?}"),
        }
    }

    // 3. CBT repeated O(1) seek turns across 0 -> 1 -> 2 -> 0 -> 2
    let cbt_path = dir.path().join("turns.cbt");
    {
        let file = File::create(&cbt_path).expect("create cbt");
        let mut builder = tar::Builder::new(file);
        for (name, bytes) in [
            ("page10.png", &blue),
            ("page2.png", &green),
            ("page1.png", &red),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, name, Cursor::new(bytes.as_slice()))
                .expect("append cbt entry");
        }
        builder.finish().expect("finish cbt");
    }
    for (req, color) in cbz_expected {
        match load_document_preview(&cbt_path, req, false).expect("cbt repeat preview") {
            DocumentPreview::PagedImage {
                image,
                page_index,
                total_pages,
            } => {
                assert_eq!((page_index, total_pages), (req, 3));
                assert_solid_color_close(&image, color);
            }
            other => panic!("expected PagedImage for CBT page {req}, got {other:?}"),
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
