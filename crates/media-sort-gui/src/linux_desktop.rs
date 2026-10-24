//! Linux desktop integration for the window icon and Wayland app id.
//!
//! Wayland compositors never read the window icon from the client; they resolve
//! a toplevel's icon (and display name) through the desktop entry whose file
//! name matches the toplevel's `app_id`, which winit sets from the window's
//! `application_id`. An AppImage that was never "integrated" by appimaged,
//! AppImageLauncher or Gear Lever has no such entry in the XDG data
//! directories, so the shell falls back to a generic icon even though the
//! AppImage itself ships a `MediaSort.desktop` and the icon files.
//!
//! [`application_id`] closes that gap at startup:
//!
//! 0. Under Flatpak the runtime already exports a desktop entry named after the
//!    sandbox id (`FLATPAK_ID`, e.g. `app.mediasort.MediaSort`) with the icon
//!    alongside it, so that id is returned verbatim and nothing is provisioned.
//! 1. If a packaged or hand-written `MediaSort.desktop` (the canonical id)
//!    exists at any level, it wins: its name is used as the app id and nothing
//!    is written.
//! 2. Otherwise, if an integration tool already registered an entry that
//!    launches this very executable/AppImage (any tool, any file name), that
//!    entry's name is adopted and nothing is written.
//! 3. Otherwise a hidden (`NoDisplay=true`) entry plus the app icon are
//!    provisioned into the user's XDG data directories, under a name unique to
//!    this module ([`PROVISIONED_ID`]) so they can never collide with - or
//!    shadow - a packaged, distro- or tool-managed entry of the same app.
//!
//! The provisioned files are *kept* between runs: they are what gives the shell
//! the icon, and removing them at exit would only churn icon/menu caches and
//! break window grouping while the app is restarted. Because the name is
//! unique, a foreign file can never legitimately live at those paths, so a
//! collision is structurally impossible; if one is found there anyway it is
//! left untouched. The files are removed again only when a canonical
//! `MediaSort.desktop` (packaged or hand-written) appears and takes over.
//!
//! The entry is hidden so nothing appears in application menus; it exists only
//! to give the shell an icon, display name and `StartupWMClass` for the window.
//! Writes use [`atomic_write`], so a crash cannot leave a truncated entry
//! behind.
//!
//! Nothing is written on non-Wayland sessions: on X11 (and any other backend)
//! the icon is delivered by the window itself (`window::Settings.icon`), so
//! only the app id is set there - to [`DESKTOP_ID`], the canonical id that
//! matches the packaged/Gear Lever `StartupWMClass`.
//!
//! `xdg-toplevel-icon-v1` would remove the need for all of this, but the pinned
//! winit (0.30, via iced 0.14) cannot speak it; revisit when iced ships a newer
//! winit, at which point this module can be reduced to that protocol or removed.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use media_sort_core::path_utils::{atomic_write, paths_equal};

/// Canonical desktop file name (without extension) used by the packaged
/// releases (vpk `-u MediaSort`) and matched by their `StartupWMClass`. Used as
/// the window's `application_id` whenever a canonical entry exists or
/// provisioning is not possible.
pub const DESKTOP_ID: &str = "MediaSort";

/// Desktop file name (and icon name) this module provisions for itself.
///
/// It is deliberately *not* the canonical [`DESKTOP_ID`]: a self-provisioned
/// file then cannot collide with - or shadow - a packaged, distro- or
/// tool-managed entry of the same app, which is what makes it safe to leave the
/// files in place between runs.
const PROVISIONED_ID: &str = "mediasort-wayland";

/// Marks the desktop entry as provisioned by this module so a foreign file at
/// the same path is never overwritten or removed.
const MARKER_KEY: &str = "X-MediaSort-Provisioned";

/// The app icon installed into the user's `hicolor` icon theme: the same
/// scalable icon (`packaging/linux/icon.svg`) the AppImage ships, so the two
/// can never drift apart. GNOME (librsvg) and KDE (QtSvg) both render theme
/// SVGs natively - Breeze itself ships SVG-only icons.
const ICON: &[u8] = include_bytes!("../../../packaging/linux/icon.svg");

/// Desktop files are tiny; anything larger is not trusted to be one.
const MAX_DESKTOP_FILE_BYTES: u64 = 64 * 1024;

