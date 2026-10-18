//! Sending files to the *host* trash under Flatpak.
//!
//! Inside a Flatpak sandbox `$XDG_DATA_HOME` is remapped to a per-app
//! directory, so the `trash` crate's "home trash" (`$XDG_DATA_HOME/Trash`) ends
//! up in the sandbox's private data directory: the file never appears in the
//! user's trash, cannot be emptied from a file manager, and only survives until
//! the app is uninstalled with `--delete-data`. Opening a file read/write is
//! what the portal uses to authorise the request, so it is also what bounds
//! this path: for anything that cannot be opened read/write (a read-only mount,
//! for instance) the caller falls back to trashing inside the sandbox.
//!
//! The portal is not always usable: it refuses files on system-internal mounts
//! (`/tmp`, `/run`, `/dev`) by design, and the fd-based rewrite of the Trash
//! portal (the CVE-2026-40354 fix) broke it for Flatpak callers on some
//! versions — upstream issue #1972, fixed in #1982 / 1.21.2, backported to
//! distributions at different speeds. The sandbox can write the real home trash
//! itself, because the manifest grants `--filesystem=home`, so
//! [`trash_into_home_trash`] takes over in those cases. Without it the `trash`
//! crate would target `$HOME/.Trash-1000`: a volume trash derived from the
//! sandbox's own bind mount of the home directory, which no file manager shows
//! and which only this app can restore from.
//!
//! The portal only *deletes*, so undoing a delete means finding the entry in
//! the host trash again. That lookup follows the FreeDesktop trash
//! specification: <https://specifications.freedesktop.org/trash-spec/trashspec-1.0.html>.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use chrono::Local;
use media_sort_core::actions::delete_action::TrashRestoreHandle;
use media_sort_core::actions::reversible::ActionError;

/// Whether this process runs inside a Flatpak sandbox, where the host trash is
/// only reachable through the portal. See
/// [`crate::platform::desktop_portal::in_flatpak`], which the OpenURI actions
/// share.
pub use crate::platform::desktop_portal::in_flatpak;

/// How many `name.N` alternatives to try before giving up on a free trash name.
const MAX_TRASH_NAME_ATTEMPTS: u32 = 10_000;

/// Ask the desktop to move `original_path` to the user's trash
/// (`org.freedesktop.portal.Trash.TrashFile`).
pub fn delete_to_trash(original_path: &Path) -> Result<Box<dyn TrashRestoreHandle>, ActionError> {
    // `xdg-desktop-portal` 1.20+ strictly requires mode 0700 on `Trash`,
    // `Trash/files`, and `Trash/info` (`ensure_dir_at`), failing with
    // "Directory already exists with bad permissions" if any of them was
    // created with 0755 (for instance by the `trash` crate's `create_dir_all`).
    // Repair any existing candidate trash directories before calling the portal.
    repair_existing_trash_permissions(original_path);

    // TrashFile takes a file descriptor, and the portal only trashes what the
    // caller can open read/write.
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(original_path)
        .map_err(ActionError::Io)?;

    // ashpd is async-only and the delete path is synchronous; a portal round
    // trip takes a few milliseconds, so drive it to completion here.
    pollster::block_on(ashpd::desktop::trash::trash_file(&file.as_fd())).map_err(to_io)?;

    Ok(Box::new(PortalTrashRestore {
        original_path: original_path.to_path_buf(),
        flushed: false,
    }))
}

fn to_io(err: impl std::error::Error + Send + Sync + 'static) -> ActionError {
    ActionError::Io(io::Error::other(err))
}

/// Tighten permissions on any existing trash directories for `original_path`
/// (plus the `$XDG_DATA_HOME/Trash` directory `trash::delete` uses on native
/// runs) to `0700` so `xdg-desktop-portal` never rejects them for bad mode bits.
pub fn repair_existing_trash_permissions(original_path: &Path) {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates = trash_directories(home.as_deref(), original_path);
    if let Some(xdg_data) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        candidates.push(xdg_data.join("Trash"));
    }
    for base in candidates {
        for dir in [base.as_path(), &base.join("files"), &base.join("info")] {
            if let Ok(meta) = fs::symlink_metadata(dir)
                && meta.is_dir()
                && (meta.mode() & 0o7777) != 0o700
            {
                let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
            }
        }
    }
}

