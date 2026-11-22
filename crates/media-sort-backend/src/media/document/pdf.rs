use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::peniko::ImageAlphaType;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

use super::{
    DecodedImage, DocumentError, DocumentPreview, DocumentUnavailableReason, MAX_PDF_FILE_BYTES,
    PREVIEW_TARGET_HEIGHT, PREVIEW_TARGET_WIDTH, read_file_capped,
};

enum PdfLoadOutcome {
    Loaded(Pdf),
    Encrypted,
}

fn load_pdf(path: &Path) -> Result<PdfLoadOutcome, DocumentError> {
    let bytes = read_file_capped(path, MAX_PDF_FILE_BYTES)?;
    match Pdf::new(Arc::new(bytes)) {
        Ok(pdf) => Ok(PdfLoadOutcome::Loaded(pdf)),
        Err(LoadPdfError::Decryption(_)) => Ok(PdfLoadOutcome::Encrypted),
        Err(LoadPdfError::Invalid) => Err(DocumentError::Message("Invalid PDF file".to_string())),
    }
}

fn render_page_from_pdf(
    pdf: &Pdf,
    page_index: usize,
    max_width: u32,
    max_height: u32,
) -> Result<(DecodedImage, usize), DocumentError> {
    let pages = pdf.pages();
    if pages.is_empty() {
        return Err(DocumentError::Message("PDF has no pages".to_string()));
    }
    let total_pages = pages.len();
    let clamped = page_index.min(total_pages - 1);
    let page = &pages[clamped];

    let (page_w, page_h) = page.render_dimensions();
    if !page_w.is_finite() || !page_h.is_finite() || page_w <= 0.0 || page_h <= 0.0 {
        return Err(DocumentError::Message(
            "PDF page has invalid dimensions".to_string(),
        ));
    }

    let raw_scale = (max_width as f32 / page_w).min(max_height as f32 / page_h);
    // Keep dimensions within u16 bounds and prevent sub-pixel collapses.
    let max_safe_scale = (6000.0 / page_w.max(page_h)).clamp(0.05, 4.0);
    let scale = raw_scale.clamp(0.05, max_safe_scale.max(0.05));

    let cache = RenderCache::new();
    let interp = InterpreterSettings::default();
    let render_settings = RenderSettings::default();
    let pixmap_settings = PixmapSettings {
        x_scale: scale,
        y_scale: scale,
        bg_color: WHITE,
    };

    let pixmap = hayro::render(page, &cache, &interp, &render_settings, &pixmap_settings);
    let width = u32::from(pixmap.width());
    let height = u32::from(pixmap.height());
    if width == 0 || height == 0 {
        return Err(DocumentError::Message(
            "Rendered PDF page has zero size".to_string(),
        ));
    }
    let rgba = pixmap.take_rgba8(ImageAlphaType::Alpha);
    Ok((DecodedImage::new(width, height, rgba), total_pages))
}

/// Renders page 0 of a PDF as a thumbnail. Encrypted or page-less PDFs cleanly return `Ok(None)`.
pub fn render_pdf_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let loaded = load_pdf(path)?;
        let PdfLoadOutcome::Loaded(pdf) = loaded else {
            return Ok(None);
        };
        if pdf.pages().is_empty() {
            return Ok(None);
        }
        let (img, _) = render_page_from_pdf(&pdf, 0, max_width, max_height)?;
        Ok(Some(img))
    }));

    match outcome {
        Ok(res) => res,
        Err(_) => Err(DocumentError::Message(
            "PDF renderer panicked while generating thumbnail".to_string(),
        )),
    }
}