/// Resolves the window `application_id` and makes the app icon discoverable by
/// desktop shells when the usual integration is missing.
///
/// Provisioned files are left in place for the next run (that is what keeps the
/// icon working); a canonical `MediaSort.desktop` always wins over them.
/// `$APPIMAGE` (when running from an AppImage), `$XDG_DATA_HOME` and
/// `$XDG_DATA_DIRS` are read from the environment; failures to write are logged
/// and never prevent the app from starting.
pub fn application_id() -> String {
    // Under Flatpak the sandbox id is the name of the desktop entry the runtime
    // exports (and the icon it installs), so it must be used verbatim on every
    // backend: it is what the shell and the compositor match the window
    // against. Nothing needs to be provisioned, and the AppImage-style
    // provisioning below must not run (it would write into the app's private
    // data directory for no benefit).
    if let Some(id) = flatpak_app_id() {
        return id;
    }

    // Wayland is the only backend where the compositor resolves the window
    // icon through the desktop entry; on X11 (and any other backend) the icon
    // is delivered by the window itself, so provisioning there would only
    // leave a permanent footprint for nothing.
    if !is_wayland() {
        return DESKTOP_ID.to_string();
    }

    let data_home = dirs::data_local_dir();
    let appdir = std::env::var_os("APPDIR").map(PathBuf::from);
    // Entries bundled inside the running AppImage's own mount must never be
    // mistaken for a packaged/system entry: the compositor is a separate
    // process and cannot see the mount, so treating them as "already
    // registered" would silently disable provisioning. (AppImageKit's default
    // `AppRun` prepends `$APPDIR/usr/share` to `$XDG_DATA_DIRS`.)
    let data_dirs: Vec<PathBuf> = xdg_data_dirs()
        .into_iter()
        .filter(|dir| !is_inside(appdir.as_deref(), dir))
        .collect();
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let exe = std::env::current_exe().ok();

    resolve_and_provision(
        data_home.as_deref(),
        &data_dirs,
        appimage.as_deref(),
        exe.as_deref(),
    )
}

/// The application id Flatpak assigned to this sandbox, or `None` when not
/// running under Flatpak.
///
/// `flatpak run` sets `FLATPAK_ID` to the manifest's `app-id`
/// (`app.mediasort.MediaSort`, see `flatpak/app.mediasort.MediaSort.yml`),
/// which is exactly the name of the desktop entry and icon the runtime exports
/// into the sandbox - the value the shell needs for window grouping, the icon
/// and the app name.
pub fn flatpak_app_id() -> Option<String> {
    flatpak_id_from(std::env::var_os("FLATPAK_ID").as_deref())
}