/// Restore handle for a file the host trashed on our behalf. The portal does
/// not report where the entry landed, so `restore` searches the host trash.
struct PortalTrashRestore {
    original_path: PathBuf,
    flushed: bool,
}

impl TrashRestoreHandle for PortalTrashRestore {
    fn restore(&mut self) -> Result<(), ActionError> {
        if self.flushed {
            return Err(ActionError::RestorationFailed("already flushed".into()));
        }

        restore_from_host_trash(&self.original_path)?;

        self.flushed = true;
        Ok(())
    }

    fn flush_to_native_trash(&mut self) -> Result<(), ActionError> {
        self.flushed = true;
        Ok(())
    }
}

impl Drop for PortalTrashRestore {
    fn drop(&mut self) {
        if !self.flushed {
            let _ = self.flush_to_native_trash();
        }
    }
}

/// Move a trashed entry back to its original path and drop its metadata.
fn restore_from_host_trash(original: &Path) -> Result<(), ActionError> {
    let Some((trashed_path, info_path)) = find_entry(original) else {
        return Err(ActionError::RestorationFailed(
            "the file is not in the host trash any more (it may have been emptied, or the host stores its trash elsewhere)"
                .into(),
        ));
    };
    if !trashed_path.exists() {
        return Err(ActionError::RestorationFailed(
            "the file is not in the trash any more (it may have been emptied)".into(),
        ));
    }
    if original.exists() {
        return Err(ActionError::RestorationFailed(
            "a file already exists at the original path".into(),
        ));
    }

    if let Some(parent) = original.parent() {
        fs::create_dir_all(parent).map_err(ActionError::Io)?;
    }
    move_file(&trashed_path, original).map_err(ActionError::Io)?;
    // Removing the info file is what makes the entry disappear for the file
    // manager; a leftover would only be cosmetic, so its failure is not fatal.
    let _ = fs::remove_file(&info_path);

    Ok(())
}

/// Move `original_path` into the real home trash (`$HOME/.local/share/Trash`)
/// — or, when `original_path` lives on another mount, into that volume's
/// `.Trash-<uid>` directory with specification-compliant `0700` permissions
/// and relative `Path=` metadata — without the portal.
///
/// `$XDG_DATA_HOME` is deliberately ignored: inside the sandbox it points at
/// the app's private data directory, which is the very problem this works
/// around.
pub fn trash_into_home_trash(
    original_path: &Path,
) -> Result<Box<dyn TrashRestoreHandle>, ActionError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| ActionError::Io(io::Error::other("HOME is not set")))?;

    let home_trash = home.join(".local/share/Trash");
    match trash_into_home_trash_at(&home_trash, original_path) {
        Ok(handle) => Ok(handle),
        Err(home_err) => trash_into_volume_trash(&home, original_path).or(Err(home_err)),
    }
}

/// The home-trash layout below `base`: `files/` holds the moved files, `info/`
/// their `.trashinfo` metadata, both 0700 as the specification requires.
fn trash_into_home_trash_at(
    base: &Path,
    original_path: &Path,
) -> Result<Box<dyn TrashRestoreHandle>, ActionError> {
    ensure_trash_directory(base)?;

    // A file from another filesystem cannot be renamed into the home trash;
    // that case belongs to the volume's own trash.
    let file_device = fs::metadata(original_path).map_err(ActionError::Io)?.dev();
    if fs::metadata(base).map_err(ActionError::Io)?.dev() != file_device {
        return Err(ActionError::Io(io::Error::other(
            "the file is not on the home filesystem",
        )));
    }

    trash_into_directory_with_recorded_path(base, original_path, original_path)
}

