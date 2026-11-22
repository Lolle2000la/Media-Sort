use std::fs::{self, File};
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::Compression;
use flate2::write::GzEncoder;
use image::GenericImageView;
use media_sort_backend::media::DecodedImage;
use media_sort_backend::media::image_decoder::{
    decode_image_dimensions, generate_thumbnail as decoder_generate_thumbnail, load_image,
    load_preview,
};
use media_sort_backend::media::thumbnail::{generate_thumbnail, thumbnail_dimensions};
use media_sort_backend::metadata::image_meta::extract_image_metadata;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

const SAMPLE_RED_RECT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">
  <rect x="0" y="0" width="24" height="24" fill="#ff0000"/>
</svg>"##;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let pid = std::process::id();
        let count = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("mediasort_svg_{prefix}_{pid}_{count}"));
        fs::create_dir_all(&dir).expect("create temp svg test dir");
        Self { path: dir }
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

fn write_gz(path: &Path, payload: &[u8]) {
    let file = File::create(path).expect("create svgz file");
    let mut encoder = GzEncoder::new(file, Compression::fast());
    encoder.write_all(payload).expect("compress svgz payload");
    encoder.finish().expect("finish svgz stream");
}

fn assert_solid_red(img: &DecodedImage, expected_w: u32, expected_h: u32) {
    assert_eq!(img.width, expected_w);
    assert_eq!(img.height, expected_h);
    assert_eq!(
        img.rgba.len(),
        (expected_w as usize) * (expected_h as usize) * 4
    );
    let cx = (expected_w / 2) as usize;
    let cy = (expected_h / 2) as usize;
    let idx = (cy * expected_w as usize + cx) * 4;
    let px = &img.rgba[idx..idx + 4];
    assert!(px[0] > 200, "expected red > 200, got {:?}", px);
    assert!(px[1] < 20, "expected green < 20, got {:?}", px);
    assert!(px[2] < 20, "expected blue < 20, got {:?}", px);
    assert_eq!(px[3], 255, "expected opaque pixel, got {:?}", px);
}

fn xml_escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[test]
fn svg_thumbnail_preview_dimensions_and_metadata() {
    let dir = TempDir::new("basic_svg");
    let svg_path = dir.join("red_square.svg");
    fs::write(&svg_path, SAMPLE_RED_RECT_SVG).expect("write sample svg");

    let thumb = generate_thumbnail(&svg_path, 128, 128)
        .expect("thumbnail should succeed")
        .expect("svg should produce thumbnail");
    assert_solid_red(&thumb, 128, 128);

    let dyn_thumb =
        decoder_generate_thumbnail(&svg_path, 128, 128).expect("decoder thumbnail should succeed");
    assert_eq!(dyn_thumb.dimensions(), (128, 128));

    let preview = load_preview(&svg_path, 1920, 1440).expect("preview should succeed");
    assert_solid_red(&preview, 1440, 1440);

    let loaded = load_image(&svg_path).expect("load_image should succeed");
    assert_eq!(loaded.dimensions(), (24, 24));

    let dims = decode_image_dimensions(&svg_path).expect("decode_image_dimensions should succeed");
    assert_eq!(dims, (24, 24));

    let thumb_dims = thumbnail_dimensions(&svg_path).expect("thumbnail_dimensions should succeed");
    assert_eq!(thumb_dims, Some((24, 24)));

    let meta = extract_image_metadata(&svg_path).expect("extract_image_metadata should succeed");
    let file_section = meta.get("File").expect("File section missing");
    assert_eq!(
        file_section.get("Dimensions").map(String::as_str),
        Some("24 x 24")
    );
}

#[test]
fn svgz_thumbnail_preview_and_dimensions() {
    let dir = TempDir::new("basic_svgz");
    let svgz_path = dir.join("red_square.svgz");
    write_gz(&svgz_path, SAMPLE_RED_RECT_SVG.as_bytes());

    let thumb = generate_thumbnail(&svgz_path, 128, 128)
        .expect("svgz thumbnail should succeed")
        .expect("svgz should produce thumbnail");
    assert_solid_red(&thumb, 128, 128);

    let preview = load_preview(&svgz_path, 1920, 1440).expect("svgz preview should succeed");
    assert_solid_red(&preview, 1440, 1440);

    let dims = decode_image_dimensions(&svgz_path).expect("svgz dimensions should succeed");
    assert_eq!(dims, (24, 24));

    let thumb_dims =
        thumbnail_dimensions(&svgz_path).expect("svgz thumbnail_dimensions should succeed");
    assert_eq!(thumb_dims, Some((24, 24)));

    let meta = extract_image_metadata(&svgz_path).expect("svgz metadata should succeed");
    let file_section = meta.get("File").expect("File section missing");
    assert_eq!(
        file_section.get("Dimensions").map(String::as_str),
        Some("24 x 24")
    );
}

