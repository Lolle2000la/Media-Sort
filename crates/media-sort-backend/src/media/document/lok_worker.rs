use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{DocumentError, MAX_PDF_FILE_BYTES, canonical_path_to_file_url, path_to_file_url};
use crate::process::{is_sandboxed, new_hidden_command, path_starts_with_dash};

const RESP_PREFIX: &str = "MS_LOK_RESP:";
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const SHUTDOWN_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CRASHES: u32 = 3;

static WORKER_EXE: OnceLock<PathBuf> = OnceLock::new();
static LOK_CLIENT: Mutex<Option<LokClient>> = Mutex::new(None);
static PROFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Registers the executable path used to spawn `--lok-worker` subprocesses.
pub fn register_worker_exe(exe: PathBuf) {
    let _ = WORKER_EXE.set(exe);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Priority {
    High,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LokOutcome {
    Ok,
    Fallback(String),
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum WorkerRequest {
    Convert {
        src_path: String,
        out_pdf_path: String,
        page_range: Option<String>,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
enum WorkerResponse {
    Ready,
    InitFailed { error: String },
    Ok,
    Err { error: String },
}

/// Converts `src_path` to `out_pdf_path` via the supervised LibreOfficeKit worker subprocess.
pub(super) fn convert_to_pdf(
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
pub(super) fn shutdown() {
    let client = {
        let mut slot = LOK_CLIENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.take()
    };
    let Some(mut client) = client else {
        return;
    };

    {
        let (lock, cvar) = &*client.shared;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.shutdown = true;
        drain_disabled_jobs(&mut state, "LibreOfficeKit worker shutting down");
        cvar.notify_all();
    }

    if let Some(handle) = client.supervisor.take() {
        let _ = handle.join();
    }
}

// ── Parent supervisor & priority queue ───────────────────────────────

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

struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    resp_rx: mpsc::Receiver<Result<WorkerResponse, String>>,
}

impl WorkerProcess {
    fn kill_and_reap(mut self) {
        drop(self.stdin);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn shutdown_gracefully(mut self) {
        let _ = write_request(&mut self.stdin, &WorkerRequest::Shutdown);
        drop(self.stdin);
        let deadline = Instant::now() + SHUTDOWN_WAIT_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
            }
        }
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

        if let Err(err) = write_request(&mut proc.stdin, &req) {
            let reason = format!("Failed to send request to LOK worker: {err}");
            if let Some(dead) = worker.take() {
                dead.kill_and_reap();
            }
            handle_worker_crash(lock, &mut crash_count, &reason);
            let _ = job.reply.try_send(LokOutcome::Fallback(reason));
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
            Ok(Ok(unexpected)) => {
                let reason = format!("Unexpected LOK worker response: {unexpected:?}");
                if let Some(dead) = worker.take() {
                    dead.kill_and_reap();
                }
                handle_worker_crash(lock, &mut crash_count, &reason);
                let _ = job.reply.try_send(LokOutcome::Fallback(reason));
            }
            Ok(Err(err)) => {
                let reason = format!("LOK worker disconnected: {err}");
                if let Some(dead) = worker.take() {
                    dead.kill_and_reap();
                }
                handle_worker_crash(lock, &mut crash_count, &reason);
                let _ = job.reply.try_send(LokOutcome::Fallback(reason));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let reason = format!("LOK worker timed out after {}s", REQUEST_TIMEOUT.as_secs());
                if let Some(dead) = worker.take() {
                    dead.kill_and_reap();
                }
                handle_worker_crash(lock, &mut crash_count, &reason);
                let _ = job.reply.try_send(LokOutcome::Fallback(reason));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let reason = "LOK worker exited unexpectedly".to_string();
                if let Some(dead) = worker.take() {
                    dead.kill_and_reap();
                }
                handle_worker_crash(lock, &mut crash_count, &reason);
                let _ = job.reply.try_send(LokOutcome::Fallback(reason));
            }
        }
    }
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

    let mut child = cmd
        .spawn()
        .map_err(|e| SpawnFailure::Crashed(format!("Failed to spawn LOK worker: {e}")))?;

    let Some(stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(SpawnFailure::Crashed(
            "LOK worker stdin pipe unavailable".into(),
        ));
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(SpawnFailure::Crashed(
            "LOK worker stdout pipe unavailable".into(),
        ));
    };

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
        .map_err(|e| {
            let _ = child.kill();
            let _ = child.wait();
            SpawnFailure::Crashed(format!("Failed to spawn LOK stdout reader: {e}"))
        })?;

    match resp_rx.recv_timeout(READY_TIMEOUT) {
        Ok(Ok(WorkerResponse::Ready)) => Ok(WorkerProcess {
            child,
            stdin,
            resp_rx,
        }),
        Ok(Ok(WorkerResponse::InitFailed { error })) => {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            Err(SpawnFailure::InitFailed(error))
        }
        Ok(Ok(other)) => {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            Err(SpawnFailure::Crashed(format!(
                "Unexpected handshake response from LOK worker: {other:?}"
            )))
        }
        Ok(Err(err)) => {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            Err(SpawnFailure::Crashed(err))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            Err(SpawnFailure::Crashed(format!(
                "LOK worker did not become ready within {}s",
                READY_TIMEOUT.as_secs()
            )))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            Err(SpawnFailure::Crashed(
                "LOK worker exited before signaling ready".into(),
            ))
        }
    }
}

fn write_request(writer: &mut impl Write, req: &WorkerRequest) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, req).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn write_response(writer: &mut impl Write, resp: &WorkerResponse) -> std::io::Result<()> {
    writer.write_all(RESP_PREFIX.as_bytes())?;
    serde_json::to_writer(&mut *writer, resp).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn parse_response_line(line: &str) -> Option<Result<WorkerResponse, serde_json::Error>> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let payload = trimmed.strip_prefix(RESP_PREFIX)?;
    Some(serde_json::from_str(payload))
}

fn build_page_range_filter_options(range: &str) -> String {
    serde_json::json!({
        "PageRange": {
            "type": "string",
            "value": range
        }
    })
    .to_string()
}

// ── Worker subprocess entry point ────────────────────────────────────

/// Entry point for the `--lok-worker` subprocess.
///
/// Loads LibreOfficeKit dynamically, serves line-delimited JSON conversion commands on stdin/stdout,
/// and destroys the LOK instance and temporary user profile before returning.
pub fn run_worker_main() -> i32 {
    let mut resp_writer = open_dedicated_response_writer();
    let profile_guard = match TempProfileGuard::create() {
        Ok(guard) => guard,
        Err(err) => {
            let _ = write_response(
                &mut resp_writer,
                &WorkerResponse::InitFailed {
                    error: format!("Failed to create LOK profile directory: {err}"),
                },
            );
            return 1;
        }
    };

    let lok = match LokInstance::initialize(&profile_guard.0) {
        Ok(lok) => lok,
        Err(err) => {
            let _ = write_response(&mut resp_writer, &WorkerResponse::InitFailed { error: err });
            drop(profile_guard);
            return 1;
        }
    };

    if write_response(&mut resp_writer, &WorkerResponse::Ready).is_err() {
        drop(lok);
        drop(profile_guard);
        return 1;
    }

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<WorkerRequest>(trimmed) {
                    Ok(WorkerRequest::Shutdown) => break,
                    Ok(WorkerRequest::Convert {
                        src_path,
                        out_pdf_path,
                        page_range,
                    }) => {
                        let resp = match lok.convert_to_pdf(
                            Path::new(&src_path),
                            Path::new(&out_pdf_path),
                            page_range.as_deref(),
                        ) {
                            Ok(()) => WorkerResponse::Ok,
                            Err(error) => WorkerResponse::Err { error },
                        };
                        if write_response(&mut resp_writer, &resp).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        let resp = WorkerResponse::Err {
                            error: format!("Invalid worker request: {err}"),
                        };
                        if write_response(&mut resp_writer, &resp).is_err() {
                            break;
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    drop(lok);
    drop(profile_guard);
    0
}

#[cfg(unix)]
fn open_dedicated_response_writer() -> Box<dyn Write> {
    use std::os::fd::FromRawFd as _;

    unsafe extern "C" {
        // SAFETY: Standard POSIX file descriptor duplication functions from libc.
        fn dup(oldfd: c_int) -> c_int;
        fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    }

    // SAFETY: fd 1 is stdout; duplicating it before redirecting fd 1 -> fd 2 isolates our JSON
    // response channel from any C++ prints LibreOffice emits on stdout.
    let saved_stdout = unsafe { dup(1) };
    if saved_stdout >= 0 {
        // SAFETY: fd 2 is stderr and `saved_stdout` owns the original stdout pipe fd.
        unsafe {
            let _ = dup2(2, 1);
        }
        // SAFETY: `saved_stdout` is a newly created valid owned file descriptor from `dup(1)`.
        let file = unsafe { File::from_raw_fd(saved_stdout) };
        Box::new(file)
    } else {
        Box::new(std::io::stdout())
    }
}

#[cfg(not(unix))]
fn open_dedicated_response_writer() -> Box<dyn Write> {
    Box::new(std::io::stdout())
}

struct TempProfileGuard(PathBuf);

impl TempProfileGuard {
    fn create() -> Result<Self, DocumentError> {
        let base = std::env::temp_dir();
        std::fs::create_dir_all(&base)?;
        let pid = std::process::id();
        for _ in 0..32 {
            let seq = PROFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let candidate = base.join(format!("media-sort-lok-profile-{pid}-{seq}"));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(Self(candidate)),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(DocumentError::Io(err)),
            }
        }
        Err(DocumentError::Message(
            "Failed to create temporary LOK user profile directory".to_string(),
        ))
    }
}

impl Drop for TempProfileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ── LibreOfficeKit dynamic loader & stable C ABI ─────────────────────

#[repr(C)]
struct LibreOfficeKit {
    p_class: *mut LibreOfficeKitClass,
}

#[repr(C)]
struct LibreOfficeKitClass {
    n_size: usize,
    destroy: Option<unsafe extern "C" fn(p_this: *mut LibreOfficeKit)>,
    document_load: Option<
        unsafe extern "C" fn(
            p_this: *mut LibreOfficeKit,
            p_url: *const c_char,
        ) -> *mut LibreOfficeKitDocument,
    >,
    get_error: Option<unsafe extern "C" fn(p_this: *mut LibreOfficeKit) -> *mut c_char>,
    document_load_with_options: Option<
        unsafe extern "C" fn(
            p_this: *mut LibreOfficeKit,
            p_url: *const c_char,
            p_options: *const c_char,
        ) -> *mut LibreOfficeKitDocument,
    >,
}

#[repr(C)]
struct LibreOfficeKitDocument {
    p_class: *mut LibreOfficeKitDocumentClass,
}

#[repr(C)]
struct LibreOfficeKitDocumentClass {
    n_size: usize,
    destroy: Option<unsafe extern "C" fn(p_this: *mut LibreOfficeKitDocument)>,
    save_as: Option<
        unsafe extern "C" fn(
            p_this: *mut LibreOfficeKitDocument,
            p_url: *const c_char,
            p_format: *const c_char,
            p_filter_options: *const c_char,
        ) -> c_int,
    >,
}

type Hook2Fn = unsafe extern "C" fn(
    install_path: *const c_char,
    user_profile_url: *const c_char,
) -> *mut LibreOfficeKit;
type HookFn = unsafe extern "C" fn(install_path: *const c_char) -> *mut LibreOfficeKit;

const fn has_vtable_member<T>(n_size: usize, offset: usize) -> bool {
    match offset.checked_add(std::mem::size_of::<T>()) {
        Some(end) => n_size >= end,
        None => false,
    }
}

fn os_fallback_install_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        vec![
            PathBuf::from("/usr/lib/libreoffice/program"),
            PathBuf::from("/usr/lib64/libreoffice/program"),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            PathBuf::from("/Applications/LibreOffice.app/Contents/Frameworks"),
            PathBuf::from("/Applications/LibreOffice.app/Contents/MacOS"),
        ]
    }
    #[cfg(target_os = "windows")]
    {
        let mut dirs = Vec::new();
        for env_var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(env_var) {
                dirs.push(PathBuf::from(base).join("LibreOffice").join("program"));
            }
        }
        dirs
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

fn candidate_library_names() -> &'static [&'static str] {
    #[cfg(target_os = "linux")]
    {
        &["libsofficeapp.so", "libmergedlo.so"]
    }
    #[cfg(target_os = "macos")]
    {
        &["libsofficeapp.dylib", "libmergedlo.dylib"]
    }
    #[cfg(target_os = "windows")]
    {
        &["sofficeapp.dll", "mergedlo.dll"]
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        &[]
    }
}

fn push_unique_dir(dirs: &mut Vec<PathBuf>, dir: PathBuf) {
    if !dirs.iter().any(|existing| existing == &dir) {
        dirs.push(dir);
    }
}

fn candidate_install_dirs_from_soffice(soffice: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(soffice_path) = soffice {
        let resolved = std::fs::canonicalize(soffice_path).unwrap_or_else(|_| soffice_path.into());
        if let Some(parent) = resolved.parent() {
            push_unique_dir(&mut dirs, parent.to_path_buf());
            #[cfg(target_os = "macos")]
            if parent.file_name().and_then(|n| n.to_str()) == Some("MacOS")
                && let Some(contents) = parent.parent()
            {
                push_unique_dir(&mut dirs, contents.join("Frameworks"));
            }
        }
        if let Some(raw_parent) = soffice_path.parent() {
            push_unique_dir(&mut dirs, raw_parent.to_path_buf());
        }
    }
    for fallback in os_fallback_install_dirs() {
        push_unique_dir(&mut dirs, fallback);
    }
    dirs
}

fn resolve_candidate_library_paths(
    install_dirs: &[PathBuf],
    lib_names: &[&str],
) -> Vec<(PathBuf, PathBuf)> {
    let mut candidates = Vec::new();
    for dir in install_dirs {
        for name in lib_names {
            let lib_path = dir.join(name);
            if lib_path.is_file() {
                candidates.push((dir.clone(), lib_path));
            }
        }
    }
    candidates
}

#[cfg(unix)]
struct DynamicLib {
    handle: *mut c_void,
}

#[cfg(unix)]
impl DynamicLib {
    fn open(path: &Path) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        #[link(name = "dl")]
        unsafe extern "C" {
            // SAFETY: Standard POSIX dynamic loader functions from libdl.
            fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
            fn dlerror() -> *const c_char;
        }
        #[cfg(not(target_os = "linux"))]
        unsafe extern "C" {
            // SAFETY: Standard POSIX dynamic loader functions from libSystem.
            fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
            fn dlerror() -> *const c_char;
        }

        const RTLD_NOW: c_int = 0x2;
        #[cfg(target_os = "macos")]
        const RTLD_LOCAL: c_int = 0x4;
        #[cfg(not(target_os = "macos"))]
        const RTLD_LOCAL: c_int = 0;

        let c_path = c_str_from_path(path)?;
        // SAFETY: `c_path` is a valid NUL-terminated path string; `RTLD_NOW | RTLD_LOCAL`
        // prevents symbol pollution into the rest of the process.
        let handle = unsafe { dlopen(c_path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            // SAFETY: `dlerror()` returns either null or a valid static/thread-local C string
            // describing the last dynamic linking failure.
            let err_ptr = unsafe { dlerror() };
            let detail = if err_ptr.is_null() {
                "unknown dlopen error".to_string()
            } else {
                // SAFETY: `dlerror()` returned a non-null NUL-terminated string.
                unsafe { CStr::from_ptr(err_ptr) }
                    .to_string_lossy()
                    .into_owned()
            };
            return Err(format!("dlopen({}) failed: {detail}", path.display()));
        }
        Ok(Self { handle })
    }

    unsafe fn get_symbol<T>(&self, name: &CStr) -> Option<T> {
        #[cfg(target_os = "linux")]
        #[link(name = "dl")]
        unsafe extern "C" {
            // SAFETY: Standard POSIX dynamic symbol lookup from libdl.
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }
        #[cfg(not(target_os = "linux"))]
        unsafe extern "C" {
            // SAFETY: Standard POSIX dynamic symbol lookup from libSystem.
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }

        const { assert!(std::mem::size_of::<T>() == std::mem::size_of::<*mut c_void>()) };
        // SAFETY: `self.handle` is a valid non-null handle returned by `dlopen` and `name` is
        // a valid NUL-terminated symbol name.
        let sym = unsafe { dlsym(self.handle, name.as_ptr()) };
        if sym.is_null() {
            None
        } else {
            // SAFETY: Caller guarantees `T` is the exact C function pointer type for `name`.
            Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&sym) })
        }
    }

    fn close_on_error(self) {
        #[cfg(target_os = "linux")]
        #[link(name = "dl")]
        unsafe extern "C" {
            // SAFETY: Standard POSIX dynamic loader cleanup from libdl.
            fn dlclose(handle: *mut c_void) -> c_int;
        }
        #[cfg(not(target_os = "linux"))]
        unsafe extern "C" {
            // SAFETY: Standard POSIX dynamic loader cleanup from libSystem.
            fn dlclose(handle: *mut c_void) -> c_int;
        }

        // SAFETY: `self.handle` was returned by `dlopen` and LOK was never initialized on it.
        unsafe {
            let _ = dlclose(self.handle);
        }
    }
}

