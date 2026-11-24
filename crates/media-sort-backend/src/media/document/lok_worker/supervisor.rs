use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::protocol::{WorkerRequest, WorkerResponse, parse_response_line, write_request};
use crate::process::{is_sandboxed, new_hidden_command, path_starts_with_dash};

const READY_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const SHUTDOWN_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CRASHES: u32 = 3;

static WORKER_EXE: OnceLock<PathBuf> = OnceLock::new();
static LOK_CLIENT: Mutex<Option<LokClient>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::media::document) enum Priority {
    High,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::media::document) enum LokOutcome {
    Ok,
    Fallback(String),
}

pub(super) fn register_worker_exe(exe: PathBuf) {
    let _ = WORKER_EXE.set(exe);
}

/// Converts `src_path` to `out_pdf_path` via the supervised LibreOfficeKit worker subprocess.
pub(in crate::media::document) fn convert_to_pdf(
    src_path: &Path,
    out_pdf_path: &Path,
    page_range: Option<&str>,
    priority: Priority,
) -> LokOutcome {
    if is_sandboxed() {
        return LokOutcome::Fallback("Sandboxed environment".to_string());
    }
    if path_starts_with_dash(src_path) || path_starts_with_dash(out_pdf_path) {
        return LokOutcome::Fallback("Refusing '-' prefixed path".to_string());
    }
    if WORKER_EXE.get().is_none() {
        return LokOutcome::Fallback("LOK worker executable not registered".to_string());
    }

    let shared = LokClient::global_shared();
    submit_to_shared(
        &shared,
        src_path.to_path_buf(),
        out_pdf_path.to_path_buf(),
        page_range.map(str::to_owned),
        priority,
    )
}

/// Shuts down the active LOK supervisor thread and any live `--lok-worker` subprocess, if any.
pub(in crate::media::document) fn shutdown() {
    let client = {
        let mut slot = LOK_CLIENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.take()
    };
    drop(client);
}

struct PendingJob {
    src_path: PathBuf,
    out_pdf_path: PathBuf,
    page_range: Option<String>,
    reply: mpsc::SyncSender<LokOutcome>,
    alive: Weak<()>,
}

impl PendingJob {
    fn is_canceled(&self) -> bool {
        self.alive.strong_count() == 0
    }
}

#[derive(Default)]
struct QueueState {
    high: VecDeque<PendingJob>,
    low: VecDeque<PendingJob>,
    disabled: bool,
    shutdown: bool,
}

/// RAII handle owning the shared LOK supervisor queue and thread; dropping it signals shutdown,
/// drains queued jobs, and joins the supervisor thread.
struct LokClient {
    shared: Arc<(Mutex<QueueState>, Condvar)>,
    supervisor: Option<JoinHandle<()>>,
}

impl LokClient {
    fn global_shared() -> Arc<(Mutex<QueueState>, Condvar)> {
        let mut slot = LOK_CLIENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let client = slot.get_or_insert_with(Self::new);
        Arc::clone(&client.shared)
    }

    fn new() -> Self {
        let shared = Arc::new((Mutex::new(QueueState::default()), Condvar::new()));
        let worker_shared = Arc::clone(&shared);
        let supervisor = match std::thread::Builder::new()
            .name("lok-supervisor".to_string())
            .spawn(move || supervisor_loop(worker_shared))
        {
            Ok(handle) => Some(handle),
            Err(err) => {
                tracing::warn!(error = %err, "Failed to spawn LOK supervisor thread; disabling LOK");
                let mut state = shared
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.disabled = true;
                None
            }
        };
        Self { shared, supervisor }
    }
}

impl Drop for LokClient {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.shared;
            let mut state = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.shutdown = true;
            drain_disabled_jobs(&mut state, "LibreOfficeKit worker shutting down");
            cvar.notify_all();
        }

        if let Some(handle) = self.supervisor.take() {
            let _ = handle.join();
        }
    }
}

fn submit_to_shared(
    shared: &Arc<(Mutex<QueueState>, Condvar)>,
    src_path: PathBuf,
    out_pdf_path: PathBuf,
    page_range: Option<String>,
    priority: Priority,
) -> LokOutcome {
    let (tx, rx) = mpsc::sync_channel(1);
    let token = Arc::new(());
    let job = PendingJob {
        src_path,
        out_pdf_path,
        page_range,
        reply: tx,
        alive: Arc::downgrade(&token),
    };

    {
        let (lock, cvar) = &**shared;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.shutdown {
            return LokOutcome::Fallback("LibreOfficeKit worker shutting down".to_string());
        }
        if state.disabled {
            return LokOutcome::Fallback("LibreOfficeKit worker disabled".to_string());
        }
        match priority {
            Priority::High => state.high.push_back(job),
            Priority::Low => state.low.push_back(job),
        }
        cvar.notify_one();
    }

    let outcome = rx
        .recv()
        .unwrap_or_else(|_| LokOutcome::Fallback("LOK supervisor channel closed".to_string()));
    drop(token);
    outcome
}

