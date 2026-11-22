use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::page::Pages;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::peniko::ImageAlphaType;
use hayro::{PixmapSettings, RenderCache, RenderSettings};

use super::poppler_pipe::{self, PopplerOutcome};
use super::{
    CanonicalFileKey, DecodedImage, DocumentError, DocumentPreview, DocumentUnavailableReason,
    MAX_PDF_FILE_BYTES, PREVIEW_TARGET_HEIGHT, PREVIEW_TARGET_WIDTH, read_file_capped,
};

const PDF_DOC_CACHE_CAPACITY: usize = 4;
const HAYRO_WORKER_QUEUE_CAPACITY: usize = 64;
const MAX_PAGE_SCALE: f32 = 2.5;

type PdfDocCache = Mutex<VecDeque<(PdfDocKey, Arc<Pdf>)>>;

static PDF_DOC_CACHE: OnceLock<PdfDocCache> = OnceLock::new();
static HAYRO_RENDER_WORKER: OnceLock<Mutex<mpsc::SyncSender<HayroRenderRequest>>> = OnceLock::new();

#[derive(Clone, PartialEq, Eq)]
enum PdfDocKey {
    File(CanonicalFileKey),
    Bytes { len: usize, hash: u64 },
}

enum PdfLoadOutcome {
    Loaded(Arc<Pdf>),
    Encrypted,
}

struct HayroRenderRequest {
    key: PdfDocKey,
    pdf: Arc<Pdf>,
    page_index: usize,
    max_width: u32,
    max_height: u32,
    reply: mpsc::SyncSender<Result<(DecodedImage, usize), DocumentError>>,
}

fn pdf_doc_cache() -> &'static Mutex<VecDeque<(PdfDocKey, Arc<Pdf>)>> {
    PDF_DOC_CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(PDF_DOC_CACHE_CAPACITY)))
}

fn doc_cache_get(key: &PdfDocKey) -> Option<Arc<Pdf>> {
    let mut cache = pdf_doc_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pos = cache.iter().position(|(k, _)| k == key)?;
    let (k, pdf) = cache.remove(pos)?;
    let out = Arc::clone(&pdf);
    cache.push_back((k, pdf));
    Some(out)
}

fn doc_cache_insert(key: PdfDocKey, pdf: Arc<Pdf>) {
    let mut cache = pdf_doc_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(pos) = cache.iter().position(|(k, _)| k == &key) {
        cache.remove(pos);
    }
    while cache.len() >= PDF_DOC_CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back((key, pdf));
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{DefaultHasher, Hasher as _};
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

fn load_pdf_file_cached(path: &Path) -> Result<(PdfDocKey, PdfLoadOutcome), DocumentError> {
    let canonical = std::fs::canonicalize(path)?;
    let meta = std::fs::metadata(&canonical)?;
    if meta.len() > MAX_PDF_FILE_BYTES {
        return Err(DocumentError::Message(format!(
            "File exceeds size limit ({} > {MAX_PDF_FILE_BYTES} bytes)",
            meta.len()
        )));
    }
    let key = PdfDocKey::File(CanonicalFileKey::from_canonical_and_meta(
        canonical.clone(),
        &meta,
    ));
    if let Some(cached) = doc_cache_get(&key) {
        return Ok((key, PdfLoadOutcome::Loaded(cached)));
    }

    let bytes = read_file_capped(&canonical, MAX_PDF_FILE_BYTES)?;
    let parsed =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Pdf::new(Arc::new(bytes))))
            .map_err(|_| DocumentError::Message("PDF parser panicked".to_string()))?;

    match parsed {
        Ok(pdf) => {
            let arc = Arc::new(pdf);
            doc_cache_insert(key.clone(), Arc::clone(&arc));
            Ok((key, PdfLoadOutcome::Loaded(arc)))
        }
        Err(LoadPdfError::Decryption(_)) => Ok((key, PdfLoadOutcome::Encrypted)),
        Err(LoadPdfError::Invalid) => Err(DocumentError::Message("Invalid PDF file".to_string())),
    }
}