/// Move `original_path` into its mount point's `.Trash-<uid>` directory,
/// recording `Path=` relative to the mount root as required by the FreeDesktop
/// Trash specification.
fn trash_into_volume_trash(
    home: &Path,
    original_path: &Path,
) -> Result<Box<dyn TrashRestoreHandle>, ActionError> {
    let uid = user_id_of(home)
        .ok_or_else(|| ActionError::Io(io::Error::other("could not determine user id")))?;
    let topdir = mount_root_of(original_path)?;
    // Never treat `$HOME` itself as a volume-trash topdir (inside Flatpak
    // `$HOME` is a bind mount, and `$HOME/.Trash-<uid>` is ignored by desktop
    // file managers).
    if topdir == home {
        return Err(ActionError::Io(io::Error::other(
            "refusing to create volume trash directly under HOME",
        )));
    }
    trash_into_volume_trash_at(&topdir, uid, original_path)
}

fn trash_into_volume_trash_at(
    topdir: &Path,
    uid: u32,
    original_path: &Path,
) -> Result<Box<dyn TrashRestoreHandle>, ActionError> {
    let relative = original_path
        .strip_prefix(topdir)
        .map_err(|_| ActionError::Io(io::Error::other("file is not inside the volume root")))?;
    let base = topdir.join(format!(".Trash-{uid}"));
    ensure_trash_directory(&base)?;
    trash_into_directory_with_recorded_path(&base, original_path, relative)
}

fn trash_into_directory_with_recorded_path(
    base: &Path,
    original_path: &Path,
    recorded_path: &Path,
) -> Result<Box<dyn TrashRestoreHandle>, ActionError> {
    let file_name = original_path
        .file_name()
        .ok_or_else(|| ActionError::Io(io::Error::other("the path has no file name")))?;

    let mut info = String::from("[Trash Info]\nPath=");
    info.push_str(&percent_encode(recorded_path.as_os_str().as_bytes()));
    info.push_str("\nDeletionDate=");
    info.push_str(&Local::now().format("%Y-%m-%dT%H:%M:%S").to_string());
    info.push('\n');

    let (name, info_path) = create_trash_info(&base.join("info"), file_name, &info)?;

    let trashed_path = base.join("files").join(&name);
    if let Err(err) = fs::rename(original_path, &trashed_path) {
        // Do not leave metadata behind for a file that never arrived: a
        // nameless entry would show up in the file manager.
        let _ = fs::remove_file(&info_path);
        return Err(ActionError::Io(err));
    }

    Ok(Box::new(HomeTrashRestore {
        original_path: original_path.to_path_buf(),
        trashed_path,
        info_path,
        flushed: false,
    }))
}

/// Find the highest ancestor of `path` that sits on the same filesystem device
/// (`st_dev`) as `path`.
fn mount_root_of(path: &Path) -> Result<PathBuf, ActionError> {
    let parent = path
        .parent()
        .ok_or_else(|| ActionError::Io(io::Error::other("path has no parent directory")))?;
    let dev = fs::metadata(parent).map_err(ActionError::Io)?.dev();
    let mut topdir = parent.to_path_buf();
    for ancestor in parent.ancestors().skip(1) {
        match fs::metadata(ancestor) {
            Ok(meta) if meta.dev() == dev => topdir = ancestor.to_path_buf(),
            _ => break,
        }
    }
    Ok(topdir)
}

fn ensure_trash_directory(base: &Path) -> Result<(), ActionError> {
    for directory in [base.to_path_buf(), base.join("files"), base.join("info")] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .map_err(ActionError::Io)?;
        // `DirBuilder::create` does not tighten the mode of a directory that
        // already exists (e.g. one previously created as 0755 by the `trash`
        // crate), so enforce 0700 explicitly.
        if let Ok(meta) = fs::symlink_metadata(&directory)
            && meta.is_dir()
            && (meta.mode() & 0o7777) != 0o700
        {
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(ActionError::Io)?;
        }
    }

    Ok(())
}

