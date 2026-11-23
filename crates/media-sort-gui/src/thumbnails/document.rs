use std::path::Path;
use std::sync::mpsc::SyncSender;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use media_sort_core::media_type::DocumentKind;

use super::pool::{
    KeepPredicate, ThumbnailRequest, ThumbnailResult, await_response, spawn_worker_pool,
    submit_request,
};
use super::{THUMBNAIL_SIZE, ThumbnailError};

const DOCUMENT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const OFFICE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(45);

pub type DocumentThumbnailResult = ThumbnailResult;

// Bounded document thumbnail worker pool — handles fast document formats (PDF,
// comic archives, EPUB covers, Markdown/plain text, and embedded/OS Office
// thumbnails) without ever invoking LibreOffice.
static DOCUMENT_THUMBNAIL_WORKER: LazyLock<Mutex<SyncSender<ThumbnailRequest>>> =
    LazyLock::new(|| {
        let num_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 4);

        spawn_worker_pool(
            "Document thumbnail",
            num_workers,
            |_| Some(()),
            |_, path| {
                media_sort_backend::media::document::extract_fast_document_thumbnail(
                    path,
                    THUMBNAIL_SIZE,
                    THUMBNAIL_SIZE,
                )
                .map(|opt| opt.map(|d| d.into_parts()))
                .map_err(|e| ThumbnailError::Decode(format!("Document thumbnail error: {e}")))
            },
        )
    });

// Dedicated Office/spreadsheet thumbnail worker pool (1 thread, since
// LibreOffice execution serializes anyway) so slow LibreOffice conversions
// cannot starve fast thumbnails on `DOCUMENT_THUMBNAIL_WORKER`.
static OFFICE_THUMBNAIL_WORKER: LazyLock<Mutex<SyncSender<ThumbnailRequest>>> =
    LazyLock::new(|| {
        spawn_worker_pool(
            "Office thumbnail",
            1,
            |_| Some(()),
            |_, path| {
                media_sort_backend::media::document::render_libreoffice_document_thumbnail(
                    path,
                    THUMBNAIL_SIZE,
                    THUMBNAIL_SIZE,
                )
                .map(|opt| opt.map(|d| d.into_parts()))
                .map_err(|e| ThumbnailError::Decode(format!("Office thumbnail error: {e}")))
            },
        )
    });

/// Generate a thumbnail for a document through the bounded worker pools.
///
/// Always tries [`DOCUMENT_THUMBNAIL_WORKER`] first (PDF, comic, EPUB, and
/// embedded/OS Office thumbnails without LibreOffice). Only if the fast path
/// returns `Ok(None)` for an Office/spreadsheet document with
/// `enable_libreoffice == true` is a conversion dispatched to
/// [`OFFICE_THUMBNAIL_WORKER`].
pub fn generate_document_thumbnail(
    path: &Path,
    enable_libreoffice: bool,
    should_keep: Option<KeepPredicate>,
) -> DocumentThumbnailResult {
    if should_keep.as_ref().is_some_and(|keep| !keep(path)) {
        return Ok(None);
    }

    match await_response(
        submit_request(
            &DOCUMENT_THUMBNAIL_WORKER,
            path,
            should_keep.clone(),
            "document",
        )?,
        DOCUMENT_RESPONSE_TIMEOUT,
        "document",
    ) {
        Ok(Some(parts)) => return Ok(Some(parts)),
        Err(e) => return Err(e),
        Ok(None) => {}
    }

    let is_office = matches!(
        DocumentKind::from_path(path),
        Some(DocumentKind::Office | DocumentKind::Spreadsheet)
    );
    if !is_office || !enable_libreoffice {
        return Ok(None);
    }
    if should_keep.as_ref().is_some_and(|keep| !keep(path)) {
        return Ok(None);
    }

    let rx = submit_request(&OFFICE_THUMBNAIL_WORKER, path, should_keep, "office")?;
    await_response(rx, OFFICE_RESPONSE_TIMEOUT, "office")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_office_thumbnail_worker_skips_when_cancelled_in_queue() {
        // Calls 0..=2 (pre-queue, inside DOCUMENT_THUMBNAIL_WORKER, and pre-office-queue)
        // return true so the job finishes the fast path (`Ok(None)`) and enters
        // `OFFICE_THUMBNAIL_WORKER`; call 3 (inside `OFFICE_THUMBNAIL_WORKER` after recv)
        // returns false so LibreOffice is skipped.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let cancel_in_worker: KeepPredicate =
            Arc::new(move |_| calls_clone.fetch_add(1, Ordering::SeqCst) < 3);

        let path = Path::new("/nonexistent/slides.pptx");
        let result = generate_document_thumbnail(path, true, Some(cancel_in_worker));
        assert!(matches!(result, Ok(None)));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }
}
