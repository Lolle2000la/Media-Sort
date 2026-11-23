use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::ThumbnailError;

/// Bounded work-queue capacity per pool. A scroll-storm through a folder of
/// video files used to feed unbounded `mpsc::channel` queues (and if every
/// mpv worker failed to start, requesters blocked forever on `recv()`);
/// `sync_channel` applies backpressure and the response timeouts un-hang
/// requesters when a pool is dead.
pub const THUMBNAIL_QUEUE_CAPACITY: usize = 256;

/// How long a producer may block on the bounded worker-pool queue before
/// giving up. A scroll-storm can fill the 256-slot queue; without this cap
/// a request would block indefinitely while every worker is busy.
pub const THUMBNAIL_QUEUE_SEND_TIMEOUT: Duration = Duration::from_secs(5);

pub type KeepPredicate = Arc<dyn Fn(&Path) -> bool + Send + Sync>;
pub type ThumbnailResult = Result<Option<(u32, u32, Vec<u8>)>, ThumbnailError>;

pub struct ThumbnailRequest {
    pub path: PathBuf,
    pub response: mpsc::Sender<ThumbnailResult>,
    pub should_keep: Option<KeepPredicate>,
}

/// Bounded send with a deadline: blocks up to `timeout`, then gives up
/// (returning `Err(value)`). `SyncSender::send_timeout` is not stable on
/// the MSRV toolchain (`std_internals`), so this is a `try_send` poll loop.
/// Callers must NOT hold the pool's `Mutex` across this call — clone the
/// sender out of the lock first.
pub fn send_with_timeout<T>(
    sender: &SyncSender<T>,
    mut value: T,
    timeout: Duration,
) -> Result<(), T> {
    let deadline = Instant::now() + timeout;
    loop {
        match sender.try_send(value) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(full_value)) => {
                if Instant::now() >= deadline {
                    return Err(full_value);
                }
                value = full_value;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(TrySendError::Disconnected(full_value)) => {
                return Err(full_value);
            }
        }
    }
}

/// Spawn a bounded worker pool that runs `process` for each queued
/// [`ThumbnailRequest`]. Each worker initializes its state via `init_state`;
/// if initialization fails (`None`), that worker thread exits cleanly.
pub fn spawn_worker_pool<S, Init, Process>(
    name: &'static str,
    num_workers: usize,
    init_state: Init,
    process: Process,
) -> Mutex<SyncSender<ThumbnailRequest>>
where
    S: Send + 'static,
    Init: Fn(usize) -> Option<S> + Send + Sync + 'static,
    Process: Fn(&mut S, &Path) -> ThumbnailResult + Send + Sync + 'static,
{
    let (tx, rx) = mpsc::sync_channel::<ThumbnailRequest>(THUMBNAIL_QUEUE_CAPACITY);
    let rx = Arc::new(Mutex::new(rx));
    let init_state = Arc::new(init_state);
    let process = Arc::new(process);

    for i in 0..num_workers {
        let rx = rx.clone();
        let init_state = init_state.clone();
        let process = process.clone();
        std::thread::spawn(move || {
            let Some(mut state) = init_state(i) else {
                return;
            };
            loop {
                let request = {
                    let guard = rx.lock().unwrap_or_else(|e| e.into_inner());
                    guard.recv().ok()
                };
                let Some(req) = request else {
                    break;
                };
                if req
                    .should_keep
                    .as_ref()
                    .is_some_and(|keep| !keep(&req.path))
                {
                    let _ = req.response.send(Ok(None));
                    continue;
                }
                let result = process(&mut state, &req.path);
                if req.response.send(result).is_err() {
                    tracing::warn!(
                        "{name} worker {i}: response channel closed for {}",
                        req.path.display()
                    );
                }
            }
        });
    }

    Mutex::new(tx)
}

/// Submit a request to `pool` without holding the pool mutex across the
/// bounded send, returning the one-shot response receiver.
pub fn submit_request(
    pool: &Mutex<SyncSender<ThumbnailRequest>>,
    path: &Path,
    should_keep: Option<KeepPredicate>,
    pool_label: &str,
) -> Result<Receiver<ThumbnailResult>, ThumbnailError> {
    let (response_tx, response_rx) = mpsc::channel();
    // Clone the sender and drop the mutex guard BEFORE the bounded send:
    // holding the pool's mutex across a blocked send would stall every other
    // thumbnail producer behind this request's backpressure.
    let sender = pool
        .lock()
        .expect("thumbnail worker pool lock is not poisoned")
        .clone();

    let req = ThumbnailRequest {
        path: path.to_path_buf(),
        response: response_tx,
        should_keep,
    };

    if send_with_timeout(&sender, req, THUMBNAIL_QUEUE_SEND_TIMEOUT).is_err() {
        return Err(ThumbnailError::transient(format!(
            "failed to queue {pool_label} thumbnail request"
        )));
    }

    Ok(response_rx)
}

/// Wait up to `timeout` for a worker pool response, mapping timeouts and
/// disconnected pools to [`ThumbnailError::Transient`].
pub fn await_response(
    rx: Receiver<ThumbnailResult>,
    timeout: Duration,
    pool_label: &str,
) -> ThumbnailResult {
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Drain if a result arrived in the tiny window around timeout,
            // otherwise treat as transient timeout.
            if let Ok(result) = rx.try_recv() {
                return result;
            }
            let _ = TryRecvError::Empty;
            Err(ThumbnailError::transient(format!(
                "{pool_label} thumbnail request timed out after {timeout:?}"
            )))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(ThumbnailError::transient(format!(
            "{pool_label} thumbnail worker pool disconnected"
        ))),
    }
}
