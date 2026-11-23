use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;

use super::dynlib::{
    DynamicLib, LoadedDynamicLib, c_str_from_path, candidate_install_dirs_from_soffice,
    candidate_library_names, resolve_candidate_library_paths,
};
use super::protocol::build_page_range_filter_options;
use crate::media::document::soffice_pipe::find_soffice;
use crate::media::document::{
    DocumentError, MAX_PDF_FILE_BYTES, canonical_path_to_file_url, path_to_file_url,
};

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

type LokDestroyFn = Option<unsafe extern "C" fn(p_this: *mut LibreOfficeKit)>;
type LokDocLoadFn = Option<
    unsafe extern "C" fn(
        p_this: *mut LibreOfficeKit,
        p_url: *const c_char,
    ) -> *mut LibreOfficeKitDocument,
>;
type LokGetErrorFn = Option<unsafe extern "C" fn(p_this: *mut LibreOfficeKit) -> *mut c_char>;
type LokDocDestroyFn = Option<unsafe extern "C" fn(p_this: *mut LibreOfficeKitDocument)>;
type LokDocSaveAsFn = Option<
    unsafe extern "C" fn(
        p_this: *mut LibreOfficeKitDocument,
        p_url: *const c_char,
        p_format: *const c_char,
        p_filter_options: *const c_char,
    ) -> c_int,
>;

const fn has_vtable_member<T>(n_size: usize, offset: usize) -> bool {
    match offset.checked_add(std::mem::size_of::<T>()) {
        Some(end) => n_size >= end,
        None => false,
    }
}

/// Reads a single vtable member of type `T` at byte `offset` from `p_class` only when `n_size`
/// covers `offset..offset + size_of::<T>()`.
///
/// Using raw pointer arithmetic instead of creating `&C` prevents undefined behavior if an older
/// LibreOfficeKit build provides a truncated vtable whose allocation is smaller than `size_of::<C>()`.
///
/// # Safety
/// - `p_class` must be non-null, aligned for `T`, and point to at least `n_size` readable bytes.
/// - `offset` must be the byte offset of a `T` field in the C vtable layout represented by `C`.
unsafe fn read_vtable_member<C, T: Copy>(
    p_class: *const C,
    n_size: usize,
    offset: usize,
) -> Option<T> {
    if !has_vtable_member::<T>(n_size, offset) {
        return None;
    }
    // SAFETY: Caller guarantees `p_class` is valid for `n_size` bytes and `has_vtable_member`
    // verified `offset + size_of::<T>() <= n_size`, so the target address stays within the vtable
    // allocation and is properly aligned for `T`.
    let member_ptr = unsafe { p_class.cast::<u8>().add(offset).cast::<T>() };
    // SAFETY: `member_ptr` points to an initialized `T` value inside the C vtable.
    Some(unsafe { member_ptr.read() })
}

/// RAII wrapper around an initialized `LibreOfficeKit` instance and its pinned shared library.
pub(super) struct LokInstance {
    _lib: Option<LoadedDynamicLib>,
    lok: *mut LibreOfficeKit,
}

