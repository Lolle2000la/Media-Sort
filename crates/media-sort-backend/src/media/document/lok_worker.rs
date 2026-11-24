//! Supervised `--lok-worker` LibreOfficeKit subprocess bridge and dynamic C ABI loader.

mod child;
mod dynlib;
mod lok_ffi;
mod protocol;
mod supervisor;

pub use child::run_worker_main;
pub(super) use supervisor::{LokOutcome, Priority, convert_to_pdf, shutdown};

/// Registers the executable path used to spawn `--lok-worker` subprocesses.
pub fn register_worker_exe(exe: std::path::PathBuf) {
    supervisor::register_worker_exe(exe);
}