/// The pure predicate behind [`flatpak_app_id`], so it can be tested without
/// touching the process environment.
fn flatpak_id_from(value: Option<&std::ffi::OsStr>) -> Option<String> {
    value
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// True when this process is running under Wayland. `WAYLAND_DISPLAY` is set by
/// the session for Wayland clients and is the only signal available before the
/// window (and its backend) exists. An XWayland client that nevertheless runs
/// as X11 would merely get a harmless extra entry.
fn is_wayland() -> bool {
    is_wayland_session(std::env::var_os("WAYLAND_DISPLAY").as_deref())
}

/// The pure predicate behind [`is_wayland`], so it can be tested without
/// touching the process environment.
fn is_wayland_session(wayland_display: Option<&std::ffi::OsStr>) -> bool {
    wayland_display.is_some_and(|value| !value.is_empty())
}

/// True when `dir` is `appdir` itself or nested inside it (component-wise, so
/// `/x` does not contain `/xy`).
fn is_inside(appdir: Option<&Path>, dir: &Path) -> bool {
    appdir.is_some_and(|appdir| dir.starts_with(appdir))
}

/// The platform-independent heart of [`application_id`], split out so tests can
/// drive it with temporary directories.
fn resolve_and_provision(
    data_home: Option<&Path>,
    data_dirs: &[PathBuf],
    appimage: Option<&Path>,
    exe: Option<&Path>,
) -> String {
    let canonical = format!("{DESKTOP_ID}.desktop");
    let user_canonical = data_home.map(|home| home.join("applications").join(&canonical));
    // A real (packaged or hand-written) canonical entry wins over anything we
    // would write, at either level.
    let foreign_canonical = data_dirs
        .iter()
        .any(|dir| dir.join("applications").join(&canonical).is_file())
        || user_canonical
            .as_deref()
            .is_some_and(|entry| entry.is_file() && !is_managed_by_us(entry));

    if foreign_canonical {
        // Our provisioned files would sit unused next to a canonical entry;
        // drop them so the canonical entry's icon and running state are used.
        if let Some(data_home) = data_home {
            remove_provisioned(data_home);
        }
        return DESKTOP_ID.to_string();
    }

    // A canonical entry this module wrote under an earlier revision (before the
    // name became unique) is migrated out of the way.
    if let Some(data_home) = data_home
        && user_canonical
            .as_deref()
            .is_some_and(|entry| entry.is_file())
    {
        remove_provisioned(data_home);
    }

    // Adopt an existing integration entry that launches this very file. This
    // covers appimaged (`appimagekit-*.desktop`), AppImageLauncher, Gear Lever,
    // `xdg-desktop-menu install` and user-created entries alike, without having
    // to guess any naming scheme - for an AppImage as well as a plain binary
    // (e.g. a hand-written entry for a development build). Our own provisioned
    // entry is skipped so it is never mistaken for an external integration (and
    // so a stale one is refreshed rather than adopted).
    if let Some(target) = appimage.or(exe)
        && let Some(id) = find_entry_for_appimage(data_home, data_dirs, target)
    {
        tracing::debug!(desktop_id = %id, "adopted existing desktop entry for the running executable");
        return id;
    }

    let Some(data_home) = data_home else {
        return DESKTOP_ID.to_string();
    };
    let Some(target) = appimage.or(exe) else {
        return DESKTOP_ID.to_string();
    };
    let Some(exec) = exec_argument(target) else {
        tracing::debug!("skipping desktop entry provisioning: path cannot be represented safely");
        return DESKTOP_ID.to_string();
    };

    match provision(data_home, &exec) {
        Ok(true) => PROVISIONED_ID.to_string(),
        // A foreign file occupies our path; leave it and fall back to the
        // canonical id (we do not own an entry under the provisioned name).
        Ok(false) => DESKTOP_ID.to_string(),
        Err(err) => {
            tracing::warn!(%err, "could not provision desktop entry for the window icon");
            DESKTOP_ID.to_string()
        }
    }
}

/// Looks for a `*.desktop` file in the XDG application directories whose
/// `Exec`/`TryExec` launches `appimage`; returns its file name (the desktop
/// entry id the compositor will use). Our own provisioned entry is ignored.
fn find_entry_for_appimage(
    data_home: Option<&Path>,
    data_dirs: &[PathBuf],
    appimage: &Path,
) -> Option<String> {
    let own_file = format!("{PROVISIONED_ID}.desktop");
    let mut applications_dirs: Vec<PathBuf> = Vec::new();
    if let Some(data_home) = data_home {
        applications_dirs.push(data_home.join("applications"));
    }
    applications_dirs.extend(data_dirs.iter().map(|dir| dir.join("applications")));

    for dir in applications_dirs {
        for file in desktop_files(&dir) {
            if file
                .file_name()
                .is_some_and(|name| name == own_file.as_str())
            {
                continue;
            }
            let Some(contents) = read_capped(&file) else {
                continue;
            };
            if exec_matches(&contents, appimage)
                && let Some(id) = file.file_stem().and_then(|stem| stem.to_str())
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// `*.desktop` files directly inside `dir`, sorted for deterministic results.
fn desktop_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "desktop"))
        .collect();
    files.sort();
    files
}

/// Reads a file only when it is small enough to plausibly be a desktop entry.
fn read_capped(path: &Path) -> Option<String> {
    if fs::metadata(path).ok()?.len() > MAX_DESKTOP_FILE_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// True when any `Exec`/`TryExec` entry in the desktop file refers to `target`.
fn exec_matches(contents: &str, target: &Path) -> bool {
    contents.lines().any(|line| {
        candidate_paths(line).iter().any(|candidate| {
            let candidate = Path::new(candidate);
            candidate.is_absolute() && paths_equal(candidate, target)
        })
    })
}

/// Extracts the executable paths a desktop-file line may contain. `Exec` is a
/// command line (tokenized with quoting/escaping); `TryExec` is a single path
/// string (where `\s` encodes a space, as written by AppImageKit's integration
/// script).
fn candidate_paths(line: &str) -> Vec<String> {
    // A key-file tolerates leading whitespace before a key.
    let line = line.trim_start();
    if let Some(value) = line.strip_prefix("Exec=") {
        exec_tokens(value)
    } else if let Some(value) = line.strip_prefix("TryExec=") {
        vec![unescape_value(value.trim().trim_matches('"'))]
    } else {
        Vec::new()
    }
}

/// Splits an `Exec` value into arguments, honoring double quotes and the
/// `\s`/`\n`/`\t`/`\r`/`\\` escapes used by desktop-file config parsers.
fn exec_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                let escaped = match chars.next() {
                    Some('s') => ' ',
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('r') => '\r',
                    Some(other) => other,
                    None => '\\',
                };
                current.push(escaped);
                has_token = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            _ if ch.is_whitespace() && !in_quotes => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            _ => {
                current.push(ch);
                has_token = true;
            }
        }
    }
    if has_token {
        tokens.push(current);
    }
    tokens
}

/// Unescapes a desktop-file value string (`\s` is a space, and so on).
fn unescape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        out.push(match chars.next() {
            Some('s') => ' ',
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some(other) => other,
            None => '\\',
        });
    }
    out
}