fn move_file(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::CrossesDevices => {
            fs::copy(src, dst)?;
            fs::remove_file(src)
        }
        Err(err) => Err(err),
    }
}

/// Claim a free name in `info_dir` and write `contents` into its `.trashinfo`,
/// creating the file exclusively so two instances cannot take the same name.
fn create_trash_info(
    info_dir: &Path,
    file_name: &OsStr,
    contents: &str,
) -> Result<(OsString, PathBuf), ActionError> {
    for counter in 1..=MAX_TRASH_NAME_ATTEMPTS {
        let name = trash_name_for(file_name, counter);
        let info_path = info_dir.join(trash_info_file_name(&name));

        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_path)
        {
            Ok(mut file) => {
                if let Err(err) = file.write_all(contents.as_bytes()) {
                    let _ = fs::remove_file(&info_path);
                    return Err(ActionError::Io(err));
                }
                return Ok((name, info_path));
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(ActionError::Io(err)),
        }
    }

    Err(ActionError::Io(io::Error::other(
        "no free name in the trash",
    )))
}

fn trash_info_file_name(name: &OsStr) -> OsString {
    let mut bytes = name.as_bytes().to_vec();
    bytes.extend_from_slice(b".trashinfo");
    OsString::from_vec(bytes)
}

fn strip_trashinfo_suffix(file_name: &OsStr) -> Option<OsString> {
    let bytes = file_name.as_bytes();
    let stem = bytes.strip_suffix(b".trashinfo")?;
    if stem.is_empty() {
        return None;
    }
    Some(OsString::from_vec(stem.to_vec()))
}

/// `name`, and `stem.2.ext` / `name.2` / `stem.2` for later collisions, with
/// the counter inserted before the *first* dot — the naming the specification
/// and the portal use. Bytes are passed through, so non-UTF-8 names survive.
fn trash_name_for(file_name: &OsStr, counter: u32) -> OsString {
    let bytes = file_name.as_bytes();
    if counter <= 1 {
        return file_name.to_os_string();
    }

    let suffix = format!(".{counter}");
    let mut name = Vec::with_capacity(bytes.len() + suffix.len());
    match bytes.iter().position(|byte| *byte == b'.') {
        Some(dot) => {
            name.extend_from_slice(&bytes[..dot]);
            name.extend_from_slice(suffix.as_bytes());
            name.extend_from_slice(&bytes[dot..]);
        }
        None => {
            name.extend_from_slice(bytes);
            name.extend_from_slice(suffix.as_bytes());
        }
    }

    OsString::from_vec(name)
}

/// Restore handle for a file this process moved into the home or volume trash
/// itself, so unlike [`PortalTrashRestore`] it knows exactly where the entry is.
struct HomeTrashRestore {
    original_path: PathBuf,
    trashed_path: PathBuf,
    info_path: PathBuf,
    flushed: bool,
}

impl TrashRestoreHandle for HomeTrashRestore {
    fn restore(&mut self) -> Result<(), ActionError> {
        if self.flushed {
            return Err(ActionError::RestorationFailed("already flushed".into()));
        }
        if !self.trashed_path.exists() {
            return Err(ActionError::RestorationFailed(
                "the file is not in the trash any more (it may have been emptied)".into(),
            ));
        }
        if self.original_path.exists() {
            return Err(ActionError::RestorationFailed(
                "a file already exists at the original path".into(),
            ));
        }

        if let Some(parent) = self.original_path.parent() {
            fs::create_dir_all(parent).map_err(ActionError::Io)?;
        }
        move_file(&self.trashed_path, &self.original_path).map_err(ActionError::Io)?;
        // Removing the info file is what makes the entry disappear for the file
        // manager; a leftover would only be cosmetic, so its failure is not fatal.
        let _ = fs::remove_file(&self.info_path);

        self.flushed = true;
        Ok(())
    }

