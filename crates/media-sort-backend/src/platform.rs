use std::fmt::Write as _;
use std::path::Path;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use std::time::Duration;

#[cfg(target_os = "linux")]
pub mod desktop_portal;
#[cfg(target_os = "windows")]
pub mod windows_shell;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const DBUS_SHOW_ITEMS_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const DBUS_OUTPUT_CAP_BYTES: u64 = 64 * 1024;

/// Returns `true` if `uri` is a well-formed `http://` or `https://` URL
/// without whitespace, control characters, or an empty authority.
pub fn is_safe_http_url(uri: &str) -> bool {
    let after_scheme = if uri.len() >= 8 && uri[..8].eq_ignore_ascii_case("https://") {
        &uri[8..]
    } else if uri.len() >= 7 && uri[..7].eq_ignore_ascii_case("http://") {
        &uri[7..]
    } else {
        return false;
    };

    !after_scheme.is_empty()
        && !after_scheme.starts_with(['/', '?', '#'])
        && !uri.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Open an HTTP/HTTPS URL in the user's default browser after validating the scheme.
pub fn open_url(uri: &str) -> Result<(), String> {
    if !is_safe_http_url(uri) {
        return Err(format!("Refusing to open unsafe or non-HTTP(S) URL: {uri}"));
    }
    open::that_detached(uri).map_err(|e| format!("Failed to open URL: {e}"))
}

/// Open `path` with the system's default application.
///
/// On Linux inside a Flatpak sandbox this delegates to `OpenURI.OpenFile`;
/// only [`desktop_portal::PortalError::Unavailable`] falls through to
/// `open::that(path)` so a failed or timed-out portal call is never retried via
/// `xdg-open`.
pub fn open_externally(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        match desktop_portal::open_externally(path) {
            Ok(()) => return Ok(()),
            Err(desktop_portal::PortalError::Unavailable) => {}
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "the OpenURI portal could not open the file; not retrying through xdg-open"
                );
                return Err(err.to_string());
            }
        }
    }

    open::that(path).map_err(|e| format!("Failed to open file externally: {e}"))
}

/// Reveal `path` in the system file manager.
pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        match desktop_portal::reveal_in_file_manager(path) {
            Ok(()) => return Ok(()),
            Err(desktop_portal::PortalError::Unavailable) => {}
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "the OpenURI portal could not reveal the file; the sandbox blocks the native routes"
                );
                return Err(err.to_string());
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to reveal file in file manager: {e}"))
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to reveal file in file manager: {e}"))
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let uri = encode_file_uri(path);
        let mut cmd = std::process::Command::new("dbus-send");
        cmd.args([
            "--session",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("array:string:{uri}"),
            "string:",
        ]);

        let dbus_ok = crate::process::run_command_capped(
            &mut cmd,
            DBUS_SHOW_ITEMS_TIMEOUT,
            DBUS_OUTPUT_CAP_BYTES,
            DBUS_OUTPUT_CAP_BYTES,
            "dbus-send",
        )
        .is_ok_and(|out| out.status.success());

        if dbus_ok {
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            std::process::Command::new("xdg-open")
                .arg(parent)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("Failed to reveal file in file manager: {e}"))
        } else {
            Err("No parent directory".to_string())
        }
    }
}

/// Percent-encode everything except RFC 3986 unreserved characters plus `/` and `:`
/// so `dbus-send`'s `array:string:` argument cannot be split or mangled.
#[cfg_attr(any(target_os = "windows", target_os = "macos"), allow(dead_code))]
fn encode_file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for ch in path.to_string_lossy().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~' | '/' | ':') {
            uri.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for byte in ch.encode_utf8(&mut buf).as_bytes() {
                let _ = write!(uri, "%{byte:02X}");
            }
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_http_url() {
        assert!(is_safe_http_url("https://mediasort.app"));
        assert!(is_safe_http_url("http://example.com:8080/path?a=1&b=2#top"));
        assert!(is_safe_http_url(
            "HTTPS://GITHUB.COM/Lolle2000la/Media-Sort"
        ));

        assert!(!is_safe_http_url(""));
        assert!(!is_safe_http_url("https://"));
        assert!(!is_safe_http_url("http:///etc/passwd"));
        assert!(!is_safe_http_url("https://?query=1"));
        assert!(!is_safe_http_url("file:///etc/passwd"));
        assert!(!is_safe_http_url("javascript:alert(1)"));
        assert!(!is_safe_http_url(
            "data:text/html,<script>alert(1)</script>"
        ));
        assert!(!is_safe_http_url("ftp://example.com/file"));
        assert!(!is_safe_http_url("/home/user/photo.jpg"));
        assert!(!is_safe_http_url("https://example.com/foo bar"));
        assert!(!is_safe_http_url("https://example.com\n"));
    }

    #[test]
    fn test_open_url_rejects_non_http() {
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "smb://server/share",
            "/tmp/test.jpg",
            "https://",
        ] {
            let err = open_url(bad).expect_err("non-http(s) URL must be rejected");
            assert!(
                err.contains("Refusing to open unsafe"),
                "unexpected error for {bad}: {err}"
            );
        }
    }

    #[test]
    fn test_encode_file_uri() {
        assert_eq!(
            encode_file_uri(Path::new("/home/user/photo.jpg")),
            "file:///home/user/photo.jpg"
        );
        assert_eq!(
            encode_file_uri(Path::new("/tmp/a b,c\"d&e;f/café.png")),
            "file:///tmp/a%20b%2Cc%22d%26e%3Bf/caf%C3%A9.png"
        );
        assert_eq!(
            encode_file_uri(Path::new("/tmp/unreserved-_.~/x:1.png")),
            "file:///tmp/unreserved-_.~/x:1.png"
        );
    }
}
