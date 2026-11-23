use std::collections::HashSet;
use std::fmt;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use lru::LruCache;

use super::document::DocumentPreviewData;
use super::media_errors::MediaErrorTracker;
use crate::subscriptions::thumbnail_tracker::ThumbnailVisibilityTracker;

pub struct CacheState {
    pub selected_image: Option<(PathBuf, iced::widget::image::Handle)>,
    pub thumbnail_cache: LruCache<PathBuf, iced::widget::image::Handle>,
    pub thumbnails_in_flight: HashSet<PathBuf>,
    pub image_cache: LruCache<PathBuf, iced::widget::image::Handle>,
    pub document_cache: LruCache<(PathBuf, usize), DocumentPreviewData>,
    pub thumbnail_tracker: ThumbnailVisibilityTracker,
    pub media_errors: MediaErrorTracker,
}

impl fmt::Debug for CacheState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CacheState")
            .field(
                "selected_image",
                &self.selected_image.as_ref().map(|(p, _)| p),
            )
            .field("thumbnail_cache_len", &self.thumbnail_cache.len())
            .field("thumbnails_in_flight_len", &self.thumbnails_in_flight.len())
            .field("image_cache_len", &self.image_cache.len())
            .field("document_cache_len", &self.document_cache.len())
            .field("media_errors_len", &self.media_errors.len())
            .finish()
    }
}

impl CacheState {
    pub fn new() -> Self {
        Self {
            selected_image: None,
            thumbnail_cache: LruCache::new(
                NonZeroUsize::new(200).expect("200 is a non-zero constant"),
            ),
            thumbnails_in_flight: HashSet::new(),
            image_cache: LruCache::new(NonZeroUsize::new(20).expect("20 is a non-zero constant")),
            document_cache: LruCache::new(
                NonZeroUsize::new(24).expect("24 is a non-zero constant"),
            ),
            thumbnail_tracker: ThumbnailVisibilityTracker::new(std::time::Duration::from_millis(
                150,
            )),
            media_errors: MediaErrorTracker::new(),
        }
    }
}

impl Default for CacheState {
    fn default() -> Self {
        Self::new()
    }
}
