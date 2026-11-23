use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(target_os = "windows")]
const FFMPEG_EXE: &str = "ffmpeg.exe";
#[cfg(not(target_os = "windows"))]
const FFMPEG_EXE: &str = "ffmpeg";

const FFMPEG_TIMEOUT: Duration = Duration::from_secs(30);
const FFMPEG_VERIFY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FFMPEG_STDOUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FFMPEG_STDERR_BYTES: u64 = 64 * 1024;
const MAX_VERIFY_OUTPUT_BYTES: u64 = 64 * 1024;

static FFMPEG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn verify_ffmpeg(ffmpeg_path: &Path) -> bool {
    let mut cmd = crate::process::new_hidden_command(ffmpeg_path);
    cmd.arg("-version");
    crate::process::run_command_capped(
        &mut cmd,
        FFMPEG_VERIFY_TIMEOUT,
        MAX_VERIFY_OUTPUT_BYTES,
        MAX_VERIFY_OUTPUT_BYTES,
        "ffmpeg -version",
    )
    .is_ok_and(|out| out.status.success())
}

/// Locate a usable ffmpeg binary: next to the current executable first
/// (bundled releases), then PATH. The result is cached for the lifetime of
/// the process in `FFMPEG_PATH` — the cached lookup skips both the PATH scan
/// and the `ffmpeg -version` verify spawn on every subsequent call.
pub fn find_ffmpeg() -> Option<PathBuf> {
    FFMPEG_PATH.get_or_init(find_ffmpeg_uncached).clone()
}

fn find_ffmpeg_uncached() -> Option<PathBuf> {
    let bundled = crate::process::find_executable_on_path(&[FFMPEG_EXE], true)?;
    if verify_ffmpeg(&bundled) {
        return Some(bundled);
    }

    // If a binary next to the executable failed verification, still try PATH.
    if let Some(on_path) = crate::process::find_executable_on_path(&[FFMPEG_EXE], false)
        && on_path != bundled
        && verify_ffmpeg(&on_path)
    {
        return Some(on_path);
    }

    None
}

/// Extract the first video frame, scaled to fit max_w×max_h (aspect preserved),
/// as RGBA. Uses `-vf scale=W:H:force_original_aspect_ratio=decrease,setsar=1`
/// piped as PNG (`-f image2pipe -vcodec png -`), decoded via the image crate.
pub fn extract_frame(path: &Path, max_w: u32, max_h: u32) -> Result<super::DecodedImage, String> {
    let ffmpeg = find_ffmpeg().ok_or_else(|| "ffmpeg not found".to_string())?;

    // Any argument starting with '-' is parsed as an ffmpeg option,
    // misaligning the whole argument list — a `-`-prefixed DIRECTORY
    // component in a relative path (e.g. `-evil/x.jpg`) is just as dangerous
    // as a file literally named `-`. The whole path is checked (not just the
    // file name) so any argument that could be parsed as an option is
    // rejected. The scanner only produces absolute paths, but the API is
    // guarded against direct misuse.
    if crate::process::path_starts_with_dash(path) {
        return Err(format!(
            "cannot extract frame: path {:?} starts with '-'",
            path
        ));
    }

    let vf = format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,setsar=1",
        max_w, max_h
    );

    // `-analyzeduration`/`-probesize` are pinned to explicit values rather
    // than relying on ffmpeg's built-in defaults, so a future ffmpeg release
    // changing its own defaults cannot silently change how far a demuxer is
    // scanned. The values match ffmpeg's current defaults — this is a
    // stability pin (explicit is better than implicit), not a behavior
    // change for legitimate media.
    let mut cmd = crate::process::new_hidden_command(&ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-analyzeduration",
        "5000000",
        "-probesize",
        "5000000",
        "-i",
    ])
    .arg(path)
    .args([
        "-frames:v",
        "1",
        "-vf",
        &vf,
        "-f",
        "image2pipe",
        "-vcodec",
        "png",
        "-",
    ]);

    let output = crate::process::run_command_capped(
        &mut cmd,
        FFMPEG_TIMEOUT,
        MAX_FFMPEG_STDOUT_BYTES,
        MAX_FFMPEG_STDERR_BYTES,
        "ffmpeg",
    )?;

    let stderr_tail = || {
        let s = String::from_utf8_lossy(&output.stderr);
        s.trim().chars().take(500).collect::<String>()
    };
    if !output.status.success() {
        return Err(format!(
            "ffmpeg exited with {}: {}",
            output.status,
            stderr_tail()
        ));
    }

    if output.stdout.is_empty() {
        return Err(format!("ffmpeg produced empty output: {}", stderr_tail()));
    }

    let img = decode_png_with_limits(&output.stdout)?;

    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(super::DecodedImage::new(w, h, rgba.into_raw()))
}

/// Decode PNG bytes from the ffmpeg pipe under the shared decode budget so a
/// hostile stream (e.g. a PNG bomb served by ffmpeg) cannot force oversized
/// allocations.
fn decode_png_with_limits(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    super::image_decoder::decode_bytes_with_limits(bytes).map_err(|e| format!("png decode: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::path_starts_with_dash;

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
        use std::os::unix::ffi::OsStringExt;
        // A raw path whose first byte is '-' but which is not valid UTF-8
        // must still be rejected (to_str() returns None, the raw-byte check
        // does not).
        let raw = vec![b'-', 0xFF, b'e'];
        assert!(path_starts_with_dash(Path::new(
            OsString::from_vec(raw).as_os_str()
        )));
        let raw = vec![0xFF, b'-'];
        assert!(!path_starts_with_dash(Path::new(
            OsString::from_vec(raw).as_os_str()
        )));
    }
}