    fn flush_to_native_trash(&mut self) -> Result<(), ActionError> {
        self.flushed = true;
        Ok(())
    }
}

impl Drop for HomeTrashRestore {
    fn drop(&mut self) {
        if !self.flushed {
            let _ = self.flush_to_native_trash();
        }
    }
}

/// The youngest trashed entry for `original`, as the file below the trash
/// directory's `files` directory and its `info` metadata file.
fn find_entry(original: &Path) -> Option<(PathBuf, PathBuf)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_entry_in(&trash_directories(home.as_deref(), original), original)
}

fn find_entry_in(directories: &[PathBuf], original: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut youngest: Option<(String, PathBuf, PathBuf)> = None;

    for directory in directories {
        let Ok(entries) = fs::read_dir(directory.join("info")) else {
            continue;
        };
        for entry in entries.flatten() {
            let info_path = entry.path();
            let Some(stem) = info_path.file_name().and_then(strip_trashinfo_suffix) else {
                continue;
            };
            let Ok(contents) = fs::read_to_string(&info_path) else {
                continue;
            };
            let Some((trashed_from, deletion_date)) = parse_trash_info(&contents) else {
                continue;
            };
            if resolve_trashed_path(directory, &trashed_from) != original {
                continue;
            }
            // A name collision is resolved by renaming the entry in `files`,
            // and the info file carries that same name.
            let trashed_path = directory.join("files").join(stem);
            let is_younger = youngest
                .as_ref()
                .is_none_or(|(date, ..)| deletion_date > *date);
            if is_younger {
                youngest = Some((deletion_date, trashed_path, info_path));
            }
        }
    }

    youngest.map(|(_, trashed_path, info_path)| (trashed_path, info_path))
}

/// Resolve the `Path=` value from a `.trashinfo` file inside `trash_dir`.
///
/// Per the FreeDesktop Trash specification, home-trash entries record an
/// absolute path, while volume-trash entries (`$topdir/.Trash/$uid` or
/// `$topdir/.Trash-$uid`) record a path relative to `$topdir`.
fn resolve_trashed_path(trash_dir: &Path, trashed_from: &[u8]) -> PathBuf {
    let raw = Path::new(OsStr::from_bytes(trashed_from));
    if raw.is_absolute() {
        return raw.to_path_buf();
    }
    match trash_topdir(trash_dir) {
        Some(topdir) => topdir.join(raw),
        None => raw.to_path_buf(),
    }
}

/// The `$topdir` that relative `Path=` entries in `trash_dir` are anchored to:
/// `<topdir>` for `<topdir>/.Trash/<uid>` and `<topdir>/.Trash-<uid>`.
fn trash_topdir(trash_dir: &Path) -> Option<&Path> {
    let parent = trash_dir.parent()?;
    if parent.file_name() == Some(OsStr::new(".Trash")) {
        parent.parent()
    } else {
        Some(parent)
    }
}

/// The trash directories that can hold `original`: the home trash, plus the
/// per-volume trash (`.Trash/<uid>`, or `.Trash-<uid>` where the shared one is
/// not usable) of every directory above it. Walking upwards finds them without
/// parsing the mount table, since they sit at a mount point.
fn trash_directories(home: Option<&Path>, original: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();

    // The spec puts the home trash at `$XDG_DATA_HOME/Trash`. Inside the
    // sandbox that variable is the app-private data directory, so it is
    // resolved from the (real) home instead.
    if let Some(home) = home {
        directories.push(home.join(".local/share/Trash"));
    }

    let Some(uid) = home.and_then(user_id_of).or_else(|| user_id_of(original)) else {
        return directories;
    };
    if let Some(parent) = original.parent() {
        for ancestor in parent.ancestors() {
            let shared = ancestor.join(".Trash").join(uid.to_string());
            if shared.is_dir() {
                directories.push(shared);
            }
            let private = ancestor.join(format!(".Trash-{uid}"));
            if private.is_dir() {
                directories.push(private);
            }
        }
    }

    directories
}