/// True when the desktop entry was written by this module.
fn is_managed_by_us(path: &Path) -> bool {
    read_capped(path).is_some_and(|contents| {
        contents.lines().any(|line| {
            line.trim_start()
                .strip_prefix(MARKER_KEY)
                .and_then(|rest| rest.strip_prefix('='))
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
        })
    })
}

/// The icon file this module installs into the user's theme.
fn icon_path(data_home: &Path) -> PathBuf {
    data_home
        .join("icons")
        .join("hicolor")
        .join("scalable")
        .join("apps")
        .join(format!("{PROVISIONED_ID}.svg"))
}

/// Writes the hidden desktop entry and the app icon below `data_home`
/// (`$XDG_DATA_HOME`, i.e. `~/.local/share`), refreshing them when they differ.
///
/// Returns `false` without touching anything when a foreign (unmarked) file
/// already occupies the provisioned entry path. The entry and icon names are
/// unique to this module, so a foreign file living at them is not expected;
/// the check is defence in depth, and no separate guard is needed for the icon
/// (a legitimate package or tool would never install a file under that name).
fn provision(data_home: &Path, exec: &str) -> io::Result<bool> {
    let applications = data_home.join("applications");
    let entry = applications.join(format!("{PROVISIONED_ID}.desktop"));
    if entry.is_file() && !is_managed_by_us(&entry) {
        tracing::debug!(path = %entry.display(), "not overwriting a foreign file at the provisioned entry path");
        return Ok(false);
    }

    // Write the icon before the entry: if the icon fails, no marked entry is
    // left behind pointing at an icon that does not exist (which would make the
    // returned app id match neither the entry nor the icon).
    let icon = icon_path(data_home);
    if let Some(icon_dir) = icon.parent() {
        fs::create_dir_all(icon_dir)?;
    }
    write_if_changed(&icon, ICON)?;

    fs::create_dir_all(&applications)?;
    write_if_changed(&entry, desktop_entry_contents(exec).as_bytes())?;
    Ok(true)
}

/// Removes the files this module provisioned. Only files it owns are touched:
/// the entry must carry the marker and the icon must be byte-for-byte the one
/// installed here (a user-replaced icon is kept). Both the uniquely-named entry
/// and one written under the canonical name by an earlier revision are removed.
fn remove_provisioned(data_home: &Path) {
    let applications = data_home.join("applications");
    for name in [
        format!("{PROVISIONED_ID}.desktop"),
        format!("{DESKTOP_ID}.desktop"),
    ] {
        let entry = applications.join(name);
        if entry.is_file()
            && is_managed_by_us(&entry)
            && let Err(err) = fs::remove_file(&entry)
        {
            tracing::debug!(path = %entry.display(), %err, "could not remove provisioned desktop entry");
        }
    }

    let icon = icon_path(data_home);
    if read_capped_bytes(&icon, ICON.len() as u64 + 1).is_ok_and(|bytes| bytes == ICON) {
        let _ = fs::remove_file(&icon);
    }
}

/// The provisioned desktop entry. `NoDisplay=true` keeps it out of application
/// menus; shells still use it for window/icon matching. `StartupWMClass` and
/// `Icon` both use [`PROVISIONED_ID`] so they match the window's app id and the
/// installed icon.
fn desktop_entry_contents(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Media Sort\n\
         Exec={exec} %U\n\
         Icon={PROVISIONED_ID}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         StartupWMClass={PROVISIONED_ID}\n\
         Categories=Utility;\n\
         {MARKER_KEY}=true\n"
    )
}

