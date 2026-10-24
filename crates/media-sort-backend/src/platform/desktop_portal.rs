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
use std::future::Future;
use std::io;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use ashpd::desktop::open_uri::{OpenDirectoryRequest, OpenFileRequest};

/// How long a synchronous portal action may wait for the portal's answer.
///
/// A portal round trip takes a few milliseconds, but nothing else bounds it:
/// `ashpd`/`zbus` build their proxies with `method_timeout: None`, so the only
/// limit is the D-Bus broker's own reply timeout — nominally 25 s, and
/// unbounded if the socket wedges — and `pollster::block_on` would wait that
/// long. On the trash path the caller is the UI reducer thread.
const PORTAL_CALL_TIMEOUT: Duration = Duration::from_secs(3);

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
    /// The portal did not answer within [`PORTAL_CALL_TIMEOUT`].
    #[error(
        "the desktop portal did not answer `{call}` within {}s",
        PORTAL_CALL_TIMEOUT.as_secs()
    )]
    Timeout { call: &'static str },
    /// The deadline timer could not be started, so the call would be unbounded.
    #[error("could not start the portal timeout timer: {message}")]
    Thread { message: String },
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
    let result = run_portal_call(
        "OpenURI.OpenFile",
        OpenFileRequest::default().send_file(&file.as_fd()),
    );
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
    let result = run_portal_call(
        "OpenURI.OpenDirectory",
        OpenDirectoryRequest::default().send(&handle.as_fd()),
    );
    log_failure(result, path, "OpenURI.OpenDirectory")
}

/// Run a blocking portal request to completion, waiting at most
/// [`PORTAL_CALL_TIMEOUT`] for it.
///
/// The `ashpd` future is polled **on this thread** through [`WithTimeout`], so
/// on either outcome it — and the file descriptor it handed to the portal — is
/// dropped right here, before the caller continues: a hung request leaves
/// nothing running behind, unlike a request moved onto a detached thread. The
/// only extra thread is the deadline timer, which exits immediately once the
/// request finishes (dropping [`WithTimeout`] closes its channel) and otherwise
/// wakes this thread once at the deadline.
pub(crate) fn run_portal_call<T, F>(call: &'static str, future: F) -> Result<T, PortalError>
where
    F: Future<Output = ashpd::Result<T>>,
{
    run_portal_call_with_timeout(call, PORTAL_CALL_TIMEOUT, future)
}

fn run_portal_call_with_timeout<T, F>(
    call: &'static str,
    timeout: Duration,
    future: F,
) -> Result<T, PortalError>
where
    F: Future<Output = ashpd::Result<T>>,
{
    let bounded = WithTimeout::new(future, timeout).map_err(|err| PortalError::Thread {
        message: err.to_string(),
    })?;

    match pollster::block_on(bounded) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => {
            tracing::warn!(%err, "portal call {call} failed");
            Err(err.into())
        }
        Err(TimedOut) => Err(PortalError::Timeout { call }),
    }
}

/// The deadline passed before the wrapped future finished.
#[derive(Debug)]
struct TimedOut;

/// Polls `future` with a wall-clock deadline.
///
/// `pollster::block_on` has no timeout of its own, and `ashpd` leaves
/// `method_timeout` unset, so a wedged portal would park the caller until the
/// D-Bus broker gives up (25 s, or never). A single timer thread blocks on the
/// channel `_cancel_tx` owns: dropping this wrapper (which happens as soon as
/// the request completes) closes that channel, so the thread returns at once
/// instead of sleeping out the deadline; at the deadline it marks `expired` and
/// wakes the caller, whose next poll returns [`TimedOut`] and drops the inner
/// future on the caller's thread.
struct WithTimeout<F> {
    future: F,
    expired: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<Waker>>>,
    /// Dropping this sender is what stops the timer thread: its `recv_timeout`
    /// returns `Disconnected` at once instead of sleeping out the deadline.
    _cancel_tx: mpsc::Sender<()>,
}