#[cfg(target_os = "windows")]
struct DynamicLib {
    handle: windows::Win32::Foundation::HMODULE,
}

#[cfg(target_os = "windows")]
impl DynamicLib {
    fn open(path: &Path) -> Result<Self, String> {
        use windows::Win32::System::LibraryLoader::{
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW,
        };
        use windows::core::{HSTRING, PCWSTR};

        let wide = HSTRING::from(path.as_os_str());
        // SAFETY: `wide` is a valid NUL-terminated UTF-16 path and the search flags restrict
        // dependency resolution to the DLL's own directory and default system directories.
        let handle = unsafe {
            LoadLibraryExW(
                PCWSTR(wide.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        }
        .map_err(|e| format!("LoadLibraryExW({}) failed: {e}", path.display()))?;
        Ok(Self { handle })
    }

    unsafe fn get_symbol<T>(&self, name: &CStr) -> Option<T> {
        use windows::Win32::System::LibraryLoader::GetProcAddress;
        use windows::core::PCSTR;

        const {
            assert!(
                std::mem::size_of::<T>()
                    == std::mem::size_of::<unsafe extern "system" fn() -> isize>()
            )
        };
        // SAFETY: `self.handle` is a valid loaded HMODULE and `name` is a NUL-terminated ASCII
        // symbol name.
        let proc = unsafe { GetProcAddress(self.handle, PCSTR(name.as_ptr().cast())) }?;
        // SAFETY: Caller guarantees `T` matches the exported C function signature for `name`.
        Some(unsafe { std::mem::transmute_copy::<unsafe extern "system" fn() -> isize, T>(&proc) })
    }

    fn close_on_error(self) {
        use windows::Win32::Foundation::FreeLibrary;

        // SAFETY: `self.handle` was returned by `LoadLibraryExW` and LOK init did not succeed.
        unsafe {
            let _ = FreeLibrary(self.handle);
        }
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
struct DynamicLib;

#[cfg(not(any(unix, target_os = "windows")))]
impl DynamicLib {
    fn open(path: &Path) -> Result<Self, String> {
        Err(format!(
            "Dynamic LibreOfficeKit loading is unsupported on this OS ({})",
            path.display()
        ))
    }

    unsafe fn get_symbol<T>(&self, _name: &CStr) -> Option<T> {
        None
    }

    fn close_on_error(self) {}
}

struct LokInstance {
    // Keep the shared library mapped after LOK initialization: LibreOffice UNO/sal threads and
    // static destructors are unsafe to unmap via dlclose/FreeLibrary before process exit.
    _lib: DynamicLib,
    lok: *mut LibreOfficeKit,
}

impl LokInstance {
    fn initialize(profile_dir: &Path) -> Result<Self, String> {
        let soffice = super::soffice_pipe::find_soffice();
        let install_dirs = candidate_install_dirs_from_soffice(soffice.as_deref());
        let candidates = resolve_candidate_library_paths(&install_dirs, candidate_library_names());
        if candidates.is_empty() {
            return Err("Could not locate LibreOfficeKit shared library".to_string());
        }

        let mut last_err = String::from("Could not load LibreOfficeKit shared library");
        for (install_dir, lib_path) in candidates {
            let lib = match DynamicLib::open(&lib_path) {
                Ok(lib) => lib,
                Err(err) => {
                    last_err = err;
                    continue;
                }
            };

            let install_cstr = match c_str_from_path(&install_dir) {
                Ok(s) => s,
                Err(err) => {
                    lib.close_on_error();
                    last_err = err;
                    continue;
                }
            };

            let profile_url = path_to_file_url(profile_dir)
                .map_err(|e| format!("Failed to encode LOK profile URL: {e}"))?;
            let profile_cstr = CString::new(profile_url)
                .map_err(|e| format!("Invalid NUL byte in profile URL: {e}"))?;

            // SAFETY: `libreofficekit_hook_2` matches LibreOfficeKit's exported C hook signature.
            let hook2 = unsafe { lib.get_symbol::<Hook2Fn>(c"libreofficekit_hook_2") };
            let lok = if let Some(hook2_fn) = hook2 {
                // SAFETY: Both C strings are valid NUL-terminated paths/URLs for the duration of
                // the call.
                unsafe { hook2_fn(install_cstr.as_ptr(), profile_cstr.as_ptr()) }
            } else {
                // SAFETY: `libreofficekit_hook` matches LibreOfficeKit's exported C hook signature.
                let Some(hook_fn) = (unsafe { lib.get_symbol::<HookFn>(c"libreofficekit_hook") })
                else {
                    lib.close_on_error();
                    last_err = format!(
                        "{} does not export libreofficekit_hook_2 or libreofficekit_hook",
                        lib_path.display()
                    );
                    continue;
                };
                // SAFETY: `install_cstr` is a valid NUL-terminated path string.
                unsafe { hook_fn(install_cstr.as_ptr()) }
            };

            if lok.is_null() {
                lib.close_on_error();
                last_err = format!(
                    "LibreOfficeKit initialization returned null for {}",
                    lib_path.display()
                );
                continue;
            }

            return Ok(Self { _lib: lib, lok });
        }

        Err(last_err)
    }

    fn class(&self) -> Result<&LibreOfficeKitClass, String> {
        if self.lok.is_null() {
            return Err("LibreOfficeKit instance pointer is null".to_string());
        }
        // SAFETY: `self.lok` is a non-null instance returned by `libreofficekit_hook*`.
        let p_class = unsafe { (*self.lok).p_class };
        if p_class.is_null() {
            return Err("LibreOfficeKit vtable pointer is null".to_string());
        }
        // SAFETY: `p_class` points to a static LibreOfficeKitClass vtable owned by LOK.
        Ok(unsafe { &*p_class })
    }

    fn last_error(&self) -> Option<String> {
        let class = self.class().ok()?;
        if !has_vtable_member::<Option<unsafe extern "C" fn(*mut LibreOfficeKit) -> *mut c_char>>(
            class.n_size,
            std::mem::offset_of!(LibreOfficeKitClass, get_error),
        ) {
            return None;
        }
        let get_error = class.get_error?;
        // SAFETY: `self.lok` is valid and `n_size` covers `get_error`.
        let ptr = unsafe { get_error(self.lok) };
        if ptr.is_null() {
            None
        } else {
            // SAFETY: `get_error` returned a non-null NUL-terminated C string.
            Some(
                unsafe { CStr::from_ptr(ptr) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    fn convert_to_pdf(
        &self,
        src_path: &Path,
        out_pdf_path: &Path,
        page_range: Option<&str>,
    ) -> Result<(), String> {
        let class = self.class()?;
        if !has_vtable_member::<
            Option<
                unsafe extern "C" fn(
                    *mut LibreOfficeKit,
                    *const c_char,
                ) -> *mut LibreOfficeKitDocument,
            >,
        >(
            class.n_size,
            std::mem::offset_of!(LibreOfficeKitClass, document_load),
        ) {
            return Err("LibreOfficeKitClass.nSize is too small for document_load".to_string());
        }
        let document_load = class
            .document_load
            .ok_or_else(|| "LibreOfficeKitClass.document_load is null".to_string())?;

        let src_url =
            path_to_file_url(src_path).map_err(|e| format!("Invalid source path URL: {e}"))?;
        let out_url = non_existent_path_to_file_url(out_pdf_path)
            .map_err(|e| format!("Invalid output path URL: {e}"))?;

        let src_cstr = CString::new(src_url).map_err(|e| format!("Invalid source URL: {e}"))?;
        let out_cstr = CString::new(out_url).map_err(|e| format!("Invalid output URL: {e}"))?;
        let filter_cstr = page_range
            .map(|r| CString::new(build_page_range_filter_options(r)))
            .transpose()
            .map_err(|e| format!("Invalid filter options JSON: {e}"))?;

        // SAFETY: `self.lok` is valid, `n_size` covers `document_load`, and `src_cstr` is a valid
        // NUL-terminated `file://` URL.
        let doc_ptr = unsafe { document_load(self.lok, src_cstr.as_ptr()) };
        if doc_ptr.is_null() {
            let detail = self
                .last_error()
                .unwrap_or_else(|| "documentLoad returned null".to_string());
            return Err(format!("LOK documentLoad failed: {detail}"));
        }
        let doc_guard = LokDocumentGuard { doc: doc_ptr };

        let doc_class = doc_guard.class()?;
        if !has_vtable_member::<
            Option<
                unsafe extern "C" fn(
                    *mut LibreOfficeKitDocument,
                    *const c_char,
                    *const c_char,
                    *const c_char,
                ) -> c_int,
            >,
        >(
            doc_class.n_size,
            std::mem::offset_of!(LibreOfficeKitDocumentClass, save_as),
        ) {
            return Err("LibreOfficeKitDocumentClass.nSize is too small for save_as".to_string());
        }
        let save_as = doc_class
            .save_as
            .ok_or_else(|| "LibreOfficeKitDocumentClass.save_as is null".to_string())?;

        let opts_ptr = filter_cstr
            .as_ref()
            .map_or(std::ptr::null(), |c| c.as_ptr());

        // SAFETY: `doc_guard.doc` is a valid loaded document, `n_size` covers `save_as`, and
        // `out_cstr`, `c"pdf"`, and `opts_ptr` (either null or valid NUL-terminated JSON) are
        // valid for the duration of the call.
        let saved = unsafe { save_as(doc_guard.doc, out_cstr.as_ptr(), c"pdf".as_ptr(), opts_ptr) };
        if saved == 0 {
            let detail = self
                .last_error()
                .unwrap_or_else(|| "saveAs returned false".to_string());
            return Err(format!("LOK saveAs failed: {detail}"));
        }

        let meta = std::fs::metadata(out_pdf_path)
            .map_err(|e| format!("LOK saveAs completed without readable PDF output: {e}"))?;
        if meta.len() == 0 {
            return Err("LOK saveAs produced an empty PDF file".to_string());
        }
        if meta.len() > MAX_PDF_FILE_BYTES {
            return Err(format!(
                "LOK converted PDF exceeds size limit ({} > {MAX_PDF_FILE_BYTES} bytes)",
                meta.len()
            ));
        }
        Ok(())
    }
}

impl Drop for LokInstance {
    fn drop(&mut self) {
        if let Ok(class) = self.class()
            && has_vtable_member::<Option<unsafe extern "C" fn(*mut LibreOfficeKit)>>(
                class.n_size,
                std::mem::offset_of!(LibreOfficeKitClass, destroy),
            )
            && let Some(destroy) = class.destroy
        {
            // SAFETY: `self.lok` is a valid LOK instance being dropped once and `n_size` covers
            // `destroy`.
            unsafe { destroy(self.lok) };
        }
    }
}

struct LokDocumentGuard {
    doc: *mut LibreOfficeKitDocument,
}

impl LokDocumentGuard {
    fn class(&self) -> Result<&LibreOfficeKitDocumentClass, String> {
        if self.doc.is_null() {
            return Err("LibreOfficeKitDocument pointer is null".to_string());
        }
        // SAFETY: `self.doc` is a non-null document handle returned by `document_load`.
        let p_class = unsafe { (*self.doc).p_class };
        if p_class.is_null() {
            return Err("LibreOfficeKitDocument vtable pointer is null".to_string());
        }
        // SAFETY: `p_class` points to a static LibreOfficeKitDocumentClass vtable owned by LOK.
        Ok(unsafe { &*p_class })
    }
}

impl Drop for LokDocumentGuard {
    fn drop(&mut self) {
        if let Ok(class) = self.class()
            && has_vtable_member::<Option<unsafe extern "C" fn(*mut LibreOfficeKitDocument)>>(
                class.n_size,
                std::mem::offset_of!(LibreOfficeKitDocumentClass, destroy),
            )
            && let Some(destroy) = class.destroy
        {
            // SAFETY: `self.doc` is a valid LOK document being dropped once and `n_size` covers
            // `destroy`.
            unsafe { destroy(self.doc) };
        }
    }
}

fn non_existent_path_to_file_url(path: &Path) -> Result<String, DocumentError> {
    let parent = path.parent().ok_or_else(|| {
        DocumentError::Message(format!("Output path has no parent: {}", path.display()))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        DocumentError::Message(format!("Output path has no file name: {}", path.display()))
    })?;
    let canon_parent = std::fs::canonicalize(parent)?;
    Ok(canonical_path_to_file_url(&canon_parent.join(file_name)))
}

fn c_str_from_path(path: &Path) -> Result<CString, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        CString::new(path.as_os_str().as_bytes())
            .map_err(|e| format!("Path contains NUL byte ({}): {e}", path.display()))
    }
    #[cfg(not(unix))]
    {
        CString::new(path.to_string_lossy().as_bytes())
            .map_err(|e| format!("Path contains NUL byte ({}): {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefixed_responses_and_skips_stdout_noise() {
        assert!(parse_response_line("warn:sal.osl: something noisy\n").is_none());
        assert!(parse_response_line("   \r\n").is_none());

        assert_eq!(
            parse_response_line("MS_LOK_RESP:{\"status\":\"ready\"}\n")
                .expect("prefixed line")
                .expect("valid json"),
            WorkerResponse::Ready
        );
        assert_eq!(
            parse_response_line(
                "MS_LOK_RESP:{\"status\":\"init_failed\",\"error\":\"no lib\"}\r\n"
            )
            .expect("prefixed line")
            .expect("valid json"),
            WorkerResponse::InitFailed {
                error: "no lib".to_string(),
            }
        );
        assert_eq!(
            parse_response_line("MS_LOK_RESP:{\"status\":\"ok\"}\n")
                .expect("prefixed line")
                .expect("valid json"),
            WorkerResponse::Ok
        );
        assert_eq!(
            parse_response_line("MS_LOK_RESP:{\"status\":\"err\",\"error\":\"bad doc\"}\n")
                .expect("prefixed line")
                .expect("valid json"),
            WorkerResponse::Err {
                error: "bad doc".to_string(),
            }
        );
    }

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
    fn vtable_nsize_check_rejects_undersized_structs() {
        let doc_load_off = std::mem::offset_of!(LibreOfficeKitClass, document_load);
        type DocLoadFn = Option<
            unsafe extern "C" fn(*mut LibreOfficeKit, *const c_char) -> *mut LibreOfficeKitDocument,
        >;
        assert!(!has_vtable_member::<DocLoadFn>(0, doc_load_off));
        assert!(!has_vtable_member::<DocLoadFn>(
            doc_load_off + std::mem::size_of::<DocLoadFn>() - 1,
            doc_load_off,
        ));
        assert!(has_vtable_member::<DocLoadFn>(
            doc_load_off + std::mem::size_of::<DocLoadFn>(),
            doc_load_off,
        ));

        let save_as_off = std::mem::offset_of!(LibreOfficeKitDocumentClass, save_as);
        type SaveAsFn = Option<
            unsafe extern "C" fn(
                *mut LibreOfficeKitDocument,
                *const c_char,
                *const c_char,
                *const c_char,
            ) -> c_int,
        >;
        assert!(!has_vtable_member::<SaveAsFn>(
            std::mem::size_of::<usize>(),
            save_as_off
        ));
        assert!(has_vtable_member::<SaveAsFn>(
            std::mem::size_of::<LibreOfficeKitDocumentClass>(),
            save_as_off
        ));
    }

    #[test]
    fn builds_page_range_filter_options_json() {
        let json = build_page_range_filter_options("1");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(
            parsed,
            serde_json::json!({
                "PageRange": {
                    "type": "string",
                    "value": "1"
                }
            })
        );
    }

    #[test]
    fn candidate_library_resolution_follows_symlinked_soffice() {
        let guard = TempProfileGuard::create().expect("create temp test dir");
        let bin_dir = guard.0.join("bin");
        let program_dir = guard.0.join("libreoffice").join("program");
        std::fs::create_dir_all(&bin_dir).expect("create bin dir");
        std::fs::create_dir_all(&program_dir).expect("create program dir");

        let real_soffice = program_dir.join("soffice");
        std::fs::write(&real_soffice, b"").expect("write dummy soffice");
        let dummy_lib = program_dir.join("libsofficeapp.so");
        std::fs::write(&dummy_lib, b"").expect("write dummy lib");

        let soffice_entry = bin_dir.join("soffice");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_soffice, &soffice_entry).expect("create symlink");
        #[cfg(not(unix))]
        let soffice_entry = real_soffice.clone();

        let dirs = candidate_install_dirs_from_soffice(Some(&soffice_entry));
        let canon_program = std::fs::canonicalize(&program_dir).expect("canonicalize program dir");
        assert_eq!(dirs.first(), Some(&canon_program));

        let libs = resolve_candidate_library_paths(&dirs, &["libsofficeapp.so"]);
        assert_eq!(libs.first().map(|(d, _)| d), Some(&canon_program));
        assert_eq!(
            libs.first().map(|(_, p)| p.clone()),
            Some(canon_program.join("libsofficeapp.so"))
        );
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
}