/// Formats `path` as a single, quoted `Exec` argument.
///
/// The value passes through two parsers: the key-file layer that reads the
/// value (`\\`, `\s`, `\n`, `\t`, `\r` are its only backslash escapes - any
/// other `\x` makes the whole file unreadable for GLib) and the shell-style
/// tokenizer that splits `Exec`. `"` and `\` are therefore escaped twice:
/// once for the shell layer, once for the key-file layer.
///
/// Paths containing `%`, `$`, backticks or control characters are rejected.
/// Control characters would corrupt the file. `%` has no portable encoding
/// either: doubling it to `%%` passes `desktop-file-validate`, but GLib's
/// load-time check resolves the *raw* `argv[0]` (including the unexpanded
/// `%%`) against the filesystem and discards the entry, while a literal `%`
/// is an invalid field code for the validator. `$` and backticks cannot be
/// escaped portably: GLib's key-file layer rejects `\$`/`` \` `` outright,
/// KDE's parser keeps the backslash inside quotes literally, and leaving them
/// unescaped fails `desktop-file-validate`. Such paths keep the system's
/// default icon.
fn exec_argument(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    if path.is_empty()
        || path
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '%' | '$' | '`'))
    {
        return None;
    }

    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for ch in path.chars() {
        match ch {
            // Shell layer `\"`, plus one more backslash for the key-file layer.
            '"' => quoted.push_str(r#"\\""#),
            // Shell layer `\\`, plus one more backslash per backslash.
            '\\' => quoted.push_str(r"\\\\"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    Some(quoted)
}

/// Reads at most `limit` bytes of `path`. Used to compare an existing file
/// against (small) expected contents without ever reading a large or hostile
/// file whole.
fn read_capped_bytes(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Writes `contents` to `path` only when they differ, so repeated startups do
/// not touch mtimes (icon themes watch these directories).
fn write_if_changed(path: &Path, contents: &[u8]) -> io::Result<()> {
    // Read one byte past `contents`, so a longer existing file can never
    // compare equal and is never read whole.
    let limit = contents.len() as u64 + 1;
    if read_capped_bytes(path, limit).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    atomic_write(path, contents)
}

/// `$XDG_DATA_DIRS` with the spec default, filtered to absolute paths.
fn xdg_data_dirs() -> Vec<PathBuf> {
    let from_env = std::env::var_os("XDG_DATA_DIRS").filter(|value| !value.is_empty());
    parse_data_dirs(from_env.as_deref())
}

/// Splits an `$XDG_DATA_DIRS` value; `None` uses the spec default. Relative
/// entries are ignored, as required by the XDG base directory specification.
fn parse_data_dirs(value: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = match value {
        Some(value) => std::env::split_paths(value).collect(),
        None => vec![
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ],
    };
    dirs.into_iter().filter(|dir| dir.is_absolute()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary `$XDG_DATA_HOME` that cleans up after itself.
    struct TempDataHome(PathBuf);

    impl TempDataHome {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "media-sort-linux-desktop-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create temp data home");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn entry(&self) -> PathBuf {
            self.0
                .join("applications")
                .join(format!("{PROVISIONED_ID}.desktop"))
        }
    }

    impl Drop for TempDataHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_file(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, contents).expect("write file");
    }

    fn installed_entry_for(appimage: &Path, exec_key: &str) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=Media Sort\n{exec_key}={} %U\n",
            appimage.display()
        )
    }

    #[test]
    fn adopts_a_desktop_entry_that_launches_the_appimage() {
        let home = TempDataHome::new("adopt");
        let appimage = home.path().join("Foo.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");
        write_file(
            &home.path().join("applications/appimagekit-Foo.desktop"),
            &installed_entry_for(&appimage, "Exec"),
        );

        let id = resolve_and_provision(Some(home.path()), &[], Some(&appimage), None);

        assert_eq!(id, "appimagekit-Foo");
        assert!(!home.entry().exists());
    }

    #[test]
    fn adopts_a_desktop_entry_that_launches_the_appimage_via_an_env_wrapper() {
        let home = TempDataHome::new("env-wrapper");
        let appimage = home.path().join("Foo.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");
        write_file(
            &home.path().join("applications/custom-name.desktop"),
            &format!(
                "[Desktop Entry]\nExec=env APPIMAGELAUNCHER_DISABLE=1 \"{}\" %U\n",
                appimage.display()
            ),
        );

        let id = resolve_and_provision(Some(home.path()), &[], Some(&appimage), None);

        assert_eq!(id, "custom-name");
    }

    #[test]
    fn adopts_a_try_exec_entry_with_escaped_spaces() {
        let home = TempDataHome::new("tryexec");
        let subdir = home.path().join("My Apps");
        fs::create_dir_all(&subdir).expect("create subdir");
        let appimage = subdir.join("Foo.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");
        let escaped = appimage.display().to_string().replace(' ', "\\s");
        write_file(
            &home.path().join("applications/foo.desktop"),
            &format!("[Desktop Entry]\nTryExec={escaped}\n"),
        );

        let id = resolve_and_provision(Some(home.path()), &[], Some(&appimage), None);

        assert_eq!(id, "foo");
    }

    #[test]
    fn adopts_an_entry_matching_a_symlinked_appimage() {
        let home = TempDataHome::new("symlink");
        let real = home.path().join("real.AppImage");
        fs::write(&real, b"appimage").expect("write appimage");
        let link = home.path().join("link.AppImage");
        std::os::unix::fs::symlink(&real, &link).expect("create symlink");
        write_file(
            &home.path().join("applications/foo.desktop"),
            &installed_entry_for(&real, "Exec"),
        );

        let id = resolve_and_provision(Some(home.path()), &[], Some(&link), None);

        assert_eq!(id, "foo");
    }

    #[test]
    fn adopts_an_entry_that_launches_the_plain_executable() {
        let home = TempDataHome::new("adopt-exe");
        let exe = home.path().join("target/debug/media-sort-gui");
        write_file(
            &home.path().join("applications/dev.desktop"),
            &installed_entry_for(&exe, "Exec"),
        );

        let id = resolve_and_provision(Some(home.path()), &[], None, Some(&exe));

        assert_eq!(id, "dev");
        assert!(!home.entry().exists());
    }

    #[test]
    fn adopts_an_entry_with_an_indented_exec() {
        let home = TempDataHome::new("indented");
        let appimage = home.path().join("Foo.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");
        // A key-file tolerates leading whitespace before a key.
        write_file(
            &home.path().join("applications/foo.desktop"),
            &format!("[Desktop Entry]\n  Exec=\"{}\" %U\n", appimage.display()),
        );

        let id = resolve_and_provision(Some(home.path()), &[], Some(&appimage), None);

        assert_eq!(id, "foo");
    }

    #[test]
    fn provisions_a_hidden_entry_and_icon_when_not_integrated() {
        let home = TempDataHome::new("provision");
        let appimage = home.path().join("MediaSort-linux-x64.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");

        let id = resolve_and_provision(Some(home.path()), &[], Some(&appimage), None);

        assert_eq!(id, PROVISIONED_ID);
        assert_ne!(PROVISIONED_ID, DESKTOP_ID);
        let contents = fs::read_to_string(home.entry()).unwrap();
        assert!(contents.contains(&format!("Exec=\"{}\" %U", appimage.display())));
        assert!(contents.contains(&format!("Icon={PROVISIONED_ID}")));
        assert!(contents.contains("NoDisplay=true"));
        assert!(contents.contains(&format!("StartupWMClass={PROVISIONED_ID}")));
        assert!(contents.contains("X-MediaSort-Provisioned=true"));
        assert!(icon_path(home.path()).is_file());
        // Nothing is written under the canonical name.
        assert!(!home.path().join("applications/MediaSort.desktop").exists());
    }

    #[test]
    fn provisions_for_the_plain_executable_without_an_appimage() {
        let home = TempDataHome::new("plain-exe");
        let exe = home.path().join("target/debug/media-sort-gui");

        let id = resolve_and_provision(Some(home.path()), &[], None, Some(&exe));

        assert_eq!(id, PROVISIONED_ID);
        let contents = fs::read_to_string(home.entry()).unwrap();
        assert!(contents.contains(&format!("Exec=\"{}\" %U", exe.display())));
    }

    #[test]
    fn refreshes_the_provisioned_entry_when_the_executable_moves() {
        let home = TempDataHome::new("refresh");
        let old = home.path().join("v1/MediaSort.AppImage");
        let new = home.path().join("v2/MediaSort.AppImage");
        write_file(&old, "old");
        write_file(&new, "new");

        assert_eq!(
            resolve_and_provision(Some(home.path()), &[], Some(&old), None),
            PROVISIONED_ID
        );
        assert_eq!(
            resolve_and_provision(Some(home.path()), &[], Some(&new), None),
            PROVISIONED_ID
        );

        let contents = fs::read_to_string(home.entry()).unwrap();
        assert!(contents.contains(&format!("Exec=\"{}\" %U", new.display())));
        assert!(!contents.contains(&format!("Exec=\"{}\" %U", old.display())));
    }

    #[test]
    fn keeps_the_provisioned_files_between_runs_and_repairs_them() {
        let home = TempDataHome::new("persistent");
        let appimage = home.path().join("MediaSort.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");

        assert_eq!(
            resolve_and_provision(Some(home.path()), &[], Some(&appimage), None),
            PROVISIONED_ID
        );
        // A second run neither adopts its own entry nor drops the files.
        assert_eq!(
            resolve_and_provision(Some(home.path()), &[], Some(&appimage), None),
            PROVISIONED_ID
        );
        assert!(home.entry().exists());
        assert!(icon_path(home.path()).is_file());

        // Missing files are repaired on the next run (there is no cleanup on
        // exit any more).
        fs::remove_file(icon_path(home.path())).unwrap();
        assert_eq!(
            resolve_and_provision(Some(home.path()), &[], Some(&appimage), None),
            PROVISIONED_ID
        );
        assert!(icon_path(home.path()).is_file());
    }

    #[test]
    fn does_not_adopt_its_own_provisioned_entry() {
        let home = TempDataHome::new("skip-own");
        let appimage = home.path().join("Foo.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");
        assert_eq!(
            resolve_and_provision(Some(home.path()), &[], Some(&appimage), None),
            PROVISIONED_ID
        );

        // A foreign entry that sorts *after* ours must still be adopted: our
        // own entry is skipped rather than chosen.
        write_file(
            &home.path().join("applications/zzz.desktop"),
            &installed_entry_for(&appimage, "Exec"),
        );
        assert_eq!(
            resolve_and_provision(Some(home.path()), &[], Some(&appimage), None),
            "zzz"
        );
    }

    #[test]
    fn migrates_an_entry_previously_written_under_the_canonical_name() {
        let home = TempDataHome::new("migrate");
        let appimage = home.path().join("MediaSort.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");
        let old = home.path().join("applications/MediaSort.desktop");
        write_file(&old, &desktop_entry_contents("\"/old/media-sort\""));

        let id = resolve_and_provision(Some(home.path()), &[], Some(&appimage), None);

        assert_eq!(id, PROVISIONED_ID);
        assert!(!old.exists());
        assert!(home.entry().is_file());
    }

    #[test]
    fn does_not_shadow_a_packaged_desktop_entry() {
        let home = TempDataHome::new("shadow-system");
        let system_data = home.path().join("system-data");
        write_file(
            &system_data.join("applications/MediaSort.desktop"),
            "[Desktop Entry]\nType=Application\nName=Media Sort\nExec=media-sort-gui\nIcon=MediaSort\n",
        );
        let appimage = home.path().join("MediaSort-linux-x64.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");

        let id = resolve_and_provision(Some(home.path()), &[system_data], Some(&appimage), None);

        assert_eq!(id, DESKTOP_ID);
        assert!(!home.entry().exists());
    }

    #[test]
    fn leaves_a_hand_written_user_entry_alone() {
        let home = TempDataHome::new("user-entry");
        let user_entry = home.path().join("applications/MediaSort.desktop");
        let custom = "[Desktop Entry]\nType=Application\nName=Custom\nExec=/opt/custom\n";
        write_file(&user_entry, custom);

        let id = resolve_and_provision(Some(home.path()), &[], None, None);

        assert_eq!(id, DESKTOP_ID);
        assert_eq!(fs::read_to_string(&user_entry).unwrap(), custom);
        assert!(!home.entry().exists());
        assert!(!home.path().join("icons").exists());
    }

    #[test]
    fn does_not_overwrite_a_foreign_file_at_the_provisioned_entry_path() {
        let home = TempDataHome::new("foreign-at-own-path");
        let entry = home.entry();
        let custom = "[Desktop Entry]\nType=Application\nName=Custom\nExec=/opt/custom\n";
        write_file(&entry, custom);
        let appimage = home.path().join("MediaSort.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");

        let id = resolve_and_provision(Some(home.path()), &[], Some(&appimage), None);

        assert_eq!(id, DESKTOP_ID);
        assert_eq!(fs::read_to_string(&entry).unwrap(), custom);
        assert!(!icon_path(home.path()).exists());
    }

    #[test]
    fn removes_its_own_provisioned_files_once_a_packaged_one_appears() {
        let home = TempDataHome::new("heal-shadow");
        let appimage = home.path().join("MediaSort-linux-x64.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");
        assert_eq!(
            resolve_and_provision(Some(home.path()), &[], Some(&appimage), None),
            PROVISIONED_ID
        );
        assert!(home.entry().exists());

        let system_data = home.path().join("system-data");
        write_file(
            &system_data.join("applications/MediaSort.desktop"),
            "[Desktop Entry]\nType=Application\nName=Media Sort\nExec=media-sort-gui\n",
        );

        let id = resolve_and_provision(Some(home.path()), &[system_data], Some(&appimage), None);

        assert_eq!(id, DESKTOP_ID);
        assert!(!home.entry().exists());
        assert!(!icon_path(home.path()).exists());
    }

    #[test]
    fn keeps_a_user_replaced_icon_when_a_canonical_entry_appears() {
        let home = TempDataHome::new("keep-icon");
        let appimage = home.path().join("MediaSort.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");
        assert_eq!(
            resolve_and_provision(Some(home.path()), &[], Some(&appimage), None),
            PROVISIONED_ID
        );
        write_file(&icon_path(home.path()), "user replacement");

        let system_data = home.path().join("system-data");
        write_file(
            &system_data.join("applications/MediaSort.desktop"),
            "[Desktop Entry]\nType=Application\nName=Media Sort\nExec=media-sort-gui\n",
        );
        let id = resolve_and_provision(Some(home.path()), &[system_data], Some(&appimage), None);

        assert_eq!(id, DESKTOP_ID);
        assert!(!home.entry().exists());
        assert!(
            icon_path(home.path()).exists(),
            "a user-replaced icon must be kept"
        );
    }

    #[test]
    fn skips_provisioning_when_no_executable_path_is_available() {
        let home = TempDataHome::new("no-target");

        let id = resolve_and_provision(Some(home.path()), &[], None, None);

        assert_eq!(id, DESKTOP_ID);
        assert!(!home.entry().exists());
    }

    #[test]
    fn returns_the_constant_id_without_a_data_home() {
        let home = TempDataHome::new("no-data-home");
        let appimage = home.path().join("MediaSort.AppImage");

        let id = resolve_and_provision(None, &[], Some(&appimage), None);

        assert_eq!(id, DESKTOP_ID);
    }

    #[test]
    fn ignores_desktop_entries_for_other_executables() {
        let home = TempDataHome::new("other-exe");
        write_file(
            &home.path().join("applications/other.desktop"),
            "[Desktop Entry]\nExec=\"/opt/other/App.AppImage\" %U\n",
        );
        let appimage = home.path().join("MediaSort.AppImage");
        fs::write(&appimage, b"appimage").expect("write appimage");

        let id = resolve_and_provision(Some(home.path()), &[], Some(&appimage), None);

        assert_eq!(id, PROVISIONED_ID);
        assert!(home.entry().exists());
    }

    #[test]
    fn exec_tokens_handles_quoting_and_escapes() {
        assert_eq!(
            exec_tokens(r#"env FOO=bar "/srv/My Apps/Foo.AppImage" %U"#),
            vec!["env", "FOO=bar", "/srv/My Apps/Foo.AppImage", "%U"]
        );
        assert_eq!(
            exec_tokens(r"/srv/My\sApps/Foo.AppImage %U"),
            vec!["/srv/My Apps/Foo.AppImage", "%U"]
        );
        assert_eq!(
            exec_tokens(r#""/srv/quoted\"path/x" u"#),
            vec!["/srv/quoted\"path/x", "u"]
        );
    }

    #[test]
    fn exec_argument_quotes_and_escapes_special_characters() {
        // `/tmp/100 junk/My "App" \z` becomes, on disk:
        //   - `"` -> `\\"`  (shell `\"` + one backslash for the key-file layer)
        //   - `\` -> `\\\\` (shell `\\` + one backslash per backslash)
        assert_eq!(
            exec_argument(Path::new(r#"/tmp/100 junk/My "App" \z"#)).unwrap(),
            r#""/tmp/100 junk/My \\"App\\" \\\\z""#
        );
        assert_eq!(
            exec_argument(Path::new("/tmp/plain.AppImage")).unwrap(),
            "\"/tmp/plain.AppImage\""
        );
    }

    #[test]
    fn exec_argument_rejects_unrepresentable_characters() {
        assert_eq!(exec_argument(Path::new("")), None);
        assert_eq!(exec_argument(Path::new("/tmp/line\nbreak.AppImage")), None);
        assert_eq!(exec_argument(Path::new("/tmp/tab\tname.AppImage")), None);
        // No encoding of these is accepted by desktop-file-validate, GLib and
        // KDE at the same time (see `exec_argument`).
        assert_eq!(
            exec_argument(Path::new("/tmp/percent%20name.AppImage")),
            None
        );
        assert_eq!(exec_argument(Path::new("/tmp/dollar$name.AppImage")), None);
        assert_eq!(exec_argument(Path::new("/tmp/back`tick.AppImage")), None);
    }

    #[test]
    fn data_dirs_use_spec_defaults_and_drop_relative_paths() {
        use std::ffi::OsStr;

        assert_eq!(
            parse_data_dirs(None),
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share")
            ]
        );
        assert_eq!(
            parse_data_dirs(Some(OsStr::new("/opt/share:relative:/usr/share"))),
            vec![PathBuf::from("/opt/share"), PathBuf::from("/usr/share")]
        );
    }

    #[test]
    fn appdir_mount_is_excluded_from_the_data_dirs() {
        assert!(is_inside(
            Some(Path::new("/tmp/.mount_abc")),
            Path::new("/tmp/.mount_abc/usr/share")
        ));
        assert!(!is_inside(
            Some(Path::new("/tmp/.mount_abc")),
            Path::new("/tmp/.mount_abcd/usr/share")
        ));
        assert!(!is_inside(None, Path::new("/usr/share")));
    }

    #[test]
    fn wayland_session_detection() {
        use std::ffi::OsStr;

        assert!(is_wayland_session(Some(OsStr::new("wayland-0"))));
        assert!(!is_wayland_session(Some(OsStr::new(""))));
        assert!(!is_wayland_session(None));
    }

    #[test]
    fn flatpak_id_comes_from_the_environment() {
        use std::ffi::OsStr;

        assert_eq!(
            flatpak_id_from(Some(OsStr::new("app.mediasort.MediaSort"))).as_deref(),
            Some("app.mediasort.MediaSort")
        );
        // A variable that is set but empty/unusable must not become the app id.
        assert_eq!(flatpak_id_from(Some(OsStr::new("  "))), None);
        assert_eq!(flatpak_id_from(Some(OsStr::new(""))), None);
        assert_eq!(flatpak_id_from(None), None);
    }
}
