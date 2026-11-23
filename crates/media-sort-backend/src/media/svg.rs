//! Hardened pure-Rust SVG and SVGZ decoding and rasterization via `resvg`.
//!
//! Enforces strict file size, gzip decompression, data-URI size, and render
//! dimension bounds, and blocks all external/filesystem `<image href>` lookups.

use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use super::DecodedImage;

const MAX_SVG_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SVG_DATA_URI_BYTES: usize = 8 * 1024 * 1024;
const MAX_SVG_RENDER_DIM: u32 = 4096;
const MAX_DYNAMIC_SVG_DIM: u32 = 1920;

/// Returns `true` when `ext` (without leading dot) is an SVG or SVGZ extension.
pub fn is_svg_extension(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz")
}

/// Returns `true` when `path` has an `.svg` or `.svgz` extension (case-insensitive).
pub fn is_svg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(is_svg_extension)
}

fn system_fonts() -> &'static Arc<resvg::usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
}

fn bounded_data_resolver() -> resvg::usvg::ImageHrefDataResolverFn<'static> {
    let default = resvg::usvg::ImageHrefResolver::default_data_resolver();
    Box::new(move |mime, data, opts| {
        if data.len() > MAX_SVG_DATA_URI_BYTES {
            return None;
        }
        default(mime, data, opts)
    })
}

fn hardened_options(with_system_fonts: bool) -> resvg::usvg::Options<'static> {
    let fontdb = if with_system_fonts {
        Arc::clone(system_fonts())
    } else {
        Arc::new(resvg::usvg::fontdb::Database::new())
    };

    resvg::usvg::Options {
        resources_dir: None,
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: bounded_data_resolver(),
            // Block all local filesystem and external URI image references.
            resolve_string: Box::new(|_, _| None),
        },
        fontdb,
        ..resvg::usvg::Options::default()
    }
}