impl<F> WithTimeout<F> {
    fn new(future: F, timeout: Duration) -> io::Result<Self> {
        let expired = Arc::new(AtomicBool::new(false));
        let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let (cancel_tx, cancel_rx) = mpsc::channel::<()>();

        let thread_expired = Arc::clone(&expired);
        let thread_waker = Arc::clone(&waker);
        std::thread::Builder::new()
            .name("portal-timeout".to_owned())
            .spawn(move || match cancel_rx.recv_timeout(timeout) {
                // The deadline passed: flag it and wake the caller, which
                // returns `TimedOut` on its next poll.
                Err(RecvTimeoutError::Timeout) => {
                    thread_expired.store(true, Ordering::SeqCst);
                    let waker = thread_waker.lock().ok().and_then(|mut slot| slot.take());
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                }
                // The request completed (`cancel_tx` dropped) or the channel
                // disconnected: nothing left to time.
                Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            })?;

        Ok(Self {
            future,
            expired,
            waker,
            _cancel_tx: cancel_tx,
        })
    }
}

impl<F: Future> Future for WithTimeout<F> {
    type Output = Result<F::Output, TimedOut>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `future` is never moved out of `self` — it is only ever
        // projected as a pinned reference — and `WithTimeout` therefore does not
        // rely on `Unpin` for its own soundness.
        let this = unsafe { self.get_unchecked_mut() };

        // Checked before polling so a deadline that passed before the first poll
        // (the timer thread runs independently of this one) is not missed.
        if this.expired.load(Ordering::SeqCst) {
            return Poll::Ready(Err(TimedOut));
        }

        if let Ok(mut slot) = this.waker.lock() {
            *slot = Some(cx.waker().clone());
        }

        // SAFETY: as above, the future is pinned in place for the wrapper's
        // whole lifetime.
        let future = unsafe { Pin::new_unchecked(&mut this.future) };
        match future.poll(cx) {
            Poll::Ready(value) => Poll::Ready(Ok(value)),
            Poll::Pending => {
                // The deadline may have passed while the inner future was being
                // polled; its wake has already been delivered, so nothing would
                // poll us again — re-check here instead of dropping it.
                if this.expired.load(Ordering::SeqCst) {
                    Poll::Ready(Err(TimedOut))
                } else {
                    Poll::Pending
                }
            }
        }
    }
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
fn log_failure<T>(
    result: Result<T, PortalError>,
    path: &Path,
    call: &str,
) -> Result<(), PortalError> {
    match result {
        Ok(_) => Ok(()),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "{call} failed");
            Err(err)
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

    /// A portal that never answers must surface as [`PortalError::Timeout`]
    /// after the budget instead of blocking the caller until the D-Bus broker
    /// gives up (on the trash path the caller is the UI reducer thread).
    #[test]
    fn a_hanging_portal_request_times_out_instead_of_blocking() {
        let started = std::time::Instant::now();
        // Stands in for a wedged portal: never resolves.
        let result: Result<u32, PortalError> = run_portal_call_with_timeout(
            "Trash.TrashFile",
            Duration::from_millis(50),
            std::future::pending::<ashpd::Result<u32>>(),
        );

        assert!(
            matches!(result, Err(PortalError::Timeout { call }) if call == "Trash.TrashFile"),
            "a hanging request must return PortalError::Timeout, got {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the caller must return at its own deadline (50ms here), not wait for the request to end or for a slow wakeup"
        );
    }

    /// The bounded wrapper must be transparent on the success path, and must not
    /// hold the caller until the deadline when the request completes at once.
    #[test]
    fn a_completed_portal_request_returns_its_value() {
        let started = std::time::Instant::now();
        let result: Result<u32, PortalError> = run_portal_call_with_timeout(
            "Trash.TrashFile",
            Duration::from_secs(30),
            std::future::ready(Ok(7)),
        );

        assert!(matches!(result, Ok(7)));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a request that finished immediately must return immediately, not when the timer expires"
        );
    }

    /// The inner future is dropped on the caller's thread as soon as the deadline
    /// passes, so nothing keeps running (or keeps the file descriptor open)
    /// behind a timed-out request.
    #[test]
    fn a_timed_out_request_future_is_dropped_on_the_caller_thread() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        impl Future for DropFlag {
            type Output = ashpd::Result<()>;
            fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
                Poll::Pending
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let result: Result<(), PortalError> = run_portal_call_with_timeout(
            "Trash.TrashFile",
            Duration::from_millis(50),
            DropFlag(Arc::clone(&dropped)),
        );

        assert!(matches!(result, Err(PortalError::Timeout { .. })));
        assert!(
            dropped.load(Ordering::SeqCst),
            "the request future (and with it the file descriptor it holds) must be dropped when the deadline passes"
        );
    }
}
