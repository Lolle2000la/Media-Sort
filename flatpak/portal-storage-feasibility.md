# Portal-only storage: feasibility

Can Media Sort reach its files entirely through the XDG Desktop Portals, so the
Flatpak stops asking for `--filesystem=home` (and `/media`, `/mnt`,
`/run/media`)?

**Short answer: not entirely, and not yet.** A folder the user picks does give
the sandbox working access to its whole subtree — reading, thumbnails,
previews, metadata and create/rename/delete all work through ordinary path APIs
on the document portal's FUSE mount (`/run/user/$UID/doc/<doc_id>/…`). Four
things the app leans on cannot be reproduced, and two of them (the filesystem
watcher and arbitrary folder-tree navigation) are core features, so a
portal-only build would be a product regression rather than a permissions
change.

That is why the manifest keeps `--filesystem=home` and the permission is asked
for explicitly (Flathub grants `finish-args-home-filesystem-access` per app on
explanation). The portal work that *is* built — the OpenURI actions in
`crates/media-sort-backend/src/platform/desktop_portal.rs`, plus
`org.freedesktop.portal.Trash` for deletion — is the part that is strictly
better under a sandbox and needs no manifest change.

## What the portals can do

| Need | Portal | Method |
|---|---|---|
| Pick folder(s) | `org.freedesktop.portal.FileChooser` | `OpenFile(directory=true, multiple=…)`; `rfd` already uses its `xdg-portal` backend for this |
| FUSE mount base | `Documents` | `GetMountPoint` |
| Export a path the app can already open | `Documents` | `Add` (file) / `AddFull` + `export-directory` (folder, interface v4) |
| Real path, for display only | `Documents` | `GetHostPaths` (v5) |
| Trash | `org.freedesktop.portal.Trash` | `TrashFile(fd)` |
| Open file / reveal folder | `org.freedesktop.portal.OpenURI` | `OpenFile(fd)` / `OpenDirectory(fd)`, both fd-based |
| Receive dropped files | `FileTransfer` | needs the *source* application to cooperate |

An exported directory is recursive, and `Documents.List`/`Info`/`Lookup` are
**host-only**: a sandboxed app cannot enumerate its own grants, it can only
remember the paths it was handed.

## What it cannot do

1. **The filesystem watcher goes blind.** inotify on the document portal's FUSE
   mount fires for operations performed *through the mount* (including the
   app's own writes) but never for a change a host process makes to the backing
   file — FUSE does not synthesize fsnotify events for out-of-band mutations
   (<https://github.com/flatpak/xdg-desktop-portal/issues/567>,
   <https://github.com/libfuse/libfuse/issues/328>). Noticing external changes
   is exactly what the watcher exists for, so the central "sort what appeared
   in this folder" loop would only see them after a manual refresh.
2. **Cross-grant move/rename returns `EXDEV`.** Each picked location is its own
   document domain, so moving a file into a *different* picked folder becomes
   copy-then-delete: non-atomic, double I/O, and outside the existing
   `rename_or_copy_and_delete` error handling. Renames *within* one grant are
   fine.
3. **A singly-picked *file* cannot be renamed or deleted.** For non-directory
   documents the FUSE layer only permits the tempfile/"atomic save" idiom
   (arbitrary renames return `EACCES`/`ENOENT`), and `mkdir`/`unlink`/`rmdir`
   require a directory document. The user must grant the *containing folder*.
4. **Grants are not durable bookmarks.** A stored document path frequently
   fails to resolve again after a reboot — identity was keyed on
   `st_dev`+`st_ino`, which btrfs/LVM/Silverblue make unstable
   (<https://github.com/flatpak/xdg-desktop-portal/issues/1362>,
   <https://github.com/flatpak/xdg-desktop-portal/issues/1845>) — and the app
   cannot re-enumerate its grants from inside the sandbox. Expect recurring
   "please pick the folder again" prompts.
5. **No arbitrary navigation.** Only the picked locations and their subtrees
   exist: there is no home/drive enumeration, and `..` above a picked root is
   not readable. The folder tree would have to be re-rooted on the grants
   (`build_parent_chain` stopping at the grant, the unconditional
   `current_folder.parent()` watch entry dropped).
6. **Paths become opaque**, and the app treats paths as identity. Users would
   see `/run/user/1000/doc/<id>/…`; `canonicalize()` on a document path either
   fails or resolves to a host path the sandbox cannot use, and `paths_equal`
   compares across two different path spaces. Caches, selection restore,
   undo/redo, the watcher subscription key and mpv frame matching would all
   have to key on `(doc_id, relative path)` instead.

I/O is slower too: the document portal's FUSE implementation caches no
attributes and no entries, so every `lookup`/`open` is a userspace round trip.

## Precedent

Apps that stay portal-only do *content* work rather than library browsing:
Metadata Cleaner has no `--filesystem` at all and processes picked files and
folders through the document portal. Apps that browse or watch a library take
the broad grant instead — Loupe (`--filesystem=host`), GNOME Text Editor
(`--filesystem=host`), Curtail (`home`), Amberol (`xdg-music:ro`). Upstream's
own answer to "file-manager-like access to specific trees" is the FileChooser
with `directory=true`, and it calls a dynamic per-path filesystem grant
"technically not possible" without a new FUSE overlay layer
(<https://github.com/flatpak/xdg-desktop-portal/issues/663>).

## If a portal-only mode is ever attempted

1. **Granted-locations model:** persist each grant (document id + intended host
   path), re-resolve on launch, and prompt for the ones that no longer resolve.
2. **Re-root the tree** on the grants, as in (5) above.
3. **Watcher:** disable it under portal mode — self-write-only events are
   useless — and say so in the UI rather than silently not refreshing.
4. **Moves:** route cross-grant moves through copy+delete, keeping same-grant
   renames as `rename(2)`.
5. **Identity:** key caches, selection and history on `(doc_id, relative path)`
   rather than the volatile mount path.
6. **Manifest:** drop `--filesystem=home`, `/media`, `/mnt` and `/run/media`;
   keep `--share=ipc`, the sockets and `--device=dri`.

Even then the UX is "pick a folder, then work inside it", not "browse your
computer", and the watcher gap stays until a FUSE notification mechanism exists
upstream.

## Verification status

`platform/desktop_portal.rs` compiles and is clippy-clean, but its D-Bus calls
cannot be exercised outside a sandbox. Smoke-test them in a real Flatpak build
(GNOME and KDE have separate portal implementations, including a GNOME
FileChooser replacement): open a file externally, and reveal one in the file
manager. The trash portal shares the caveat and the same manual check.