fn user_id_of(path: &Path) -> Option<u32> {
    fs::metadata(path).ok().map(|metadata| metadata.uid())
}

/// The `Path` (percent-encoded) and `DeletionDate` of a `.trashinfo` file.
fn parse_trash_info(contents: &str) -> Option<(Vec<u8>, String)> {
    let mut trashed_from = None;
    let mut deletion_date = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("Path=") {
            trashed_from = Some(percent_decode(value.trim()));
        } else if let Some(value) = line.strip_prefix("DeletionDate=") {
            deletion_date = Some(value.trim().to_owned());
        }
    }

    Some((trashed_from?, deletion_date?))
}

/// Decode the percent-encoding the trash specification uses for paths.
fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    decoded
}

/// Encode a path the way the trash specification — and the portal — do: the
/// unreserved URI characters plus `/` stay as they are, everything else becomes
/// `%XX`.
fn percent_encode(input: &[u8]) -> String {
    let mut encoded = String::with_capacity(input.len());
    for &byte in input {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary directory that cleans up after itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "media-sort-trash-portal-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_trashinfo(trash: &Path, stem: &str, original: &Path, date: &str) {
        fs::create_dir_all(trash.join("info")).expect("create info dir");
        let encoded = original
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_') {
                    (*byte as char).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect::<String>();
        fs::write(
            trash.join("info").join(format!("{stem}.trashinfo")),
            format!("[Trash Info]\nPath={encoded}\nDeletionDate={date}\n"),
        )
        .expect("write trashinfo");
    }

    #[test]
    fn percent_decoding_handles_escapes_and_plain_bytes() {
        assert_eq!(percent_decode("/home/a%20b"), b"/home/a b");
        assert_eq!(percent_decode("%C3%A4.jpg"), "ä.jpg".as_bytes());
        // A truncated or malformed escape is left alone rather than dropping
        // bytes.
        assert_eq!(percent_decode("100%"), b"100%");
        assert_eq!(percent_decode("%zz"), b"%zz");
    }

    #[test]
    fn trashinfo_parsing_reads_path_and_date() {
        let parsed = parse_trash_info(
            "[Trash Info]\nPath=/tmp/a%20b.jpg\nDeletionDate=2026-10-16T01:02:03\n",
        );
        assert_eq!(
            parsed,
            Some((b"/tmp/a b.jpg".to_vec(), "2026-10-16T01:02:03".to_owned()))
        );
        assert_eq!(parse_trash_info("[Trash Info]\nPath=/tmp/a.jpg\n"), None);
        assert_eq!(parse_trash_info("garbage"), None);
    }

    #[test]
    fn finds_the_youngest_entry_for_the_path() {
        let temp = TempDir::new("youngest");
        let trash = temp.path().join("Trash");
        let original = temp.path().join("photo.jpg");

        write_trashinfo(
            &trash,
            "other.jpg",
            &temp.path().join("other.jpg"),
            "2026-10-16T05:00:00",
        );
        write_trashinfo(&trash, "photo.jpg", &original, "2026-10-16T03:00:00");
        // A later delete of a file that had the same name.
        write_trashinfo(&trash, "photo.jpg.2", &original, "2026-10-16T04:00:00");

        let (trashed_path, info_path) =
            find_entry_in(std::slice::from_ref(&trash), &original).expect("entry found");

        assert_eq!(trashed_path, trash.join("files/photo.jpg.2"));
        assert_eq!(info_path, trash.join("info/photo.jpg.2.trashinfo"));
    }

    #[test]
    fn ignores_entries_for_other_paths() {
        let temp = TempDir::new("other-paths");
        let trash = temp.path().join("Trash");
        write_trashinfo(
            &trash,
            "other.jpg",
            &temp.path().join("other.jpg"),
            "2026-10-16T05:00:00",
        );

        assert!(find_entry_in(&[trash], &temp.path().join("photo.jpg")).is_none());
    }

    #[test]
    fn trash_directories_cover_home_and_a_volume() {
        let temp = TempDir::new("directories");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let uid = user_id_of(&home).expect("uid of the temp home");

        let volume = temp.path().join("media/stick");
        let original = volume.join("DCIM/photo.jpg");
        fs::create_dir_all(&original).expect("create fixture");
        fs::create_dir_all(volume.join(format!(".Trash-{uid}"))).expect("create volume trash");

        let directories = trash_directories(Some(&home), &original);

        assert!(directories.contains(&home.join(".local/share/Trash")));
        assert!(directories.contains(&volume.join(format!(".Trash-{uid}"))));
        // Directories below the volume root are not trash locations.
        assert!(!directories.contains(&volume.join("DCIM").join(format!(".Trash-{uid}"))));
    }

    #[test]
    fn percent_encoding_round_trips() {
        let path = "/home/a b/100%/ä.png";

        let encoded = percent_encode(path.as_bytes());

        assert_eq!(percent_decode(&encoded), path.as_bytes());
        assert!(encoded.starts_with("/home/a%20b/"));
        assert!(encoded.contains("100%25"));
        assert!(encoded.contains("%C3%A4"));
    }

    #[test]
    fn collision_names_follow_the_trash_spec() {
        assert_eq!(trash_name_for(OsStr::new("photo.jpg"), 1), "photo.jpg");
        assert_eq!(trash_name_for(OsStr::new("photo.jpg"), 2), "photo.2.jpg");
        assert_eq!(
            trash_name_for(OsStr::new("archive.tar.gz"), 3),
            "archive.3.tar.gz"
        );
        assert_eq!(trash_name_for(OsStr::new("noext"), 2), "noext.2");
        // The counter goes before the first dot, exactly as the portal's
        // `get_unique_trash_name` does it — including a leading dot.
        assert_eq!(trash_name_for(OsStr::new(".bashrc"), 2), ".2.bashrc");
    }

    #[test]
    fn trashing_into_the_home_trash_moves_and_restores_the_file() {
        let temp = TempDir::new("home-trash");
        let base = temp.path().join("Trash");
        let original = temp.path().join("photo.jpg");
        fs::write(&original, b"content").expect("write file");

        let mut handle = trash_into_home_trash_at(&base, &original).expect("trash");

        assert!(!original.exists());
        assert_eq!(
            fs::read(base.join("files/photo.jpg")).expect("moved file"),
            b"content"
        );
        let info = fs::read_to_string(base.join("info/photo.jpg.trashinfo")).expect("info file");
        let (trashed_from, deletion_date) = parse_trash_info(&info).expect("info parses");
        assert_eq!(trashed_from, original.as_os_str().as_bytes().to_vec());
        assert_eq!(deletion_date.len(), "2026-10-18T17:00:00".len());
        assert_eq!(
            base.join("info").metadata().expect("info metadata").mode() & 0o777,
            0o700
        );

        handle.restore().expect("restore");

        assert_eq!(fs::read(&original).expect("restored file"), b"content");
        assert!(!base.join("info/photo.jpg.trashinfo").exists());
    }

    #[test]
    fn a_second_file_with_the_same_name_gets_a_counter() {
        let temp = TempDir::new("home-trash-collision");
        let base = temp.path().join("Trash");
        fs::create_dir_all(temp.path().join("a")).expect("create a");
        fs::create_dir_all(temp.path().join("b")).expect("create b");
        let first = temp.path().join("a/photo.jpg");
        let second = temp.path().join("b/photo.jpg");
        fs::write(&first, b"first").expect("write first");
        fs::write(&second, b"second").expect("write second");

        let _first = trash_into_home_trash_at(&base, &first).expect("trash first");
        let _second = trash_into_home_trash_at(&base, &second).expect("trash second");

        assert_eq!(
            fs::read(base.join("files/photo.jpg")).expect("first entry"),
            b"first"
        );
        assert_eq!(
            fs::read(base.join("files/photo.2.jpg")).expect("second entry"),
            b"second"
        );
        assert!(base.join("info/photo.2.jpg.trashinfo").exists());
    }

    #[test]
    fn restore_refuses_when_the_original_path_is_taken_again() {
        let temp = TempDir::new("home-trash-occupied");
        let base = temp.path().join("Trash");
        let original = temp.path().join("photo.jpg");
        fs::write(&original, b"content").expect("write file");
        let mut handle = trash_into_home_trash_at(&base, &original).expect("trash");
        fs::write(&original, b"replacement").expect("write replacement");

        assert!(handle.restore().is_err());
        // The trashed copy is untouched, so it can still be retrieved.
        assert_eq!(
            fs::read(base.join("files/photo.jpg")).expect("trashed file"),
            b"content"
        );
    }

    #[test]
    fn files_from_another_filesystem_are_refused() {
        // The home trash only holds files from the home filesystem: anything
        // else has to go to the volume's own trash.
        let temp = TempDir::new("home-trash-other-fs");
        let base = temp.path().join("Trash");
        fs::create_dir_all(&base).expect("create base");

        // /proc lives on a different filesystem than the temporary directory.
        let result = trash_into_home_trash_at(&base, Path::new("/proc/self/cmdline"));

        assert!(result.is_err());
    }

    #[test]
    fn finds_volume_trash_entries_recorded_with_relative_paths() {
        let temp = TempDir::new("volume-relative");
        let volume = temp.path().join("mnt/datasets");
        let trash = volume.join(".Trash-1000");
        let original = volume.join("DCIM/photo.jpg");

        // Per the FreeDesktop Trash spec, volume trashes record `Path=`
        // relative to the volume topdir (`DCIM/photo.jpg`, not `/.../mnt/datasets/DCIM/photo.jpg`).
        write_trashinfo(
            &trash,
            "photo.jpg",
            Path::new("DCIM/photo.jpg"),
            "2026-10-18T10:00:00",
        );

        let (trashed_path, info_path) =
            find_entry_in(std::slice::from_ref(&trash), &original).expect("volume entry found");

        assert_eq!(trashed_path, trash.join("files/photo.jpg"));
        assert_eq!(info_path, trash.join("info/photo.jpg.trashinfo"));
    }

    #[test]
    fn trashing_into_volume_trash_writes_relative_path_and_restores() {
        let temp = TempDir::new("volume-trash");
        let volume = temp.path().join("mnt/datasets");
        fs::create_dir_all(volume.join("sub")).expect("create sub");
        let original = volume.join("sub/photo.jpg");
        fs::write(&original, b"volume-content").expect("write file");

        let mut handle = trash_into_volume_trash_at(&volume, 1000, &original).expect("trash");

        let trash = volume.join(".Trash-1000");
        assert!(!original.exists());
        let info = fs::read_to_string(trash.join("info/photo.jpg.trashinfo")).expect("info file");
        let (trashed_from, _) = parse_trash_info(&info).expect("info parses");
        assert_eq!(trashed_from, b"sub/photo.jpg".to_vec());
        // `find_entry_in` must also match the relative path back to `original`.
        assert!(find_entry_in(std::slice::from_ref(&trash), &original).is_some());

        handle.restore().expect("restore");
        assert_eq!(fs::read(&original).expect("restored"), b"volume-content");
    }

    #[test]
    fn ensure_trash_directory_tightens_existing_0755_directories_to_0700() {
        let temp = TempDir::new("permissions");
        let base = temp.path().join("Trash");
        for dir in [&base, &base.join("files"), &base.join("info")] {
            fs::create_dir_all(dir).expect("create dir");
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("set 0755");
        }

        ensure_trash_directory(&base).expect("ensure trash directory");

        for dir in [&base, &base.join("files"), &base.join("info")] {
            assert_eq!(
                dir.metadata().expect("metadata").mode() & 0o7777,
                0o700,
                "{} must be tightened to 0700",
                dir.display()
            );
        }
    }
}
