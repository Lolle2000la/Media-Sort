//! Reaching the user's desktop from inside the Flatpak sandbox (Linux).
//!
//! Under Flatpak the file manager and the default handler for a file are *host*
//! applications. The sandbox's session-bus policy lets an app talk only to its
//! own name, `org.freedesktop.DBus` and `org.freedesktop.portal.*`
//! (<https://docs.flatpak.org/en/latest/sandbox-permissions.html>), so
//! `dbus-send … org.freedesktop.FileManager1` is refused outright and
//! `xdg-open` reaches the host only by accident.
//! `org.freedesktop.portal.OpenURI` is the supported route: it takes a file
//! descriptor and the portal hands it to the host, which is also the only way a
//! document-portal path can reach a host application at all.
//!
//! Outside a sandbox every caller keeps its native
//! `open`/`explorer`/`xdg-open` implementation, so nothing here runs unless
//! [`in_flatpak`] says so. The same predicate gates the trash portal
//! ([`crate::filesystem::trash`]), which re-exports it.

use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use ashpd::desktop::open_uri::{OpenDirectoryRequest, OpenFileRequest};

/// Why a portal action was not performed.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    /// The process is not sandboxed, or the path cannot be handed over, so the
    /// caller's native implementation applies.
    #[error("the desktop portal is not available")]
    Unavailable,
    /// A portal request failed.
    #[error(transparent)]
    Portal(#[from] ashpd::Error),
    /// The path could not be opened. The portal authorises on an open file
    /// descriptor, so a path this process cannot open cannot be handed over.
    #[error("failed to open {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Whether this process runs inside a Flatpak sandbox, where the host's
/// applications are reachable only through the desktop portals.
pub fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some_and(|id| !id.is_empty())
        || Path::new("/.flatpak-info").exists()
}

/// Open `path` with the user's default application (`OpenURI.OpenFile`).
///
/// Returns [`PortalError::Unavailable`] when the process is not sandboxed or
/// `path` is not an existing file, so the caller keeps its native `open::that`
/// path.
pub fn open_externally(path: &Path) -> Result<(), PortalError> {
    // `open_externally` is also called with web addresses, and OpenURI's file
    // call takes a descriptor, so anything that is not an existing file is left
    // to the caller's `xdg-open`/`open::that` handling.
    if !in_flatpak() || !path.exists() {
        return Err(PortalError::Unavailable);
    }
    let file = open(path)?;
    let result = pollster::block_on(OpenFileRequest::default().send_file(&file.as_fd()));
    log_failure(result, path, "OpenURI.OpenFile")
}

/// Show `path` in the file manager (`OpenURI.OpenDirectory`).
///
/// Despite its name, `OpenURI.OpenDirectory` takes a file descriptor for the
/// file (or directory) to reveal and asks the host to open its *containing*
/// directory with the item selected via `org.freedesktop.FileManager1.ShowItems`
/// (falling back to opening the containing directory when `FileManager1` is
/// absent). Passing `path.parent()` would open the *grandparent* directory and
/// highlight the parent folder instead of the file. We only fall back to
/// [`fallback_parent_directory`] when `path` itself cannot be opened.
pub fn reveal_in_file_manager(path: &Path) -> Result<(), PortalError> {
    if !in_flatpak() {
        return Err(PortalError::Unavailable);
    }
    let handle = open(path).or_else(|err| match fallback_parent_directory(path) {
        Some(parent) => open(parent),
        None => Err(err),
    })?;
    let result = pollster::block_on(OpenDirectoryRequest::default().send(&handle.as_fd()));
    log_failure(result, path, "OpenURI.OpenDirectory")
}

/// Fallback parent directory when `path` itself no longer exists on disk, or
/// `None` when `path` is a bare file name or filesystem root.
fn fallback_parent_directory(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

/// Open `path` read-only, for the descriptor the portal authorises on.
fn open(path: &Path) -> Result<File, PortalError> {
    File::open(path).map_err(|source| PortalError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// `Ok` once the request was handed to the portal — for these two actions there
/// is no further response to act on, so only failure is reported.
fn log_failure<T>(result: ashpd::Result<T>, path: &Path, call: &str) -> Result<(), PortalError> {
    match result {
        Ok(_) => Ok(()),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "{call} failed");
            Err(err.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_parent_directory_resolves_containing_folder() {
        assert_eq!(
            fallback_parent_directory(Path::new("/home/user/Pictures/img.png")),
            Some(Path::new("/home/user/Pictures"))
        );

        // A bare file name has an empty parent, which would otherwise open the
        // current directory instead of anything the user asked for.
        assert_eq!(fallback_parent_directory(Path::new("img.png")), None);

        // A filesystem root has no parent at all.
        assert_eq!(fallback_parent_directory(Path::new("/")), None);
    }
}
