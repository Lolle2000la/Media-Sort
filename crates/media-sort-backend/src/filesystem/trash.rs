use std::path::{Path, PathBuf};

use media_sort_core::actions::delete_action::TrashRestoreHandle;
use media_sort_core::actions::reversible::ActionError;
#[cfg(target_os = "windows")]
use media_sort_core::path_utils;

/// Reaching the host trash through `org.freedesktop.portal.Trash` under Flatpak
/// (see the module for why the `trash` crate cannot be used there).
#[cfg(target_os = "linux")]
mod portal;

#[cfg(target_os = "macos")]
fn macos_trash_item(path: &Path) -> Result<PathBuf, ActionError> {
    use objc2::rc::Retained;
    use objc2_foundation::{NSError, NSFileManager, NSString, NSURL};

    let path_str = path
        .to_str()
        .ok_or_else(|| ActionError::Io(std::io::Error::other("Invalid path encoding")))?;

    // SAFETY: We are calling Objective-C FFI methods through the `objc2` and
    // `objc2_foundation` runtimes. All pointers are checked for nullability
    // before dereferencing or retaining them, and the runtime handles the
    // reference counting correctly once retained.
    unsafe {
        let file_manager = NSFileManager::defaultManager();
        let ns_str = NSString::from_str(path_str);
        let url = NSURL::fileURLWithPath(&ns_str);

        let mut resulting_url: *mut NSURL = std::ptr::null_mut();
        let mut error: *mut NSError = std::ptr::null_mut();

        let success: bool = objc2::msg_send![
            &file_manager,
            trashItemAtURL: &*url,
            resultingItemURL: &mut resulting_url,
            error: &mut error
        ];

        if !success || resulting_url.is_null() {
            let err_msg = if !error.is_null() {
                let err = Retained::retain(error).expect("error pointer is checked to be non-null");
                err.localizedDescription().to_string()
            } else {
                "Cocoa trashItemAtURL failed".to_string()
            };
            return Err(ActionError::Io(std::io::Error::other(err_msg)));
        }

        let res_id =
            Retained::retain(resulting_url).expect("resulting_url is checked to be non-null");
        let path_nsstring = res_id
            .path()
            .expect("resulting trash URL must have a valid path");
        Ok(PathBuf::from(path_nsstring.to_string()))
    }
}

pub fn delete_to_trash(path: &Path) -> Result<Box<dyn TrashRestoreHandle>, ActionError> {
    let original_path = path.to_path_buf();

    #[cfg(not(target_os = "macos"))]
    {
        // Windows: callers may pass 8.3 short paths (e.g. TEMP resolves to
        // C:\Users\RUNNER~1\... on CI runners). Canonicalize while the file
        // still exists so the stored path matches what the trash metadata
        // records, stripping the \\?\ verbatim prefix canonicalize yields.
        // Symlinks are exempt: canonicalize would resolve the link and trash
        // the TARGET file instead of the link itself.
        #[cfg(target_os = "windows")]
        let original_path = {
            let is_symlink = path_utils::is_symlink(&original_path);
            let canon = if is_symlink {
                original_path.clone()
            } else {
                original_path
                    .canonicalize()
                    .unwrap_or(original_path.clone())
            };
            match canon.to_string_lossy().strip_prefix(r"\\?\") {
                Some(stripped) => PathBuf::from(stripped.to_owned()),
                None => canon,
            }
        };

        // Under Flatpak `$XDG_DATA_HOME` is remapped to a per-app directory, so
        // `trash` would move the file into the sandbox's private data
        // directory, where the desktop cannot see or empty it; it would also
        // read the sandbox's bind mount of the home directory as a mount point
        // and trash into a `$HOME/.Trash-1000` volume trash that no file
        // manager shows. Ask the host through the portal first, then write the
        // real home trash ourselves, and keep the in-sandbox fallback only for
        // files the home trash cannot take (another filesystem, where the
        // crate finds the volume's own trash).
        #[cfg(target_os = "linux")]
        if portal::in_flatpak() {
            match portal::delete_to_trash(&original_path) {
                Ok(handle) => return Ok(handle),
                Err(err) => {
                    tracing::warn!(
                        path = %original_path.display(),
                        %err,
                        "org.freedesktop.portal.Trash failed; trashing into the host trash directly instead"
                    );
                    match portal::trash_into_home_trash(&original_path) {
                        Ok(handle) => return Ok(handle),
                        Err(err) => tracing::warn!(
                            path = %original_path.display(),
                            %err,
                            "direct host trash failed; trashing inside the sandbox instead"
                        ),
                    }
                }
            }
        }

        trash::delete(&original_path)
            .map_err(|e| ActionError::Io(std::io::Error::other(e.to_string())))?;

        // The `trash` crate creates `Trash/{,files,info}` with `fs::create_dir_all`
        // (mode 0755 under a normal umask), whereas `xdg-desktop-portal` 1.20+
        // strictly requires mode 0700 and rejects any subsequent portal trash
        // call with "Directory already exists with bad permissions". Normalize
        // permissions back to 0700 immediately after `trash::delete`.
        #[cfg(target_os = "linux")]
        portal::repair_existing_trash_permissions(&original_path);

        // Windows: remember when the delete happened so restore can bind
        // this handle to the correct trash entry among several same-name
        // items (time_deleted has second granularity like the shell's).
        #[cfg(target_os = "windows")]
        let delete_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Ok(Box::new(NativeTrashRestore {
            original_path,
            #[cfg(target_os = "windows")]
            delete_time,
            flushed: false,
        }))
    }

    #[cfg(target_os = "macos")]
    {
        let trash_path = macos_trash_item(&original_path)?;
        Ok(Box::new(NativeTrashRestore {
            original_path,
            trash_path: Some(trash_path),
            flushed: false,
        }))
    }
}