#[test]
fn svg_with_text_renders_without_error_or_panic() {
    let dir = TempDir::new("text_svg");
    let svg_path = dir.join("label.svg");
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="80" viewBox="0 0 200 80">
  <rect x="0" y="0" width="200" height="80" fill="#ffffff"/>
  <text x="16" y="48" font-size="24" fill="#111111">MediaSort SVG</text>
</svg>"##;
    fs::write(&svg_path, svg).expect("write text svg");

    let thumb = generate_thumbnail(&svg_path, 128, 128)
        .expect("text svg thumbnail should succeed")
        .expect("text svg should yield thumbnail");
    assert_eq!((thumb.width, thumb.height), (128, 51));
    assert_eq!(thumb.rgba.len(), 128 * 51 * 4);

    let preview = load_preview(&svg_path, 1920, 1440).expect("text svg preview should succeed");
    assert_eq!((preview.width, preview.height), (1920, 768));
    assert_eq!(preview.rgba.len(), 1920 * 768 * 4);
}

#[test]
fn svg_blocks_local_filesystem_image_href() {
    let dir = TempDir::new("block_href");
    let png_path = dir.join("secret.png");
    let red_png = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 0, 0, 255]));
    red_png
        .save_with_format(&png_path, image::ImageFormat::Png)
        .expect("save secret png");

    let escaped_abs = xml_escape_attr(&png_path.to_string_lossy());
    let svg_abs = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <image href="{escaped_abs}" width="64" height="64"/>
</svg>"#
    );
    let svg_abs_path = dir.join("leak_abs.svg");
    fs::write(&svg_abs_path, svg_abs).expect("write abs href svg");

    let thumb = generate_thumbnail(&svg_abs_path, 64, 64)
        .expect("render should succeed while ignoring external href")
        .expect("thumbnail expected");
    assert_eq!((thumb.width, thumb.height), (64, 64));
    assert!(
        thumb.rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 0),
        "external local file image href must be blocked (expected all pixels alpha == 0)"
    );

    let preview = load_preview(&svg_abs_path, 64, 64)
        .expect("preview should succeed while ignoring external href");
    assert!(
        preview.rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 0),
        "preview must also block external local file image href"
    );

    let svg_rel = r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <image href="secret.png" width="64" height="64"/>
</svg>"#;
    let svg_rel_path = dir.join("leak_rel.svg");
    fs::write(&svg_rel_path, svg_rel).expect("write rel href svg");

    let rel_thumb = generate_thumbnail(&svg_rel_path, 64, 64)
        .expect("render should succeed while ignoring relative href")
        .expect("thumbnail expected");
    assert!(
        rel_thumb
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| px[3] == 0),
        "relative local file image href must be blocked"
    );
}

#[test]
fn svgz_gzip_bomb_is_rejected_without_oom() {
    let dir = TempDir::new("gzip_bomb");
    let bomb_path = dir.join("bomb.svgz");
    {
        let file = File::create(&bomb_path).expect("create bomb svgz");
        let mut encoder = GzEncoder::new(file, Compression::fast());
        encoder
            .write_all(br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">"#)
            .expect("write svg header");
        let spaces = [b' '; 64 * 1024];
        let target_bytes = 17 * 1024 * 1024 + 1024;
        let mut written = 0usize;
        while written < target_bytes {
            let chunk = (target_bytes - written).min(spaces.len());
            encoder
                .write_all(&spaces[..chunk])
                .expect("write whitespace chunk");
            written += chunk;
        }
        encoder.write_all(b"</svg>").expect("write svg footer");
        encoder.finish().expect("finish gzip bomb");
    }

    assert!(generate_thumbnail(&bomb_path, 128, 128).is_err());
    assert!(load_preview(&bomb_path, 1920, 1440).is_err());
    assert!(decode_image_dimensions(&bomb_path).is_err());
    assert!(thumbnail_dimensions(&bomb_path).is_err());

    // Even if disguised with a .svg extension, gzip magic must still trigger the bounded guard.
    let disguised_path = dir.join("bomb_disguised.svg");
    fs::copy(&bomb_path, &disguised_path).expect("copy gzip bomb as .svg");
    assert!(generate_thumbnail(&disguised_path, 128, 128).is_err());
    assert!(decode_image_dimensions(&disguised_path).is_err());
}

#[test]
fn malformed_svg_returns_err_without_panicking() {
    let dir = TempDir::new("malformed");
    let cases: [(&str, &[u8]); 5] = [
        ("unclosed.svg", b"<svg><rect"),
        ("not_svg.svg", b"<html><body>not an svg</body></html>"),
        ("empty.svg", b""),
        ("invalid_utf8.svg", &[0xff, 0xfe, 0x00, 0x80]),
        (
            "zero_size.svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"/>"#,
        ),
    ];

    for (name, content) in cases {
        let path = dir.join(name);
        fs::write(&path, content).expect("write malformed case");
        assert!(
            generate_thumbnail(&path, 128, 128).is_err(),
            "expected Err for {name} thumbnail"
        );
        assert!(
            load_preview(&path, 1920, 1440).is_err(),
            "expected Err for {name} preview"
        );
        assert!(
            decode_image_dimensions(&path).is_err(),
            "expected Err for {name} dimensions"
        );
        assert!(
            thumbnail_dimensions(&path).is_err(),
            "expected Err for {name} thumbnail_dimensions"
        );
        assert!(
            load_image(&path).is_err(),
            "expected Err for {name} load_image"
        );
    }
}
