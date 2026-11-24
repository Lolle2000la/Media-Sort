use std::ffi::{CStr, CString};
#[cfg(unix)]
use std::ffi::{c_char, c_int, c_void};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

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

pub(super) fn candidate_library_names() -> &'static [&'static str] {
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

pub(super) fn candidate_install_dirs_from_soffice(soffice: Option<&Path>) -> Vec<PathBuf> {
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

pub(super) fn resolve_candidate_library_paths(
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

pub(super) fn c_str_from_path(path: &Path) -> Result<CString, String> {
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

/// Dynamic library handle kept intentionally mapped until process exit.
///
/// After LibreOfficeKit initializes, LibreOffice spawns internal UNO/sal threads and registers
/// static destructors; unmapping the shared library via `dlclose`/`FreeLibrary` before process
/// exit is unsafe and can crash on dangling thread or `atexit` callbacks.
pub(super) struct LoadedDynamicLib {
    _lib: ManuallyDrop<DynamicLib>,
}

/// RAII handle to an OS dynamic library that automatically unloads on drop unless promoted to
/// [`LoadedDynamicLib`] via [`DynamicLib::keep_loaded`].
#[cfg(unix)]
pub(super) struct DynamicLib {
    handle: *mut c_void,
}

#[cfg(unix)]
impl DynamicLib {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
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

    pub(super) unsafe fn get_symbol<T>(&self, name: &CStr) -> Option<T> {
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
}

#[cfg(unix)]
impl Drop for DynamicLib {
    fn drop(&mut self) {
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

        // SAFETY: `self.handle` was returned by `dlopen` and was not promoted via `keep_loaded()`.
        unsafe {
            let _ = dlclose(self.handle);
        }
    }
}

/// RAII handle to an OS dynamic library that automatically unloads on drop unless promoted to
/// [`LoadedDynamicLib`] via [`DynamicLib::keep_loaded`].
#[cfg(target_os = "windows")]
pub(super) struct DynamicLib {
    handle: windows::Win32::Foundation::HMODULE,
}

#[cfg(target_os = "windows")]
impl DynamicLib {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
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

    pub(super) unsafe fn get_symbol<T>(&self, name: &CStr) -> Option<T> {
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
}

#[cfg(target_os = "windows")]
impl Drop for DynamicLib {
    fn drop(&mut self) {
        use windows::Win32::Foundation::FreeLibrary;

        // SAFETY: `self.handle` was returned by `LoadLibraryExW` and was not promoted via
        // `keep_loaded()`.
        unsafe {
            let _ = FreeLibrary(self.handle);
        }
    }
}

/// RAII handle to an OS dynamic library on unsupported targets.
#[cfg(not(any(unix, target_os = "windows")))]
pub(super) struct DynamicLib;

#[cfg(not(any(unix, target_os = "windows")))]
impl DynamicLib {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        Err(format!(
            "Dynamic LibreOfficeKit loading is unsupported on this OS ({})",
            path.display()
        ))
    }

    pub(super) unsafe fn get_symbol<T>(&self, _name: &CStr) -> Option<T> {
        None
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
impl Drop for DynamicLib {
    fn drop(&mut self) {}
}

impl DynamicLib {
    /// Promotes this handle to a permanently mapped [`LoadedDynamicLib`] after LOK initialization.
    pub(super) fn keep_loaded(self) -> LoadedDynamicLib {
        // SAFETY: LibreOfficeKit has initialized successfully; LibreOffice UNO/sal threads and
        // static destructors are unsafe to unmap via `dlclose`/`FreeLibrary` before process exit,
        // so wrapping in `ManuallyDrop` suppresses `DynamicLib::drop` for process lifetime.
        LoadedDynamicLib {
            _lib: ManuallyDrop::new(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::document::soffice_pipe::ManagedTempDir;

    #[test]
    fn candidate_library_resolution_follows_symlinked_soffice() {
        let guard =
            ManagedTempDir::create_in_temp("ms-lok-dynlib-test").expect("create temp test dir");
        let bin_dir = guard.path().join("bin");
        let program_dir = guard.path().join("libreoffice").join("program");
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
    fn dynamic_lib_open_missing_returns_err() {
        let guard =
            ManagedTempDir::create_in_temp("ms-lok-missing-lib").expect("create temp test dir");
        let missing = guard.path().join("nonexistent-libsofficeapp.so");
        assert!(DynamicLib::open(&missing).is_err());
    }
}