fn svg_decoding_error(
    msg: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> image::ImageError {
    image::ImageError::Decoding(image::error::DecodingError::new(
        image::error::ImageFormatHint::Name("svg".to_string()),
        msg,
    ))
}

fn svg_limit_error(kind: image::error::LimitErrorKind) -> image::ImageError {
    image::ImageError::Limits(image::error::LimitError::from_kind(kind))
}

fn read_svg_utf8(path: &Path) -> Result<String, image::ImageError> {
    let file = std::fs::File::open(path)?;
    let meta_len = file.metadata()?.len();
    if meta_len > MAX_SVG_FILE_BYTES {
        return Err(svg_limit_error(
            image::error::LimitErrorKind::InsufficientMemory,
        ));
    }

    let prealloc = usize::try_from(meta_len)
        .unwrap_or(0)
        .min(MAX_SVG_FILE_BYTES as usize);
    let mut raw = Vec::with_capacity(prealloc);
    file.take(MAX_SVG_FILE_BYTES + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_SVG_FILE_BYTES {
        return Err(svg_limit_error(
            image::error::LimitErrorKind::InsufficientMemory,
        ));
    }

    // Handle gzip-compressed SVGZ (and gzip-magic payloads even if named .svg)
    // with a strict decompressed byte cap to defeat gzip bombs.
    let bytes = if raw.starts_with(&[0x1f, 0x8b]) {
        let decompressed_cap = raw.len().saturating_mul(2).min(1024 * 1024);
        let mut out = Vec::with_capacity(decompressed_cap);
        flate2::read::GzDecoder::new(&raw[..])
            .take(MAX_SVG_FILE_BYTES + 1)
            .read_to_end(&mut out)
            .map_err(|e| svg_decoding_error(format!("invalid gzip SVG stream: {e}")))?;
        if out.len() as u64 > MAX_SVG_FILE_BYTES {
            return Err(svg_limit_error(
                image::error::LimitErrorKind::InsufficientMemory,
            ));
        }
        out
    } else {
        raw
    };

    String::from_utf8(bytes)
        .map_err(|e| svg_decoding_error(format!("SVG content is not valid UTF-8: {e}")))
}

fn parse_svg_tree(
    text: &str,
    with_system_fonts: bool,
) -> Result<resvg::usvg::Tree, image::ImageError> {
    let opts = hardened_options(with_system_fonts);
    match catch_unwind(AssertUnwindSafe(|| {
        resvg::usvg::Tree::from_str(text, &opts)
    })) {
        Ok(Ok(tree)) => Ok(tree),
        Ok(Err(e)) => Err(svg_decoding_error(format!("SVG parse failed: {e}"))),
        Err(_) => Err(svg_decoding_error("SVG parser panicked")),
    }
}

fn parse_svg_from_path(
    path: &Path,
    with_system_fonts: bool,
) -> Result<resvg::usvg::Tree, image::ImageError> {
    let text = read_svg_utf8(path)?;
    parse_svg_tree(&text, with_system_fonts)
}

fn tree_dimensions(tree: &resvg::usvg::Tree) -> Result<(u32, u32), image::ImageError> {
    let size = tree.size();
    let w = size.width();
    let h = size.height();
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return Err(svg_decoding_error(
            "SVG has non-finite or non-positive dimensions",
        ));
    }
    let clamped_w = w.round().clamp(1.0, MAX_SVG_RENDER_DIM as f32) as u32;
    let clamped_h = h.round().clamp(1.0, MAX_SVG_RENDER_DIM as f32) as u32;
    Ok((clamped_w, clamped_h))
}

fn render_svg_tree(
    tree: &resvg::usvg::Tree,
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    let size = tree.size();
    let tree_w = size.width();
    let tree_h = size.height();
    if !tree_w.is_finite() || !tree_h.is_finite() || tree_w <= 0.0 || tree_h <= 0.0 {
        return Err(svg_decoding_error(
            "SVG has non-finite or non-positive dimensions",
        ));
    }

    let fit_w = max_width.clamp(1, MAX_SVG_RENDER_DIM);
    let fit_h = max_height.clamp(1, MAX_SVG_RENDER_DIM);
    let scale = (f64::from(fit_w) / f64::from(tree_w)).min(f64::from(fit_h) / f64::from(tree_h));
    if !scale.is_finite() || scale <= 0.0 {
        return Err(svg_decoding_error("SVG render scale is invalid"));
    }

    let dst_w = (f64::from(tree_w) * scale)
        .round()
        .clamp(1.0, f64::from(fit_w)) as u32;
    let dst_h = (f64::from(tree_h) * scale)
        .round()
        .clamp(1.0, f64::from(fit_h)) as u32;

    let sx = dst_w as f32 / tree_w;
    let sy = dst_h as f32 / tree_h;
    if !sx.is_finite() || !sy.is_finite() || sx <= 0.0 || sy <= 0.0 {
        return Err(svg_decoding_error("SVG transform scale is invalid"));
    }

    let mut pixmap = resvg::tiny_skia::Pixmap::new(dst_w, dst_h)
        .ok_or_else(|| svg_limit_error(image::error::LimitErrorKind::DimensionError))?;
    let transform = resvg::tiny_skia::Transform::from_scale(sx, sy);

    let render_res = catch_unwind(AssertUnwindSafe(|| {
        resvg::render(tree, transform, &mut pixmap.as_mut());
    }));
    if render_res.is_err() {
        return Err(svg_decoding_error("SVG renderer panicked"));
    }

    let mut rgba = Vec::with_capacity(pixmap.data().len());
    for p in pixmap.pixels() {
        if p.alpha() == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let c = p.demultiply();
            rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
    }

    Ok(DecodedImage::new(dst_w, dst_h, rgba))
}

/// Parses the SVG/SVGZ file at `path` (without loading system fonts) and returns
/// its declared `(width, height)` clamped to `1..=4096`.
pub fn svg_dimensions(path: &Path) -> Result<(u32, u32), image::ImageError> {
    let tree = parse_svg_from_path(path, false)?;
    tree_dimensions(&tree)
}

/// Renders the SVG/SVGZ file at `path` directly to fit inside `max_width` x `max_height`
/// (clamped to `1..=4096`), preserving aspect ratio and returning straight RGBA8 pixels.
pub fn render_svg(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<DecodedImage, image::ImageError> {
    let tree = parse_svg_from_path(path, true)?;
    render_svg_tree(&tree, max_width, max_height)
}

/// Renders the SVG/SVGZ file at `path` at its intrinsic dimensions capped to `1920x1920`
/// (preserving aspect ratio) and returns it as a [`image::DynamicImage`].
pub fn load_svg_dynamic(path: &Path) -> Result<image::DynamicImage, image::ImageError> {
    let tree = parse_svg_from_path(path, true)?;
    let (w, h) = tree_dimensions(&tree)?;
    let decoded = render_svg_tree(
        &tree,
        w.clamp(1, MAX_DYNAMIC_SVG_DIM),
        h.clamp(1, MAX_DYNAMIC_SVG_DIM),
    )?;
    super::image_decoder::rgba_to_dynamic(decoded.width, decoded.height, decoded.rgba)
}