fn load_pdf_bytes_cached(bytes: Vec<u8>) -> Result<(PdfDocKey, Arc<Pdf>), DocumentError> {
    if bytes.len() as u64 > MAX_PDF_FILE_BYTES {
        return Err(DocumentError::Message(format!(
            "PDF buffer exceeds size limit ({} > {MAX_PDF_FILE_BYTES} bytes)",
            bytes.len()
        )));
    }
    let key = PdfDocKey::Bytes {
        len: bytes.len(),
        hash: hash_bytes(&bytes),
    };
    if let Some(cached) = doc_cache_get(&key) {
        return Ok((key, cached));
    }

    let parsed =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Pdf::new(Arc::new(bytes))))
            .map_err(|_| DocumentError::Message("PDF parser panicked".to_string()))?;
    let pdf =
        parsed.map_err(|_| DocumentError::Message("Failed to parse converted PDF".to_string()))?;
    let arc = Arc::new(pdf);
    doc_cache_insert(key.clone(), Arc::clone(&arc));
    Ok((key, arc))
}

fn hayro_worker_sender() -> &'static Mutex<mpsc::SyncSender<HayroRenderRequest>> {
    HAYRO_RENDER_WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel(HAYRO_WORKER_QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("hayro-pdf-worker".to_string())
            .spawn(move || hayro_worker_loop(rx))
            .expect("spawn hayro PDF render worker");
        Mutex::new(tx)
    })
}

fn hayro_worker_loop(rx: mpsc::Receiver<HayroRenderRequest>) {
    let mut pending_next: Option<HayroRenderRequest> = None;
    loop {
        let first = match pending_next.take() {
            Some(req) => req,
            None => match rx.recv() {
                Ok(req) => req,
                Err(_) => break,
            },
        };
        pending_next = run_active_pdf_session(first, &rx);
    }
}

fn run_active_pdf_session(
    first: HayroRenderRequest,
    rx: &mpsc::Receiver<HayroRenderRequest>,
) -> Option<HayroRenderRequest> {
    let active_key = first.key.clone();
    let active_pdf = Arc::clone(&first.pdf);
    let pages = active_pdf.pages();
    let cache = RenderCache::new();
    let interp = InterpreterSettings::default();
    let render_settings = RenderSettings::default();

    let mut current = first;
    loop {
        let HayroRenderRequest {
            page_index,
            max_width,
            max_height,
            reply,
            ..
        } = current;

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_page_from_pages(
                pages,
                &cache,
                &interp,
                &render_settings,
                page_index,
                max_width,
                max_height,
            )
        }));

        let panicked = outcome.is_err();
        let result = outcome.unwrap_or_else(|_| {
            Err(DocumentError::Message(
                "PDF renderer panicked while rendering page".to_string(),
            ))
        });
        let _ = reply.send(result);

        if panicked {
            // Drop the active RenderCache cleanly instead of reusing potentially inconsistent
            // interior-mutable state after a panic.
            return None;
        }

        match rx.recv() {
            Ok(next) if next.key == active_key => {
                current = next;
            }
            Ok(next) => return Some(next),
            Err(_) => return None,
        }
    }
}

fn render_page_from_pages<'a>(
    pages: &'a Pages<'a>,
    cache: &RenderCache<'a>,
    interp: &InterpreterSettings,
    render_settings: &RenderSettings,
    page_index: usize,
    max_width: u32,
    max_height: u32,
) -> Result<(DecodedImage, usize), DocumentError> {
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

    let max_width = max_width.max(1);
    let max_height = max_height.max(1);
    let raw_scale = (max_width as f32 / page_w).min(max_height as f32 / page_h);
    // Keep dimensions within u16 bounds, cap scaling at MAX_PAGE_SCALE, and avoid sub-pixel collapses.
    let max_safe_scale = (6000.0 / page_w.max(page_h)).clamp(0.05, MAX_PAGE_SCALE);
    let scale = raw_scale.clamp(0.05, max_safe_scale.max(0.05));

    let pixmap_settings = PixmapSettings {
        x_scale: scale,
        y_scale: scale,
        bg_color: WHITE,
    };

    let pixmap = hayro::render(page, cache, interp, render_settings, &pixmap_settings);
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

fn render_via_hayro_worker(
    key: PdfDocKey,
    pdf: Arc<Pdf>,
    page_index: usize,
    max_width: u32,
    max_height: u32,
) -> Result<(DecodedImage, usize), DocumentError> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let sender = hayro_worker_sender()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    sender
        .send(HayroRenderRequest {
            key,
            pdf,
            page_index,
            max_width: max_width.max(1),
            max_height: max_height.max(1),
            reply: reply_tx,
        })
        .map_err(|_| DocumentError::Message("PDF render worker disconnected".to_string()))?;

    reply_rx
        .recv()
        .map_err(|_| DocumentError::Message("PDF render worker failed to reply".to_string()))?
}