/// Renders `page_index` of `path` at high resolution for the preview pane.
pub fn load_pdf_preview(path: &Path, page_index: usize) -> Result<DocumentPreview, DocumentError> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let file_len = std::fs::metadata(path)?.len();
        if file_len > MAX_PDF_FILE_BYTES {
            return Ok(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::TooLarge,
            });
        }
        let loaded = load_pdf(path)?;
        let PdfLoadOutcome::Loaded(pdf) = loaded else {
            return Ok(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::Encrypted,
            });
        };
        if pdf.pages().is_empty() {
            return Ok(DocumentPreview::Unavailable {
                reason: DocumentUnavailableReason::UnsupportedOrCorrupt(
                    "PDF contains no pages".to_string(),
                ),
            });
        }
        let (image, total_pages) = render_page_from_pdf(
            &pdf,
            page_index,
            PREVIEW_TARGET_WIDTH,
            PREVIEW_TARGET_HEIGHT,
        )?;
        let clamped_page = page_index.min(total_pages - 1);
        Ok(DocumentPreview::PagedImage {
            image,
            page_index: clamped_page,
            total_pages,
        })
    }));

    match outcome {
        Ok(res) => res,
        Err(_) => Err(DocumentError::Message(
            "PDF renderer panicked while rendering preview".to_string(),
        )),
    }
}

/// Renders `page_index` from an in-memory PDF buffer (used by the LibreOffice headless PDF bridge).
pub(crate) fn render_pdf_bytes_page(
    bytes: Vec<u8>,
    page_index: usize,
    max_width: u32,
    max_height: u32,
) -> Result<(DecodedImage, usize), DocumentError> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let pdf = Pdf::new(Arc::new(bytes))
            .map_err(|_| DocumentError::Message("Failed to parse converted PDF".to_string()))?;
        render_page_from_pdf(&pdf, page_index, max_width, max_height)
    }));
    match outcome {
        Ok(res) => res,
        Err(_) => Err(DocumentError::Message(
            "PDF renderer panicked while rendering converted document".to_string(),
        )),
    }
}

/// Extracts PDF document metadata fields for the metadata inspector.
pub fn extract_pdf_metadata(path: &Path) -> Result<BTreeMap<String, String>, DocumentError> {
    let mut fields = BTreeMap::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let loaded = load_pdf(path)?;
        let PdfLoadOutcome::Loaded(pdf) = loaded else {
            fields.insert("Encrypted".into(), "Yes".into());
            return Ok::<_, DocumentError>(());
        };
        fields.insert("Pages".into(), pdf.pages().len().to_string());
        let meta = pdf.metadata();
        if let Some(title) = meta.title.as_deref().and_then(decode_pdf_string) {
            fields.insert("Title".into(), title);
        }
        if let Some(author) = meta.author.as_deref().and_then(decode_pdf_string) {
            fields.insert("Author".into(), author);
        }
        if let Some(subject) = meta.subject.as_deref().and_then(decode_pdf_string) {
            fields.insert("Subject".into(), subject);
        }
        if let Some(creator) = meta.creator.as_deref().and_then(decode_pdf_string) {
            fields.insert("Creator".into(), creator);
        }
        if let Some(producer) = meta.producer.as_deref().and_then(decode_pdf_string) {
            fields.insert("Producer".into(), producer);
        }
        if let Some(dt) = meta.creation_date {
            fields.insert(
                "Created".into(),
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
                ),
            );
        }
        Ok(())
    }));

    match outcome {
        Ok(res) => {
            res?;
            Ok(fields)
        }
        Err(_) => Err(DocumentError::Message(
            "PDF parser panicked while reading metadata".to_string(),
        )),
    }
}

fn decode_pdf_string(bytes: &[u8]) -> Option<String> {
    let text = if bytes.starts_with(&[0xFE, 0xFF]) {
        let pairs = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c));
        char::decode_utf16(pairs)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        let pairs = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c));
        char::decode_utf16(pairs)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
    } else if let Ok(s) = std::str::from_utf8(bytes) {
        s.to_string()
    } else {
        // Fallback byte-per-char Latin-1 decoding.
        bytes.iter().map(|&b| char::from(b)).collect()
    };
    let trimmed = text.trim().trim_matches('\0').trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}
