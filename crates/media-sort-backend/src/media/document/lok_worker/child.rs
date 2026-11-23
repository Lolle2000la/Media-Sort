use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use super::lok_ffi::LokInstance;
use super::protocol::{WorkerRequest, WorkerResponse, write_response};
use crate::media::document::soffice_pipe::ManagedTempDir;

/// Entry point for the `--lok-worker` subprocess.
///
/// Loads LibreOfficeKit dynamically, serves line-delimited JSON conversion commands on stdin/stdout,
/// and destroys the LOK instance and temporary user profile via RAII before returning.
pub fn run_worker_main() -> i32 {
    let mut resp_writer = open_dedicated_response_writer();
    // Declared before `lok` so Rust's reverse-declaration drop order destroys `LokInstance` before
    // `ManagedTempDir` removes the temporary profile directory on every exit path.
    let profile_guard = match ManagedTempDir::create_in_temp("media-sort-lok-profile") {
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

    let lok = match LokInstance::initialize(profile_guard.path()) {
        Ok(lok) => lok,
        Err(err) => {
            let _ = write_response(&mut resp_writer, &WorkerResponse::InitFailed { error: err });
            return 1;
        }
    };

    if write_response(&mut resp_writer, &WorkerResponse::Ready).is_err() {
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

    0
}

#[cfg(unix)]
fn open_dedicated_response_writer() -> Box<dyn Write> {
    use std::ffi::c_int;
    use std::fs::File;
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