fn ensure_pdf_within_size_limit(path: &Path) -> Result<(), DocumentError> {
    let file_len = std::fs::metadata(path)?.len();
    if file_len > MAX_PDF_FILE_BYTES {
        return Err(DocumentError::Message(format!(
            "File exceeds size limit ({file_len} > {MAX_PDF_FILE_BYTES} bytes)"
        )));
    }
    Ok(())
}

/// Renders page 0 of a PDF as a thumbnail. Encrypted or page-less PDFs cleanly return `Ok(None)`.
pub fn render_pdf_thumbnail(
    path: &Path,
    max_width: u32,
    max_height: u32,
) -> Result<Option<DecodedImage>, DocumentError> {
    let max_width = max_width.max(1);
    let max_height = max_height.max(1);
    ensure_pdf_within_size_limit(path)?;

    if poppler_pipe::is_available() {
        match poppler_pipe::render_pdf_page(path, 0, max_width, max_height) {
            PopplerOutcome::Ok((img, _)) => return Ok(Some(img)),
            PopplerOutcome::Encrypted => return Ok(None),
            PopplerOutcome::Err(err) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %err,
                    "Poppler thumbnail failed; falling back to hayro"
                );
            }
        }
    }

    let (key, loaded) = load_pdf_file_cached(path)?;
    let PdfLoadOutcome::Loaded(pdf) = loaded else {
        return Ok(None);
    };
    if pdf.pages().is_empty() {
        return Ok(None);
    }
    let (img, _) = render_via_hayro_worker(key, pdf, 0, max_width, max_height)?;
    Ok(Some(img))
}

/// Renders `page_index` of `path` at high resolution for the preview pane.
pub fn load_pdf_preview(path: &Path, page_index: usize) -> Result<DocumentPreview, DocumentError> {
    let file_len = std::fs::metadata(path)?.len();
    if file_len > MAX_PDF_FILE_BYTES {
        return Ok(DocumentPreview::Unavailable {
            reason: DocumentUnavailableReason::TooLarge,
        });
    }

    if poppler_pipe::is_available() {
        match poppler_pipe::render_pdf_page(
            path,
            page_index,
            PREVIEW_TARGET_WIDTH,
            PREVIEW_TARGET_HEIGHT,
        ) {
            PopplerOutcome::Ok((image, total_pages)) => {
                let clamped_page = page_index.min(total_pages.saturating_sub(1));
                return Ok(DocumentPreview::PagedImage {
                    image,
                    page_index: clamped_page,
                    total_pages,
                });
            }
            PopplerOutcome::Encrypted => {
                return Ok(DocumentPreview::Unavailable {
                    reason: DocumentUnavailableReason::Encrypted,
                });
            }
            PopplerOutcome::Err(err) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %err,
                    "Poppler preview failed; falling back to hayro"
                );
            }
        }
    }

    let (key, loaded) = load_pdf_file_cached(path)?;
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
    let (image, total_pages) = render_via_hayro_worker(
        key,
        pdf,
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
}

/// Renders `page_index` from an in-memory PDF buffer.
#[allow(dead_code)]
pub(crate) fn render_pdf_bytes_page(
    bytes: Vec<u8>,
    page_index: usize,
    max_width: u32,
    max_height: u32,
) -> Result<(DecodedImage, usize), DocumentError> {
    let (key, pdf) = load_pdf_bytes_cached(bytes)?;
    if pdf.pages().is_empty() {
        return Err(DocumentError::Message("PDF has no pages".to_string()));
    }
    render_via_hayro_worker(key, pdf, page_index, max_width, max_height)
}

/// Extracts PDF document metadata fields for the metadata inspector.
pub fn extract_pdf_metadata(path: &Path) -> Result<BTreeMap<String, String>, DocumentError> {
    ensure_pdf_within_size_limit(path)?;

    if poppler_pipe::is_available() {
        match poppler_pipe::query_pdf_info(path) {
            PopplerOutcome::Ok(info) => return Ok(info.fields),
            PopplerOutcome::Encrypted => {
                let mut fields = BTreeMap::new();
                fields.insert("Encrypted".into(), "Yes".into());
                return Ok(fields);
            }
            PopplerOutcome::Err(err) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %err,
                    "Poppler pdfinfo failed; falling back to hayro"
                );
            }
        }
    }

    let (_, loaded) = load_pdf_file_cached(path)?;
    let mut fields = BTreeMap::new();
    let PdfLoadOutcome::Loaded(pdf) = loaded else {
        fields.insert("Encrypted".into(), "Yes".into());
        return Ok(fields);
    };

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
    }));

    match outcome {
        Ok(()) => Ok(fields),
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