/// Windows trash item matching: case-insensitive and tolerant of `\\?\`
/// verbatim prefixes, since the shell may report a different path form
/// than the caller passed (short 8.3 names, casing, verbatim paths).
#[cfg(target_os = "windows")]
fn windows_trash_paths_match(item_path: &Path, stored: &Path) -> bool {
    fn strip_verbatim(p: &Path) -> String {
        let s = p.to_string_lossy();
        s.strip_prefix(r"\\?\")
            .map(str::to_owned)
            .unwrap_or_else(|| s.into_owned())
    }
    strip_verbatim(item_path).eq_ignore_ascii_case(&strip_verbatim(stored))
}

struct NativeTrashRestore {
    original_path: PathBuf,
    #[cfg(target_os = "macos")]
    trash_path: Option<PathBuf>,
    /// Unix seconds when the delete was performed; used on Windows to bind
    /// this handle to the correct recycle-bin entry.
    #[cfg(target_os = "windows")]
    delete_time: i64,
    flushed: bool,
}

impl TrashRestoreHandle for NativeTrashRestore {
    fn restore(&mut self) -> Result<(), ActionError> {
        if self.flushed {
            return Err(ActionError::RestorationFailed("already flushed".into()));
        }

        #[cfg(all(
            unix,
            not(target_os = "macos"),
            not(target_os = "ios"),
            not(target_os = "android")
        ))]
        {
            let items = trash::os_limited::list()
                .map_err(|e| ActionError::Io(std::io::Error::other(e.to_string())))?;

            let item = items
                .into_iter()
                .find(|i| i.original_path() == self.original_path)
                .ok_or_else(|| {
                    ActionError::RestorationFailed("item not found in system trash".into())
                })?;

            trash::os_limited::restore_all([item])
                .map_err(|e| ActionError::Io(std::io::Error::other(e.to_string())))?;

            self.flushed = true;
            Ok(())
        }

        #[cfg(target_os = "windows")]
        {
            let items = trash::os_limited::list()
                .map_err(|e| ActionError::Io(std::io::Error::other(e.to_string())))?;

            let parent = self.original_path.parent();
            let ext = self
                .original_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());

            // On Windows, TrashItem::original_path() is unreliable: the
            // shell display name (SIGDN_PARENTRELATIVE) drops the extension
            // for known file types (Explorer's "hide extensions" setting),
            // so both matching and the crate's name-based restore would use
            // the wrong file name. Instead match on original_parent plus the
            // extension of the recycle-bin data file ($Rxxxx.ext, which keeps
            // the true extension), and among same-name candidates pick the
            // entry whose deletion time is closest to this handle's delete
            // (correct undo order: newest delete is undone first). Restore
            // with the true file name patched in.
            let mut candidates: Vec<_> = items
                .into_iter()
                .filter(|i| {
                    let parent_matches =
                        parent.is_some_and(|p| windows_trash_paths_match(&i.original_parent, p));
                    let id_ext = PathBuf::from(&i.id)
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_lowercase());
                    parent_matches && id_ext == ext
                })
                .collect();
            // Among same-name candidates pick the entry whose deletion time
            // is closest to this handle's delete (correct undo order: newest
            // delete is undone first). Both timestamps have second
            // granularity, so a delete that straddles a second boundary
            // yields a captured delete_time one second LATER than the
            // shell's timestamp for the same item, creating an exact
            // distance tie with a newer same-name item — the tiebreak below
            // prefers the OLDER entry (the item this handle deleted can
            // never carry a later shell timestamp than the one captured
            // after the delete call returned), so the sort outcome is
            // deterministic regardless of enumeration order.
            candidates.sort_by(|a, b| {
                let da = (a.time_deleted - self.delete_time).abs();
                let db = (b.time_deleted - self.delete_time).abs();
                da.cmp(&db).then(a.time_deleted.cmp(&b.time_deleted))
            });

            let Some(mut item) = candidates.into_iter().next() else {
                return Err(ActionError::RestorationFailed(
                    "item not found in system trash".into(),
                ));
            };
            if let Some(name) = self.original_path.file_name() {
                item.name = name.to_os_string();
            }

            trash::os_limited::restore_all([item])
                .map_err(|e| ActionError::Io(std::io::Error::other(e.to_string())))?;

            self.flushed = true;
            Ok(())
        }

        #[cfg(target_os = "macos")]
        {
            if let Some(ref trash_item_path) = self.trash_path {
                if !trash_item_path.exists() {
                    return Err(ActionError::RestorationFailed(
                        "Trash item no longer exists or trash was emptied".into(),
                    ));
                }
                if let Some(parent) = self.original_path.parent() {
                    std::fs::create_dir_all(parent).map_err(ActionError::Io)?;
                }

                std::fs::rename(trash_item_path, &self.original_path).map_err(ActionError::Io)?;

                self.flushed = true;
                Ok(())
            } else {
                Err(ActionError::RestorationFailed(
                    "Missing localized trash handle".into(),
                ))
            }
        }
    }

    fn flush_to_native_trash(&mut self) -> Result<(), ActionError> {
        self.flushed = true;
        Ok(())
    }
}

impl Drop for NativeTrashRestore {
    fn drop(&mut self) {
        if !self.flushed {
            let _ = self.flush_to_native_trash();
        }
    }
}