fn pop_next_job(state: &mut QueueState) -> Option<PendingJob> {
    if let Some(job) = state.high.pop_front() {
        return Some(job);
    }
    while let Some(job) = state.low.pop_front() {
        if !job.is_canceled() {
            return Some(job);
        }
    }
    None
}

fn drain_disabled_jobs(state: &mut QueueState, reason: &str) {
    while let Some(job) = state.high.pop_front() {
        let _ = job.reply.try_send(LokOutcome::Fallback(reason.to_string()));
    }
    while let Some(job) = state.low.pop_front() {
        if !job.is_canceled() {
            let _ = job.reply.try_send(LokOutcome::Fallback(reason.to_string()));
        }
    }
}

fn terminate_child(stdin: &mut Option<ChildStdin>, child: &mut Option<Child>) {
    drop(stdin.take());
    if let Some(mut proc) = child.take() {
        let _ = proc.kill();
        let _ = proc.wait();
    }
}

/// RAII guard owning an in-flight spawned child during pipe setup and `Ready` handshake.
///
/// Dropping before [`ChildSpawnGuard::into_worker`] closes stdin and kills + reaps the child.
struct ChildSpawnGuard {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
}

impl ChildSpawnGuard {
    fn new(child: Child) -> Self {
        Self {
            child: Some(child),
            stdin: None,
        }
    }

    fn take_pipes(&mut self) -> Result<ChildStdout, SpawnFailure> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| SpawnFailure::Crashed("LOK worker child missing".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SpawnFailure::Crashed("LOK worker stdin pipe unavailable".into()))?;
        self.stdin = Some(stdin);
        child
            .stdout
            .take()
            .ok_or_else(|| SpawnFailure::Crashed("LOK worker stdout pipe unavailable".into()))
    }

    fn into_worker(
        mut self,
        resp_rx: mpsc::Receiver<Result<WorkerResponse, String>>,
    ) -> WorkerProcess {
        WorkerProcess {
            child: self.child.take(),
            stdin: self.stdin.take(),
            resp_rx,
        }
    }
}

impl Drop for ChildSpawnGuard {
    fn drop(&mut self) {
        terminate_child(&mut self.stdin, &mut self.child);
    }
}

/// RAII handle owning a ready `--lok-worker` subprocess; dropping it kills + reaps the child if
/// not already gracefully exited via [`WorkerProcess::shutdown_gracefully`].
struct WorkerProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    resp_rx: mpsc::Receiver<Result<WorkerResponse, String>>,
}

impl WorkerProcess {
    fn send_request(&mut self, req: &WorkerRequest) -> std::io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdin closed"))?;
        write_request(stdin, req)
    }

    fn shutdown_gracefully(mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = write_request(&mut stdin, &WorkerRequest::Shutdown);
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        let deadline = Instant::now() + SHUTDOWN_WAIT_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
            }
        }
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        terminate_child(&mut self.stdin, &mut self.child);
    }
}

