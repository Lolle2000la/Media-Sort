use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use media_sort_backend::filesystem::scanner::scan_media_files;
use media_sort_backend::filesystem::trash::delete_to_trash;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        Self::new_in(&std::env::temp_dir(), prefix)
    }

    fn new_in(base: &Path, prefix: &str) -> Self {
        let pid = std::process::id();
        let count = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = base.join(format!("{}_{}_{}", prefix, pid, count));
        fs::create_dir_all(&dir).unwrap();
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

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn copy_fixture(fixture_name: &str, dest_dir: &Path) -> PathBuf {
    let src = fixtures_dir().join(fixture_name);
    let dest = dest_dir.join(fixture_name);
    fs::copy(&src, &dest).expect("failed to copy fixture");
    dest
}

#[test]
fn test_scan_empty_dir() {
    let tmp = TempDir::new("scan_empty");
    let results: Vec<_> = scan_media_files(tmp.path()).into_iter().collect();
    assert!(
        results.is_empty(),
        "expected empty vec for dir with no media files"
    );
}

#[test]
fn test_scan_images_only() {
    let tmp = TempDir::new("scan_images");
    copy_fixture("test_image.jpg", tmp.path());
    copy_fixture("test_image.png", tmp.path());

    let results: Vec<_> = scan_media_files(tmp.path()).into_iter().collect();
    assert_eq!(results.len(), 2, "expected 2 image files");
    let names: Vec<&str> = results
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    assert!(names.contains(&"test_image.jpg"));
    assert!(names.contains(&"test_image.png"));
}

#[test]
fn test_scan_filtered_by_extension() {
    let tmp = TempDir::new("scan_filtered");
    copy_fixture("test_image.jpg", tmp.path());
    fs::write(tmp.join("notes.bin"), b"not media").unwrap();

    let results: Vec<_> = scan_media_files(tmp.path()).into_iter().collect();
    assert_eq!(results.len(), 1, "expected only the jpg, not the bin");
    let name = results[0].file_name().unwrap().to_str().unwrap();
    assert_eq!(name, "test_image.jpg");
}

#[test]
fn test_scan_no_recursion() {
    let tmp = TempDir::new("scan_norecurse");
    copy_fixture("test_image.jpg", tmp.path());
    let subdir = tmp.join("subdir");
    fs::create_dir_all(&subdir).unwrap();
    copy_fixture("test_image.png", &subdir);

    let results: Vec<_> = scan_media_files(tmp.path()).into_iter().collect();
    assert_eq!(
        results.len(),
        1,
        "expected only top-level jpg, not the one in subdir"
    );
    let name = results[0].file_name().unwrap().to_str().unwrap();
    assert_eq!(name, "test_image.jpg");
}

#[test]
fn test_scan_with_fixtures() {
    let tmp = TempDir::new("scan_fixtures");
    copy_fixture("test_image.jpg", tmp.path());
    copy_fixture("test_image.png", tmp.path());
    copy_fixture("test_image.gif", tmp.path());
    copy_fixture("test_audio.mp3", tmp.path());
    copy_fixture("test_audio.flac", tmp.path());

    let results: Vec<_> = scan_media_files(tmp.path()).into_iter().collect();
    assert_eq!(results.len(), 5, "expected all 5 fixture files to be found");
}

// ============================================================
// Trash tests
// ============================================================

#[test]
fn test_delete_to_trash_no_filename() {
    let result = delete_to_trash(std::path::Path::new("/"));
    assert!(result.is_err(), "should fail for path with no file name");
}

// ============================================================
// Scanner channel disconnect tests
// ============================================================

#[test]
fn test_scanner_channel_disconnect() {
    let rx = scan_media_files(Path::new("/tmp"));
    drop(rx);
}

#[test]
fn test_scanner_nonexistent_directory() {
    let nonexistent = Path::new("/nonexistent/scanner_test_dir_12345");
    let results: Vec<_> = scan_media_files(nonexistent).into_iter().collect();
    assert!(results.is_empty());
}

// ============================================================
// Additional trash test
// ============================================================

#[test]
fn test_delete_to_trash_nonexistent_file() {
    let result = delete_to_trash(Path::new("/nonexistent/trash_test_12345.txt"));
    assert!(result.is_err());
}

#[test]
fn test_delete_to_trash_and_restore_roundtrip() {
    let tmp = TempDir::new("trash_roundtrip");
    let file = tmp.path.join("roundtrip.txt");
    fs::write(&file, b"trash roundtrip").unwrap();

    let mut handle = delete_to_trash(&file).expect("delete_to_trash failed");
    assert!(!file.exists(), "file should be in trash after delete");

    handle.restore().expect("restore failed");
    assert!(file.exists(), "file should be back after restore");
    assert_eq!(fs::read_to_string(&file).unwrap(), "trash roundtrip");
}

/// Windows regression test: the shell's trash display name
/// (SIGDN_PARENTRELATIVE) drops the extension for known file types
/// (Explorer's default "hide extensions" setting), which used to break
/// both finding the item and restoring it under its real name.
#[cfg(target_os = "windows")]
#[test]
fn test_windows_trash_restore_preserves_extension() {
    let tmp = TempDir::new("trash_ext");
    // .jpg is a "known file type" subject to Explorer's extension hiding.
    let file = tmp.path.join("photo.jpg");
    fs::write(&file, b"jpeg data").unwrap();

    let mut handle = delete_to_trash(&file).expect("delete_to_trash failed");
    assert!(!file.exists());

    handle.restore().expect("restore failed");

    assert!(
        file.exists(),
        "file must be restored WITH its .jpg extension"
    );
    assert!(
        !tmp.path.join("photo").exists(),
        "file must NOT be restored as an extension-less sibling"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "jpeg data");
}

/// Windows: when several items with the same name/extension from the same
/// folder sit in the recycle bin, restore must pick OUR (most recently
/// deleted) item, not an older same-name one.
#[cfg(target_os = "windows")]
#[test]
fn test_windows_trash_restore_picks_most_recent() {
    let tmp = TempDir::new("trash_recent");
    let file = tmp.path.join("same.txt");

    fs::write(&file, b"first version").unwrap();
    let mut handle1 = delete_to_trash(&file).expect("first delete failed");

    // time_deleted has second granularity; force the two deletes into
    // different seconds for a deterministic tiebreak.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    fs::write(&file, b"second version").unwrap();
    let mut handle2 = delete_to_trash(&file).expect("second delete failed");

    // Restore OLDEST first: the trash crate's restore refuses to overwrite
    // the existing path (RestoreCollision), and undo semantics dictate the
    // newest entry is undone first anyway.
    handle1.restore().expect("first restore failed");
    assert!(file.exists());
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "first version",
        "restore must pick the oldest same-name item first"
    );

    // The file now collides with the newer trash entry; move it away so
    // the newer entry can be restored.
    let moved = tmp.path.join("moved_away.txt");
    fs::rename(&file, &moved).unwrap();

    handle2.restore().expect("second restore failed");
    assert!(file.exists());
    assert_eq!(fs::read_to_string(&file).unwrap(), "second version");
}