impl LokInstance {
    pub(super) fn initialize(profile_dir: &Path) -> Result<Self, String> {
        let soffice = find_soffice();
        let install_dirs = candidate_install_dirs_from_soffice(soffice.as_deref());
        let candidates = resolve_candidate_library_paths(&install_dirs, candidate_library_names());
        if candidates.is_empty() {
            return Err("Could not locate LibreOfficeKit shared library".to_string());
        }

        let profile_url = path_to_file_url(profile_dir)
            .map_err(|e| format!("Failed to encode LOK profile URL: {e}"))?;
        let profile_cstr = CString::new(profile_url)
            .map_err(|e| format!("Invalid NUL byte in profile URL: {e}"))?;

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
                    // `lib` is dropped automatically here (`dlclose`/`FreeLibrary`).
                    last_err = err;
                    continue;
                }
            };

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
                    // `lib` is dropped automatically here.
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
                // `lib` is dropped automatically here.
                last_err = format!(
                    "LibreOfficeKit initialization returned null for {}",
                    lib_path.display()
                );
                continue;
            }

            return Ok(Self {
                _lib: Some(lib.keep_loaded()),
                lok,
            });
        }

        Err(last_err)
    }

    fn read_vtable(&self) -> Result<(*const LibreOfficeKitClass, usize), String> {
        if self.lok.is_null() {
            return Err("LibreOfficeKit instance pointer is null".to_string());
        }
        // SAFETY: `self.lok` is a non-null instance pointer whose leading field is `p_class`.
        // Reading only `p_class` via raw pointer avoids constructing an `&LibreOfficeKit` reference.
        let p_class = unsafe { std::ptr::addr_of!((*self.lok).p_class).read() };
        if p_class.is_null() {
            return Err("LibreOfficeKit vtable pointer is null".to_string());
        }
        // SAFETY: `p_class` is non-null and every `LibreOfficeKitClass` begins with `n_size`.
        // Reading `n_size` via raw pointer avoids creating a whole-struct `&LibreOfficeKitClass`
        // reference before `n_size` is validated (preventing UB on truncated older LOK vtables).
        let n_size = unsafe { std::ptr::addr_of!((*p_class).n_size).read() };
        Ok((p_class, n_size))
    }

    fn last_error(&self) -> Option<String> {
        let (p_class, n_size) = self.read_vtable().ok()?;
        // SAFETY: `p_class` is valid for `n_size` bytes and `get_error` has type `LokGetErrorFn`.
        let get_error = unsafe {
            read_vtable_member::<LibreOfficeKitClass, LokGetErrorFn>(
                p_class,
                n_size,
                std::mem::offset_of!(LibreOfficeKitClass, get_error),
            )
        }??;
        // SAFETY: `self.lok` is valid and `n_size` covers `get_error`.
        let ptr = unsafe { get_error(self.lok) };
        if ptr.is_null() {
            None
        } else {
            // Intentionally do NOT pass `ptr` to `libc::free`: `freeError` was added late in
            // `LibreOfficeKitClass`, and calling `libc::free` across OS/shared-library CRT
            // boundaries (e.g., LibreOffice's C++ allocator or Windows CRT vs Rust) is unsafe.
            // Because the `--lok-worker` process is short-lived and only queries `last_error()` on
            // failure paths, retaining the small diagnostic string avoids cross-CRT UB cleanly.
            // SAFETY: `get_error` returned a non-null NUL-terminated C string.
            Some(
                unsafe { CStr::from_ptr(ptr) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    pub(super) fn convert_to_pdf(
        &self,
        src_path: &Path,
        out_pdf_path: &Path,
        page_range: Option<&str>,
    ) -> Result<(), String> {
        let (p_class, n_size) = self.read_vtable()?;
        // SAFETY: `p_class` is valid for `n_size` bytes and `document_load` has type `LokDocLoadFn`.
        let document_load_slot = unsafe {
            read_vtable_member::<LibreOfficeKitClass, LokDocLoadFn>(
                p_class,
                n_size,
                std::mem::offset_of!(LibreOfficeKitClass, document_load),
            )
        }
        .ok_or_else(|| "LibreOfficeKitClass.nSize is too small for document_load".to_string())?;
        let document_load = document_load_slot
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

        let (doc_p_class, doc_n_size) = doc_guard.read_vtable()?;
        // SAFETY: `doc_p_class` is valid for `doc_n_size` bytes and `save_as` has type
        // `LokDocSaveAsFn`.
        let save_as_slot = unsafe {
            read_vtable_member::<LibreOfficeKitDocumentClass, LokDocSaveAsFn>(
                doc_p_class,
                doc_n_size,
                std::mem::offset_of!(LibreOfficeKitDocumentClass, save_as),
            )
        }
        .ok_or_else(|| "LibreOfficeKitDocumentClass.nSize is too small for save_as".to_string())?;
        let save_as = save_as_slot
            .ok_or_else(|| "LibreOfficeKitDocumentClass.save_as is null".to_string())?;

        let opts_ptr = filter_cstr
            .as_ref()
            .map_or(std::ptr::null(), |c| c.as_ptr());

        // SAFETY: `doc_guard.doc` is a valid loaded document, `doc_n_size` covers `save_as`, and
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
        if let Ok((p_class, n_size)) = self.read_vtable()
            // SAFETY: `p_class` is valid for `n_size` bytes and `destroy` has type `LokDestroyFn`.
            && let Some(Some(destroy)) = unsafe {
                read_vtable_member::<LibreOfficeKitClass, LokDestroyFn>(
                    p_class,
                    n_size,
                    std::mem::offset_of!(LibreOfficeKitClass, destroy),
                )
            }
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
    fn read_vtable(&self) -> Result<(*const LibreOfficeKitDocumentClass, usize), String> {
        if self.doc.is_null() {
            return Err("LibreOfficeKitDocument pointer is null".to_string());
        }
        // SAFETY: `self.doc` is a non-null document handle whose leading field is `p_class`.
        let p_class = unsafe { std::ptr::addr_of!((*self.doc).p_class).read() };
        if p_class.is_null() {
            return Err("LibreOfficeKitDocument vtable pointer is null".to_string());
        }
        // SAFETY: `p_class` is non-null and every `LibreOfficeKitDocumentClass` begins with
        // `n_size`. Reading `n_size` via raw pointer avoids whole-struct reference UB on truncated
        // vtables.
        let n_size = unsafe { std::ptr::addr_of!((*p_class).n_size).read() };
        Ok((p_class, n_size))
    }
}

impl Drop for LokDocumentGuard {
    fn drop(&mut self) {
        if let Ok((p_class, n_size)) = self.read_vtable()
            // SAFETY: `p_class` is valid for `n_size` bytes and `destroy` has type `LokDocDestroyFn`.
            && let Some(Some(destroy)) = unsafe {
                read_vtable_member::<LibreOfficeKitDocumentClass, LokDocDestroyFn>(
                    p_class,
                    n_size,
                    std::mem::offset_of!(LibreOfficeKitDocumentClass, destroy),
                )
            }
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ms_lok_ffi_test_{label}_{}_{seq}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp test dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Clone, Copy)]
    enum SaveAsBehavior {
        WriteBytes(&'static [u8]),
        CreateEmptyFile,
        CreateOversizedSparseFile,
        ReturnFalseNoFile,
        ReturnTrueWithoutCreatingFile,
    }

    struct MockState {
        lok_destroy_calls: usize,
        doc_destroy_calls: usize,
        load_returns_null: bool,
        error_msg: Option<&'static CStr>,
        last_filter_options: Option<String>,
        save_as_behavior: SaveAsBehavior,
        target_out_path: Option<PathBuf>,
        mock_doc: Box<LibreOfficeKitDocument>,
        mock_doc_vtable: Box<LibreOfficeKitDocumentClass>,
    }

    impl Default for MockState {
        fn default() -> Self {
            let mut vtable = Box::new(LibreOfficeKitDocumentClass {
                n_size: std::mem::size_of::<LibreOfficeKitDocumentClass>(),
                destroy: Some(mock_doc_destroy),
                save_as: Some(mock_doc_save_as),
            });
            let doc = Box::new(LibreOfficeKitDocument {
                p_class: (&raw mut *vtable),
            });
            Self {
                lok_destroy_calls: 0,
                doc_destroy_calls: 0,
                load_returns_null: false,
                error_msg: None,
                last_filter_options: None,
                save_as_behavior: SaveAsBehavior::WriteBytes(b"%PDF-1.4\n"),
                target_out_path: None,
                mock_doc: doc,
                mock_doc_vtable: vtable,
            }
        }
    }

    thread_local! {
        static MOCK_STATE: RefCell<MockState> = RefCell::new(MockState::default());
    }

    fn reset_mock_state(behavior: SaveAsBehavior) {
        MOCK_STATE.with(|cell| {
            *cell.borrow_mut() = MockState {
                save_as_behavior: behavior,
                ..Default::default()
            };
        });
    }

    unsafe extern "C" fn mock_lok_destroy(_p_this: *mut LibreOfficeKit) {
        MOCK_STATE.with(|cell| cell.borrow_mut().lok_destroy_calls += 1);
    }

    unsafe extern "C" fn mock_doc_destroy(_p_this: *mut LibreOfficeKitDocument) {
        MOCK_STATE.with(|cell| cell.borrow_mut().doc_destroy_calls += 1);
    }

    unsafe extern "C" fn mock_get_error(_p_this: *mut LibreOfficeKit) -> *mut c_char {
        MOCK_STATE.with(|cell| {
            cell.borrow()
                .error_msg
                .map_or(std::ptr::null_mut(), |c| c.as_ptr().cast_mut())
        })
    }

    unsafe extern "C" fn mock_document_load(
        _p_this: *mut LibreOfficeKit,
        _p_url: *const c_char,
    ) -> *mut LibreOfficeKitDocument {
        MOCK_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            if state.load_returns_null {
                return std::ptr::null_mut();
            }
            let vtable_ptr = &raw mut *state.mock_doc_vtable;
            state.mock_doc.p_class = vtable_ptr;
            &raw mut *state.mock_doc
        })
    }

    unsafe extern "C" fn mock_doc_save_as(
        _p_this: *mut LibreOfficeKitDocument,
        _p_url: *const c_char,
        _p_format: *const c_char,
        p_filter_options: *const c_char,
    ) -> c_int {
        MOCK_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            state.last_filter_options = if p_filter_options.is_null() {
                None
            } else {
                // SAFETY: Non-null `p_filter_options` is a valid NUL-terminated C string.
                Some(
                    unsafe { CStr::from_ptr(p_filter_options) }
                        .to_string_lossy()
                        .into_owned(),
                )
            };
            let out_path = state.target_out_path.clone();
            match state.save_as_behavior {
                SaveAsBehavior::WriteBytes(bytes) => {
                    if let Some(path) = out_path {
                        std::fs::write(path, bytes).expect("write mock PDF output");
                    }
                    1
                }
                SaveAsBehavior::CreateEmptyFile => {
                    if let Some(path) = out_path {
                        std::fs::write(path, b"").expect("write empty mock PDF");
                    }
                    1
                }
                SaveAsBehavior::CreateOversizedSparseFile => {
                    if let Some(path) = out_path {
                        let file = std::fs::File::create(path).expect("create sparse mock PDF");
                        file.set_len(MAX_PDF_FILE_BYTES + 1)
                            .expect("extend mock PDF past MAX_PDF_FILE_BYTES");
                    }
                    1
                }
                SaveAsBehavior::ReturnFalseNoFile => 0,
                SaveAsBehavior::ReturnTrueWithoutCreatingFile => 1,
            }
        })
    }

    #[repr(C)]
    struct TruncatedVtableOnlySize {
        n_size: usize,
    }

    #[repr(C)]
    struct TruncatedLokVtableWithDestroy {
        n_size: usize,
        destroy: LokDestroyFn,
    }

    #[repr(C)]
    struct TruncatedDocVtableWithDestroy {
        n_size: usize,
        destroy: LokDocDestroyFn,
    }

    #[test]
    fn vtable_nsize_check_rejects_undersized_structs() {
        let doc_load_off = std::mem::offset_of!(LibreOfficeKitClass, document_load);
        assert!(!has_vtable_member::<LokDocLoadFn>(0, doc_load_off));
        assert!(!has_vtable_member::<LokDocLoadFn>(
            doc_load_off + std::mem::size_of::<LokDocLoadFn>() - 1,
            doc_load_off,
        ));
        assert!(has_vtable_member::<LokDocLoadFn>(
            doc_load_off + std::mem::size_of::<LokDocLoadFn>(),
            doc_load_off,
        ));

        let save_as_off = std::mem::offset_of!(LibreOfficeKitDocumentClass, save_as);
        assert!(!has_vtable_member::<LokDocSaveAsFn>(
            std::mem::size_of::<usize>(),
            save_as_off
        ));
        assert!(has_vtable_member::<LokDocSaveAsFn>(
            std::mem::size_of::<LibreOfficeKitDocumentClass>(),
            save_as_off
        ));
    }

    #[test]
    fn lok_document_guard_calls_destroy_on_drop_and_respects_nsize() {
        reset_mock_state(SaveAsBehavior::ReturnFalseNoFile);

        // 1. Full vtable with `destroy: Some(...)` invokes `destroy` on drop.
        let mut full_vtable = LibreOfficeKitDocumentClass {
            n_size: std::mem::size_of::<LibreOfficeKitDocumentClass>(),
            destroy: Some(mock_doc_destroy),
            save_as: None,
        };
        let mut doc = LibreOfficeKitDocument {
            p_class: &raw mut full_vtable,
        };
        drop(LokDocumentGuard { doc: &raw mut doc });
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);

        // 2. Genuinely truncated vtable smaller than `LibreOfficeKitDocumentClass` skips `destroy`
        // without creating a whole-struct reference or triggering UB.
        let mut tiny_vtable = TruncatedVtableOnlySize {
            n_size: std::mem::size_of::<TruncatedVtableOnlySize>(),
        };
        let mut tiny_doc = LibreOfficeKitDocument {
            p_class: (&raw mut tiny_vtable).cast::<LibreOfficeKitDocumentClass>(),
        };
        drop(LokDocumentGuard {
            doc: &raw mut tiny_doc,
        });
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);

        // 3. Partial `n_size` (`offset + size_of - 1`) skips `destroy` even if function pointer is set.
        let destroy_end = std::mem::offset_of!(LibreOfficeKitDocumentClass, destroy)
            + std::mem::size_of::<LokDocDestroyFn>();
        let mut partial_vtable = LibreOfficeKitDocumentClass {
            n_size: destroy_end - 1,
            destroy: Some(mock_doc_destroy),
            save_as: None,
        };
        let mut partial_doc = LibreOfficeKitDocument {
            p_class: &raw mut partial_vtable,
        };
        drop(LokDocumentGuard {
            doc: &raw mut partial_doc,
        });
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);

        // 4. `n_size` covers `destroy`, but `destroy == None` is a clean no-op.
        let mut none_destroy_vtable = LibreOfficeKitDocumentClass {
            n_size: destroy_end,
            destroy: None,
            save_as: None,
        };
        let mut none_destroy_doc = LibreOfficeKitDocument {
            p_class: &raw mut none_destroy_vtable,
        };
        drop(LokDocumentGuard {
            doc: &raw mut none_destroy_doc,
        });
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);

        // 5. Null `doc` and null `p_class` drop cleanly.
        drop(LokDocumentGuard {
            doc: std::ptr::null_mut(),
        });
        let mut null_vtable_doc = LibreOfficeKitDocument {
            p_class: std::ptr::null_mut(),
        };
        drop(LokDocumentGuard {
            doc: &raw mut null_vtable_doc,
        });
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);
    }

    #[test]
    fn lok_instance_calls_destroy_on_drop_and_respects_nsize() {
        reset_mock_state(SaveAsBehavior::ReturnFalseNoFile);

        // 1. Full vtable with `destroy: Some(...)` invokes `destroy` on drop.
        let mut full_vtable = LibreOfficeKitClass {
            n_size: std::mem::size_of::<LibreOfficeKitClass>(),
            destroy: Some(mock_lok_destroy),
            document_load: None,
            get_error: None,
            document_load_with_options: None,
        };
        let mut lok = LibreOfficeKit {
            p_class: &raw mut full_vtable,
        };
        drop(LokInstance {
            _lib: None,
            lok: &raw mut lok,
        });
        assert_eq!(MOCK_STATE.with(|s| s.borrow().lok_destroy_calls), 1);

        // 2. Genuinely truncated vtable (`TruncatedVtableOnlySize`) skips `destroy` safely.
        let mut tiny_vtable = TruncatedVtableOnlySize {
            n_size: std::mem::size_of::<TruncatedVtableOnlySize>(),
        };
        let mut tiny_lok = LibreOfficeKit {
            p_class: (&raw mut tiny_vtable).cast::<LibreOfficeKitClass>(),
        };
        drop(LokInstance {
            _lib: None,
            lok: &raw mut tiny_lok,
        });
        assert_eq!(MOCK_STATE.with(|s| s.borrow().lok_destroy_calls), 1);

        // 3. `n_size` 1 byte short of covering `destroy` skips `destroy`.
        let destroy_end = std::mem::offset_of!(LibreOfficeKitClass, destroy)
            + std::mem::size_of::<LokDestroyFn>();
        let mut partial_vtable = LibreOfficeKitClass {
            n_size: destroy_end - 1,
            destroy: Some(mock_lok_destroy),
            document_load: None,
            get_error: None,
            document_load_with_options: None,
        };
        let mut partial_lok = LibreOfficeKit {
            p_class: &raw mut partial_vtable,
        };
        drop(LokInstance {
            _lib: None,
            lok: &raw mut partial_lok,
        });
        assert_eq!(MOCK_STATE.with(|s| s.borrow().lok_destroy_calls), 1);

        // 4. `n_size` covers `destroy`, but `destroy == None` skips cleanly.
        let mut none_destroy_vtable = LibreOfficeKitClass {
            n_size: destroy_end,
            destroy: None,
            document_load: None,
            get_error: None,
            document_load_with_options: None,
        };
        let mut none_destroy_lok = LibreOfficeKit {
            p_class: &raw mut none_destroy_vtable,
        };
        drop(LokInstance {
            _lib: None,
            lok: &raw mut none_destroy_lok,
        });
        assert_eq!(MOCK_STATE.with(|s| s.borrow().lok_destroy_calls), 1);
    }

    #[test]
    fn convert_to_pdf_rejects_undersized_vtables() {
        let dir = TempDir::new("undersized_vtables");
        let src_path = dir.0.join("input.docx");
        std::fs::write(&src_path, b"dummy").expect("write dummy source");
        let out_path = dir.0.join("output.pdf");

        // Case 1: Truncated `LibreOfficeKitClass` omitting `document_load` (smaller struct).
        reset_mock_state(SaveAsBehavior::ReturnFalseNoFile);
        let mut lok_vtable_small = TruncatedLokVtableWithDestroy {
            n_size: std::mem::size_of::<TruncatedLokVtableWithDestroy>(),
            destroy: Some(mock_lok_destroy),
        };
        let mut lok_small = LibreOfficeKit {
            p_class: (&raw mut lok_vtable_small).cast::<LibreOfficeKitClass>(),
        };
        let inst_small = LokInstance {
            _lib: None,
            lok: &raw mut lok_small,
        };
        let err = inst_small
            .convert_to_pdf(&src_path, &out_path, None)
            .expect_err("truncated LOK vtable must reject missing document_load");
        assert!(err.contains("document_load"), "unexpected error: {err}");
        drop(inst_small);
        assert_eq!(MOCK_STATE.with(|s| s.borrow().lok_destroy_calls), 1);

        // Case 2: Full `LibreOfficeKitClass`, but returned document has a truncated vtable
        // omitting `save_as`. `LokDocumentGuard` must still destroy the loaded document!
        reset_mock_state(SaveAsBehavior::ReturnFalseNoFile);
        let mut doc_vtable_small = TruncatedDocVtableWithDestroy {
            n_size: std::mem::size_of::<TruncatedDocVtableWithDestroy>(),
            destroy: Some(mock_doc_destroy),
        };
        MOCK_STATE.with(|cell| {
            cell.borrow_mut().mock_doc.p_class =
                (&raw mut doc_vtable_small).cast::<LibreOfficeKitDocumentClass>();
        });
        unsafe extern "C" fn load_with_small_doc(
            _p_this: *mut LibreOfficeKit,
            _p_url: *const c_char,
        ) -> *mut LibreOfficeKitDocument {
            MOCK_STATE.with(|cell| &raw mut *cell.borrow_mut().mock_doc)
        }

        let mut lok_vtable = LibreOfficeKitClass {
            n_size: std::mem::size_of::<LibreOfficeKitClass>(),
            destroy: Some(mock_lok_destroy),
            document_load: Some(load_with_small_doc),
            get_error: None,
            document_load_with_options: None,
        };
        let mut lok = LibreOfficeKit {
            p_class: &raw mut lok_vtable,
        };
        let inst = LokInstance {
            _lib: None,
            lok: &raw mut lok,
        };
        let err = inst
            .convert_to_pdf(&src_path, &out_path, None)
            .expect_err("truncated document vtable must reject missing save_as");
        assert!(err.contains("save_as"), "unexpected error: {err}");
        assert_eq!(
            MOCK_STATE.with(|s| s.borrow().doc_destroy_calls),
            1,
            "LokDocumentGuard must call destroy even when save_as is outside n_size"
        );
        drop(inst);
        assert_eq!(MOCK_STATE.with(|s| s.borrow().lok_destroy_calls), 1);
    }

    #[test]
    fn convert_to_pdf_handles_success_save_as_failure_empty_and_oversized_pdf() {
        let dir = TempDir::new("convert_paths");
        let src_path = dir.0.join("report.docx");
        std::fs::write(&src_path, b"dummy").expect("write source");

        let mut lok_vtable = LibreOfficeKitClass {
            n_size: std::mem::size_of::<LibreOfficeKitClass>(),
            destroy: Some(mock_lok_destroy),
            document_load: Some(mock_document_load),
            get_error: Some(mock_get_error),
            document_load_with_options: None,
        };
        let mut lok = LibreOfficeKit {
            p_class: &raw mut lok_vtable,
        };
        let inst = LokInstance {
            _lib: None,
            lok: &raw mut lok,
        };

        // 1. `document_load` returns null -> surfaces `last_error()`.
        reset_mock_state(SaveAsBehavior::ReturnFalseNoFile);
        MOCK_STATE.with(|cell| {
            let mut s = cell.borrow_mut();
            s.load_returns_null = true;
            s.error_msg = Some(c"mock load failure");
        });
        let err = inst
            .convert_to_pdf(&src_path, &dir.0.join("out1.pdf"), None)
            .expect_err("null document_load must fail");
        assert!(err.contains("mock load failure"), "got: {err}");
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 0);

        // 2. `save_as` returns 0 -> surfaces `last_error()` and drops `LokDocumentGuard`.
        reset_mock_state(SaveAsBehavior::ReturnFalseNoFile);
        MOCK_STATE.with(|cell| {
            cell.borrow_mut().error_msg = Some(c"mock save failure");
        });
        let err = inst
            .convert_to_pdf(&src_path, &dir.0.join("out2.pdf"), Some("1"))
            .expect_err("save_as returning 0 must fail");
        assert!(err.contains("mock save failure"), "got: {err}");
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);
        assert_eq!(
            MOCK_STATE.with(|s| s.borrow().last_filter_options.clone()),
            Some(r#"{"PageRange":{"type":"string","value":"1"}}"#.to_string())
        );

        // 3. `save_as` returns 1 without creating the output file -> metadata error + destroy called.
        reset_mock_state(SaveAsBehavior::ReturnTrueWithoutCreatingFile);
        let err = inst
            .convert_to_pdf(&src_path, &dir.0.join("missing_out.pdf"), None)
            .expect_err("missing output file must fail");
        assert!(err.contains("without readable PDF output"), "got: {err}");
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);

        // 4. `save_as` writes empty 0-byte PDF -> empty PDF error + destroy called.
        let empty_out = dir.0.join("empty.pdf");
        reset_mock_state(SaveAsBehavior::CreateEmptyFile);
        MOCK_STATE.with(|cell| cell.borrow_mut().target_out_path = Some(empty_out.clone()));
        let err = inst
            .convert_to_pdf(&src_path, &empty_out, None)
            .expect_err("empty PDF must fail");
        assert!(err.contains("empty PDF file"), "got: {err}");
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);

        // 5. `save_as` writes oversized PDF (> MAX_PDF_FILE_BYTES) -> size limit error + destroy.
        let big_out = dir.0.join("oversized.pdf");
        reset_mock_state(SaveAsBehavior::CreateOversizedSparseFile);
        MOCK_STATE.with(|cell| cell.borrow_mut().target_out_path = Some(big_out.clone()));
        let err = inst
            .convert_to_pdf(&src_path, &big_out, None)
            .expect_err("oversized PDF must fail");
        assert!(err.contains("exceeds size limit"), "got: {err}");
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);

        // 6. Happy path writes valid non-empty PDF -> Ok(()) + destroy called.
        let ok_out = dir.0.join("valid.pdf");
        reset_mock_state(SaveAsBehavior::WriteBytes(b"%PDF-1.7\n%mock\n"));
        MOCK_STATE.with(|cell| cell.borrow_mut().target_out_path = Some(ok_out.clone()));
        inst.convert_to_pdf(&src_path, &ok_out, None)
            .expect("valid PDF conversion should succeed");
        assert_eq!(MOCK_STATE.with(|s| s.borrow().doc_destroy_calls), 1);

        drop(inst);
        assert_eq!(MOCK_STATE.with(|s| s.borrow().lok_destroy_calls), 1);
    }

    #[test]
    fn non_existent_path_to_file_url_rejects_missing_parent() {
        let dir = TempDir::new("file_url");
        let missing_parent = dir.0.join("does_not_exist").join("out.pdf");
        assert!(non_existent_path_to_file_url(&missing_parent).is_err());

        assert!(non_existent_path_to_file_url(Path::new("")).is_err());
        assert!(non_existent_path_to_file_url(Path::new("/")).is_err());

        let valid_out = dir.0.join("my report.pdf");
        let url = non_existent_path_to_file_url(&valid_out).expect("valid output file URL");
        assert!(url.starts_with("file://"), "unexpected URL: {url}");
        assert!(url.ends_with("/my%20report.pdf"), "unexpected URL: {url}");
    }
}
