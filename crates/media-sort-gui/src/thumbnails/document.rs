use std::path::Path;
use std::sync::mpsc::SyncSender;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use super::pool::{
    KeepPredicate, ThumbnailRequest, ThumbnailResult, await_response, spawn_worker_pool,
    submit_request,
};
use super::{THUMBNAIL_SIZE, ThumbnailError};

const DOCUMENT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

pub type DocumentThumbnailResult = ThumbnailResult;

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
                media_sort_backend::media::document::generate_document_thumbnail_with_options(
                    path,
                    THUMBNAIL_SIZE,
                    THUMBNAIL_SIZE,
                    false,
                )
                .map(|opt| opt.map(|d| d.into_parts()))
                .map_err(|e| ThumbnailError::Decode(format!("Document thumbnail error: {e}")))
            },
        )
    });

/// Generate a thumbnail for a document through the bounded document worker pool.
pub fn generate_document_thumbnail(
    path: &Path,
    should_keep: Option<KeepPredicate>,
) -> DocumentThumbnailResult {
    if should_keep.as_ref().is_some_and(|keep| !keep(path)) {
        return Ok(None);
    }
    let rx = submit_request(&DOCUMENT_THUMBNAIL_WORKER, path, should_keep, "document")?;
    await_response(rx, DOCUMENT_RESPONSE_TIMEOUT, "document")
}