mod watcher_tests {
    use super::*;
    use media_sort_backend::filesystem::watcher::{FileSystemEvent, watch_directories};
    use std::time::{Duration, Instant};

    /// Polls `rx` until the deadline, returning everything collected.
    /// Polling (not `blocking_recv`) — the latter blocks indefinitely on
    /// an empty channel and would hang the test. Each channel message is a
    /// debounced batch, so the batches are flattened.
    fn drain_until(
        rx: &mut tokio::sync::mpsc::Receiver<Vec<FileSystemEvent>>,
        deadline: Instant,
    ) -> Vec<FileSystemEvent> {
        let mut collected = Vec::new();
        loop {
            if Instant::now() >= deadline {
                break;
            }
            match rx.try_recv() {
                Ok(batch) => collected.extend(batch),
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        collected
    }

    #[test]
    fn test_watch_reports_add_remove_and_rename() {
        let tmp = TempDir::new("mediasort_watch");

        #[cfg(target_os = "macos")]
        {
            // The macOS backend is kqueue (workspace `macos_kqueue`
            // feature), which detects new children by scanning the
            // directory on a NOTE_WRITE and reporting only the FIRST
            // entry not yet in its watch map — anything else in the same
            // burst is silently absorbed. The rename below is therefore
            // the only mutation after the watch starts, making
            // `renamed.jpg` the only unknown entry the scan can report.
            // The create path is covered by
            // `test_watch_multiple_directories`, and remove/rename
            // classification by the `classify_event` unit tests.
            let old = tmp.path().join("old.jpg");
            fs::write(&old, b"data").unwrap();

            let (_handle, mut rx) = watch_directories(&[tmp.path().to_path_buf()]);
            std::thread::sleep(Duration::from_millis(200));

            fs::rename(&old, tmp.path().join("renamed.jpg")).unwrap();

            let deadline = Instant::now() + Duration::from_secs(30);
            let mut rename_seen = false;
            while Instant::now() < deadline && !rename_seen {
                for event in drain_until(&mut rx, Instant::now() + Duration::from_millis(500)) {
                    match &event {
                        FileSystemEvent::Renamed(from, to)
                            if from.file_name().is_some_and(|n| n == "old.jpg")
                                && to.file_name().is_some_and(|n| n == "renamed.jpg") =>
                        {
                            rename_seen = true;
                        }
                        FileSystemEvent::Added(p)
                            if p.file_name().is_some_and(|n| n == "renamed.jpg") =>
                        {
                            // The kqueue directory scan reports the renamed
                            // file as a new (unknown) entry; its existence
                            // classifies it as Added.
                            rename_seen = true;
                        }
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(
                rename_seen,
                "expected a Renamed or Added event for renamed.jpg"
            );

            drop(_handle);
        }

        #[cfg(not(target_os = "macos"))]
        {
            let (_handle, mut rx) = watch_directories(&[tmp.path().to_path_buf()]);
            // Give the watcher a moment to register (debouncer thread + OS).
            std::thread::sleep(Duration::from_millis(200));

            fs::write(tmp.path().join("new.jpg"), b"data").unwrap();
            let added_deadline = Instant::now() + Duration::from_secs(10);
            let mut added_seen = false;
            while Instant::now() < added_deadline && !added_seen {
                for event in drain_until(&mut rx, Instant::now() + Duration::from_millis(500)) {
                    if matches!(&event, FileSystemEvent::Added(p) if p.file_name().is_some_and(|n| n == "new.jpg"))
                    {
                        added_seen = true;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(added_seen, "expected an Added event for new.jpg");

            fs::remove_file(tmp.path().join("new.jpg")).unwrap();
            let removed_deadline = Instant::now() + Duration::from_secs(10);
            let mut removed_seen = false;
            while Instant::now() < removed_deadline && !removed_seen {
                for event in drain_until(&mut rx, Instant::now() + Duration::from_millis(500)) {
                    if matches!(&event, FileSystemEvent::Removed(p) if p.file_name().is_some_and(|n| n == "new.jpg"))
                    {
                        removed_seen = true;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(removed_seen, "expected a Removed event for new.jpg");

            // Rename: some backends pair both paths on one event, others
            // emit separate remove/create — accept either representation.
            fs::write(tmp.path().join("old.jpg"), b"data").unwrap();
            std::thread::sleep(Duration::from_millis(300));
            let _ = drain_until(&mut rx, Instant::now() + Duration::from_millis(200));
            fs::rename(tmp.path().join("old.jpg"), tmp.path().join("renamed.jpg")).unwrap();
            let rename_deadline = Instant::now() + Duration::from_secs(10);
            let mut rename_seen = false;
            while Instant::now() < rename_deadline && !rename_seen {
                for event in drain_until(&mut rx, Instant::now() + Duration::from_millis(500)) {
                    match &event {
                        FileSystemEvent::Renamed(from, to)
                            if from.file_name().is_some_and(|n| n == "old.jpg")
                                && to.file_name().is_some_and(|n| n == "renamed.jpg") =>
                        {
                            rename_seen = true;
                        }
                        FileSystemEvent::Added(p)
                            if p.file_name().is_some_and(|n| n == "renamed.jpg") =>
                        {
                            // Windows delivers the two rename sides as
                            // separate single-path events; the destination
                            // side classifies as Added.
                            rename_seen = true;
                        }
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(
                rename_seen,
                "expected a Renamed or Added event for renamed.jpg"
            );

            drop(_handle);
        }
    }

    #[test]
    fn test_watch_multiple_directories() {
        let base = TempDir::new("mediasort_watch_multi");
        let a = base.path().join("a");
        let b = base.path().join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();

        let (_handle, mut rx) = watch_directories(&[a.clone(), b.clone()]);
        std::thread::sleep(Duration::from_millis(200));

        fs::write(a.join("in_a.jpg"), b"data").unwrap();
        fs::write(b.join("in_b.jpg"), b"data").unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut seen_a = false;
        let mut seen_b = false;
        while Instant::now() < deadline && !(seen_a && seen_b) {
            for event in drain_until(&mut rx, Instant::now() + Duration::from_millis(500)) {
                if let FileSystemEvent::Added(p) = event {
                    seen_a |= p.file_name().is_some_and(|n| n == "in_a.jpg");
                    seen_b |= p.file_name().is_some_and(|n| n == "in_b.jpg");
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(seen_a, "expected an Added event for in_a.jpg");
        assert!(seen_b, "expected an Added event for in_b.jpg");

        drop(_handle);
    }
}