fn supervisor_loop(shared: Arc<(Mutex<QueueState>, Condvar)>) {
    let (lock, cvar) = &*shared;
    let mut worker: Option<WorkerProcess> = None;
    let mut crash_count: u32 = 0;

    loop {
        let job = {
            let mut state = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if state.shutdown {
                    drain_disabled_jobs(&mut state, "LibreOfficeKit worker shutting down");
                    drop(state);
                    if let Some(proc) = worker.take() {
                        proc.shutdown_gracefully();
                    }
                    return;
                }

                if state.disabled {
                    drain_disabled_jobs(&mut state, "LibreOfficeKit worker disabled");
                    state = cvar
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    continue;
                }

                if let Some(job) = pop_next_job(&mut state) {
                    break job;
                }

                if worker.is_some() {
                    let (guard, timeout_res) = cvar
                        .wait_timeout(state, IDLE_TIMEOUT)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state = guard;
                    if timeout_res.timed_out()
                        && !state.shutdown
                        && state.high.is_empty()
                        && state.low.is_empty()
                    {
                        let idle_worker = worker.take();
                        drop(state);
                        if let Some(proc) = idle_worker {
                            proc.shutdown_gracefully();
                        }
                        state = lock
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                } else {
                    state = cvar
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        };

        if worker.is_none() {
            match spawn_worker() {
                Ok(proc) => worker = Some(proc),
                Err(SpawnFailure::InitFailed(reason)) => {
                    tracing::warn!(
                        reason = %reason,
                        "LibreOfficeKit initialization failed; disabling LOK permanently for this session"
                    );
                    let mut state = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.disabled = true;
                    drain_disabled_jobs(&mut state, &reason);
                    drop(state);
                    let _ = job.reply.try_send(LokOutcome::Fallback(reason));
                    continue;
                }
                Err(SpawnFailure::Crashed(reason)) => {
                    crash_count = crash_count.saturating_add(1);
                    tracing::warn!(
                        crash_count,
                        reason = %reason,
                        "LOK worker failed during startup"
                    );
                    if crash_count >= MAX_CRASHES {
                        let mut state = lock
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.disabled = true;
                        drain_disabled_jobs(&mut state, &reason);
                    }
                    let _ = job.reply.try_send(LokOutcome::Fallback(reason));
                    continue;
                }
            }
        }

        let Some(proc) = worker.as_mut() else {
            continue;
        };

        let req = WorkerRequest::Convert {
            src_path: job.src_path.to_string_lossy().into_owned(),
            out_pdf_path: job.out_pdf_path.to_string_lossy().into_owned(),
            page_range: job.page_range.clone(),
        };

        if let Err(err) = proc.send_request(&req) {
            fail_worker_job(
                &mut worker,
                lock,
                &mut crash_count,
                job,
                format!("Failed to send request to LOK worker: {err}"),
            );
            continue;
        }

        match proc.resp_rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(Ok(WorkerResponse::Ok)) => {
                let _ = job.reply.try_send(LokOutcome::Ok);
            }
            Ok(Ok(WorkerResponse::Err { error })) => {
                // Document-level failure: the worker process remains healthy for subsequent jobs.
                let _ = job.reply.try_send(LokOutcome::Fallback(error));
            }
            Ok(Ok(unexpected)) => fail_worker_job(
                &mut worker,
                lock,
                &mut crash_count,
                job,
                format!("Unexpected LOK worker response: {unexpected:?}"),
            ),
            Ok(Err(err)) => fail_worker_job(
                &mut worker,
                lock,
                &mut crash_count,
                job,
                format!("LOK worker disconnected: {err}"),
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => fail_worker_job(
                &mut worker,
                lock,
                &mut crash_count,
                job,
                format!("LOK worker timed out after {}s", REQUEST_TIMEOUT.as_secs()),
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => fail_worker_job(
                &mut worker,
                lock,
                &mut crash_count,
                job,
                "LOK worker exited unexpectedly".to_string(),
            ),
        }
    }
}

fn fail_worker_job(
    worker: &mut Option<WorkerProcess>,
    lock: &Mutex<QueueState>,
    crash_count: &mut u32,
    job: PendingJob,
    reason: String,
) {
    drop(worker.take());
    handle_worker_crash(lock, crash_count, &reason);
    let _ = job.reply.try_send(LokOutcome::Fallback(reason));
}

fn handle_worker_crash(lock: &Mutex<QueueState>, crash_count: &mut u32, reason: &str) {
    *crash_count = crash_count.saturating_add(1);
    tracing::warn!(crash_count = *crash_count, reason = %reason, "LOK worker crashed or timed out");
    if *crash_count >= MAX_CRASHES {
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disabled = true;
        drain_disabled_jobs(&mut state, reason);
    }
}

#[derive(Debug)]
enum SpawnFailure {
    InitFailed(String),
    Crashed(String),
}

fn spawn_worker() -> Result<WorkerProcess, SpawnFailure> {
    let exe = WORKER_EXE
        .get()
        .ok_or_else(|| SpawnFailure::InitFailed("LOK worker executable not registered".into()))?;

    let mut cmd = new_hidden_command(exe);
    cmd.arg("--lok-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let child = cmd
        .spawn()
        .map_err(|e| SpawnFailure::Crashed(format!("Failed to spawn LOK worker: {e}")))?;
    let mut guard = ChildSpawnGuard::new(child);
    let stdout = guard.take_pipes()?;

    let (resp_tx, resp_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("lok-stdout-reader".to_string())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = resp_tx.send(Err("LOK worker closed stdout".to_string()));
                        break;
                    }
                    Ok(_) => {
                        if let Some(parsed) = parse_response_line(&line) {
                            let send_res = match parsed {
                                Ok(resp) => resp_tx.send(Ok(resp)),
                                Err(err) => resp_tx
                                    .send(Err(format!("Malformed LOK worker response: {err}"))),
                            };
                            if send_res.is_err() {
                                break;
                            }
                        }
                    }
                    Err(err) => {
                        let _ =
                            resp_tx.send(Err(format!("Error reading LOK worker stdout: {err}")));
                        break;
                    }
                }
            }
        })
        .map_err(|e| SpawnFailure::Crashed(format!("Failed to spawn LOK stdout reader: {e}")))?;

    match resp_rx.recv_timeout(READY_TIMEOUT) {
        Ok(Ok(WorkerResponse::Ready)) => Ok(guard.into_worker(resp_rx)),
        Ok(Ok(WorkerResponse::InitFailed { error })) => Err(SpawnFailure::InitFailed(error)),
        Ok(Ok(other)) => Err(SpawnFailure::Crashed(format!(
            "Unexpected handshake response from LOK worker: {other:?}"
        ))),
        Ok(Err(err)) => Err(SpawnFailure::Crashed(err)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(SpawnFailure::Crashed(format!(
            "LOK worker did not become ready within {}s",
            READY_TIMEOUT.as_secs()
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(SpawnFailure::Crashed(
            "LOK worker exited before signaling ready".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_queue_pops_high_before_low_and_skips_disconnected_low() {
        let mut state = QueueState::default();
        let (tx_low_dropped, rx_low_dropped) = mpsc::sync_channel(1);
        let dropped_token = Arc::new(());
        let dropped_weak = Arc::downgrade(&dropped_token);
        drop(dropped_token);
        drop(rx_low_dropped);

        let (tx_low_live, _rx_low_live) = mpsc::sync_channel(1);
        let low_token = Arc::new(());

        let (tx_high, _rx_high) = mpsc::sync_channel(1);
        let high_token = Arc::new(());

        state.low.push_back(PendingJob {
            src_path: PathBuf::from("low-dropped.docx"),
            out_pdf_path: PathBuf::from("low-dropped.pdf"),
            page_range: Some("1".to_string()),
            reply: tx_low_dropped,
            alive: dropped_weak,
        });
        state.low.push_back(PendingJob {
            src_path: PathBuf::from("low-live.docx"),
            out_pdf_path: PathBuf::from("low-live.pdf"),
            page_range: Some("1".to_string()),
            reply: tx_low_live,
            alive: Arc::downgrade(&low_token),
        });
        state.high.push_back(PendingJob {
            src_path: PathBuf::from("high.docx"),
            out_pdf_path: PathBuf::from("high.pdf"),
            page_range: None,
            reply: tx_high,
            alive: Arc::downgrade(&high_token),
        });

        let first = pop_next_job(&mut state).expect("high priority job");
        assert_eq!(first.src_path, PathBuf::from("high.docx"));

        let second = pop_next_job(&mut state).expect("live low priority job");
        assert_eq!(second.src_path, PathBuf::from("low-live.docx"));

        assert!(pop_next_job(&mut state).is_none());
    }

    #[test]
    fn shutdown_exits_supervisor_and_drains_queued_jobs() {
        let shared = Arc::new((Mutex::new(QueueState::default()), Condvar::new()));
        let worker_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("lok-supervisor-test".to_string())
            .spawn(move || supervisor_loop(worker_shared))
            .expect("spawn test supervisor");

        let (tx, rx) = mpsc::sync_channel(1);
        let token = Arc::new(());
        {
            let (lock, cvar) = &*shared;
            let mut state = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.disabled = true;
            state.low.push_back(PendingJob {
                src_path: PathBuf::from("queued.docx"),
                out_pdf_path: PathBuf::from("queued.pdf"),
                page_range: Some("1".to_string()),
                reply: tx,
                alive: Arc::downgrade(&token),
            });
            state.shutdown = true;
            cvar.notify_all();
        }

        handle.join().expect("supervisor exits cleanly on shutdown");
        assert!(matches!(rx.try_recv(), Ok(LokOutcome::Fallback(_))));
        // Also ensure global `shutdown()` is safe when no client was started.
        shutdown();
    }

    #[test]
    fn lok_client_drop_shuts_down_supervisor_cleanly() {
        let client = LokClient::new();
        let shared = Arc::clone(&client.shared);

        let (tx, rx) = mpsc::sync_channel(1);
        let token = Arc::new(());
        {
            let (lock, _cvar) = &*shared;
            let mut state = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.disabled = true;
            state.low.push_back(PendingJob {
                src_path: PathBuf::from("drop-queued.docx"),
                out_pdf_path: PathBuf::from("drop-queued.pdf"),
                page_range: Some("1".to_string()),
                reply: tx,
                alive: Arc::downgrade(&token),
            });
        }

        drop(client);

        let state = shared
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.shutdown);
        assert!(state.high.is_empty() && state.low.is_empty());
        assert!(matches!(rx.try_recv(), Ok(LokOutcome::Fallback(_))));
    }

    #[test]
    fn child_spawn_guard_reaps_on_drop() {
        let exe = std::env::current_exe().expect("current test executable");
        let mut cmd = new_hidden_command(&exe);
        cmd.arg("--help")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = cmd.spawn().expect("spawn test binary");
        let mut guard = ChildSpawnGuard::new(child);
        let _stdout = guard.take_pipes().expect("take child pipes");
        drop(guard);
    }
}
