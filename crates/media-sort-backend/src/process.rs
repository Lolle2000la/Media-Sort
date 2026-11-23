use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Output captured from a bounded subprocess execution via [`run_command_capped`].
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Returns `true` when running inside a Flatpak or Snap sandbox.
pub fn is_sandboxed() -> bool {
    #[cfg(target_os = "linux")]
    if crate::platform::desktop_portal::in_flatpak() {
        return true;
    }
    std::env::var_os("SNAP").is_some_and(|v| !v.is_empty())
}

/// Whether `path`'s first byte/code unit is `-`, i.e. the argument would be
/// parsed by CLI tools like `ffmpeg` as an option rather than a path value.
///
/// Checked on the raw OS string rather than `to_str()`: a non-UTF8 path (or,
/// on Windows, a path with an unpaired surrogate) whose first byte is `-` would
/// otherwise bypass a UTF-8-only check even though `Command::arg` passes the
/// exact raw bytes to the child process.
pub fn path_starts_with_dash(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().first() == Some(&b'-')
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        path.as_os_str().encode_wide().next() == Some(0x002D)
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.to_str().is_some_and(|s| s.starts_with('-'))
    }
}

/// Search for the first existing executable matching any name in `names`,
/// optionally checking the current executable's directory first before `PATH`.
pub fn find_executable_on_path(names: &[&str], check_exe_dir: bool) -> Option<PathBuf> {
    if check_exe_dir
        && let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
        && let Some(found) = first_matching_file(dir, names)
    {
        return Some(found);
    }

    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| first_matching_file(&dir, names))
}

fn first_matching_file(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| {
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}

/// `CREATE_NO_WINDOW` (0x0800_0000, winbase.h).
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Build a [`Command`] for `program` with the platform's "no console window"
/// flags applied.
///
/// On Windows, spawning a console subprocess from a `windows_subsystem = "windows"`
/// binary allocates a visible console window per spawn unless `CREATE_NO_WINDOW`
/// is set.
pub fn new_hidden_command(program: &Path) -> Command {
    #[allow(unused_mut)] // `command` is only mutated on Windows targets
    let mut command = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn read_pipe_capped<R: std::io::Read>(mut pipe: R, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    pipe.by_ref().take(max_bytes).read_to_end(&mut buf)?;

    // Drain any excess output so the child never blocks on a full pipe buffer
    // or receives spurious SIGPIPE while our retained buffer stays bounded.
    let mut scratch = [0u8; 8192];
    loop {
        match pipe.read(&mut scratch) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(buf)
}

fn kill_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Execute `cmd` with `Stdio::null()` stdin, bounded stdout/stderr reader
/// threads, and `try_wait` backoff polling up to `timeout`. Kills and reaps
/// the child process on timeout or error.
pub fn run_command_capped(
    cmd: &mut Command,
    timeout: Duration,
    max_stdout_bytes: u64,
    max_stderr_bytes: u64,
    tool_label: &str,
) -> Result<CommandOutput, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{tool_label} spawn: {e}"))?;

    let Some(stdout_pipe) = child.stdout.take() else {
        kill_and_reap(&mut child);
        return Err(format!("{tool_label} stdout pipe missing"));
    };
    let Some(stderr_pipe) = child.stderr.take() else {
        kill_and_reap(&mut child);
        return Err(format!("{tool_label} stderr pipe missing"));
    };

    let stdout_thread = std::thread::spawn(move || read_pipe_capped(stdout_pipe, max_stdout_bytes));
    let stderr_thread = std::thread::spawn(move || read_pipe_capped(stderr_pipe, max_stderr_bytes));

    let deadline = Instant::now() + timeout;
    let mut backoff = Duration::from_millis(1);
    let max_backoff = Duration::from_millis(10);

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let now = Instant::now();
                if now >= deadline {
                    kill_and_reap(&mut child);
                    // Reader threads are intentionally left detached: kill+wait
                    // closes the child's write ends so they exit on EOF, whereas
                    // joining here could hang if a grandchild inherited the pipe.
                    return Err(format!("{tool_label} timed out after {timeout:?}"));
                }
                let sleep_for = backoff.min(deadline.saturating_duration_since(now));
                std::thread::sleep(sleep_for);
                backoff = (backoff * 2).min(max_backoff);
            }
            Err(e) => {
                kill_and_reap(&mut child);
                return Err(format!("{tool_label} wait: {e}"));
            }
        }
    };

    let stdout = stdout_thread
        .join()
        .map_err(|_| format!("{tool_label} stdout thread panicked"))?
        .map_err(|e| format!("{tool_label} stdout read: {e}"))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| format!("{tool_label} stderr thread panicked"))?
        .map_err(|e| format!("{tool_label} stderr read: {e}"))?;

    Ok(CommandOutput {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_starts_with_dash_utf8() {
        assert!(path_starts_with_dash(Path::new("-evil/x.jpg")));
        assert!(path_starts_with_dash(Path::new("-")));
        assert!(!path_starts_with_dash(Path::new("/tmp/-evil/x.jpg")));
        assert!(!path_starts_with_dash(Path::new("x.jpg")));
    }

    #[cfg(unix)]
    #[test]
    fn test_path_starts_with_dash_non_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let raw = vec![b'-', 0xFF, b'e'];
        assert!(path_starts_with_dash(Path::new(
            OsString::from_vec(raw).as_os_str()
        )));

        let raw = vec![0xFF, b'-'];
        assert!(!path_starts_with_dash(Path::new(
            OsString::from_vec(raw).as_os_str()
        )));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_command_capped_success_and_stdout_cap() {
        let mut cmd = new_hidden_command(Path::new("sh"));
        cmd.args(["-c", "printf '0123456789abcdef'"]);
        let out = run_command_capped(&mut cmd, Duration::from_secs(2), 6, 1024, "test-cap")
            .expect("command should succeed");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"012345");
        assert!(out.stderr.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn test_run_command_capped_success_and_stdout_cap() {
        let mut cmd = new_hidden_command(Path::new("cmd"));
        cmd.args(["/C", "echo 0123456789abcdef"]);
        let out = run_command_capped(&mut cmd, Duration::from_secs(2), 6, 1024, "test-cap")
            .expect("command should succeed");
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 6);
        assert_eq!(&out.stdout, b"012345");
    }

    #[test]
    fn test_run_command_capped_timeout() {
        #[cfg(unix)]
        let mut cmd = {
            let mut c = new_hidden_command(Path::new("sh"));
            c.args(["-c", "sleep 5"]);
            c
        };
        #[cfg(windows)]
        let mut cmd = {
            let mut c = new_hidden_command(Path::new("cmd"));
            c.args(["/C", "ping -n 6 127.0.0.1 >nul"]);
            c
        };

        let started = Instant::now();
        let err = run_command_capped(
            &mut cmd,
            Duration::from_millis(40),
            1024,
            1024,
            "test-timeout",
        )
        .expect_err("sleeping process should time out");
        assert!(err.contains("timed out"), "unexpected error: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "timeout took too long: {:?}",
            started.elapsed()
        );
    }
}
