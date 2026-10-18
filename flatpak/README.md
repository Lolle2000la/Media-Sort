# Flatpak / Flathub packaging

This directory holds everything needed to build and submit **Media Sort** as a
Flatpak on [Flathub](https://flathub.org):

| File | Purpose |
| --- | --- |
| `app.mediasort.MediaSort.yml` | The Flatpak build manifest (this is the file submitted to Flathub). |
| `cargo-sources.json` | Vendored Rust crates, generated from `Cargo.lock` (Flathub builds offline). |

Upstream metadata lives with the other Linux packaging files:

- `packaging/linux/app.mediasort.MediaSort.metainfo.xml` — AppStream metadata (Flathub store listing).
- `packaging/linux/app.mediasort.MediaSort.desktop` — desktop entry.
- `packaging/linux/icon.svg` — the icon (installed as `app.mediasort.MediaSort.svg`).

## Application id

The id is **`app.mediasort.MediaSort`**, derived from the `mediasort.app` domain
(declared in `website/public/CNAME`). This makes the app eligible for the
Flathub *Verified* badge: after the first build, add the verification token
Flathub gives you to `website/public/.well-known/org.flathub.VerifiedApps.txt`
and click *Verify* on the app's Flathub page. The id is also the value of the
window's Wayland/X11 app id at runtime (see below), so keep the metadata file
names, the `<id>` in the metainfo, the `Icon=` in the desktop file and the
`app-id` in the manifest in sync.

## How it is built

- **Runtime/SDK:** `org.freedesktop.Platform` / `org.freedesktop.Sdk` `26.08`
  (Freedesktop). Flathub requires the latest runtime at submission time; bump
  this and re-check when a new yearly branch lands.
- **Rust:** the `org.freedesktop.Sdk.Extension.rust-stable` SDK extension.
- **libmpv:** not part of the runtime, so `mpv` (v0.41.0) is built from source
  in a module, together with its two unconditional build dependencies that the
  runtime does not ship — `libass` and `libplacebo`. Everything the three link
  against comes from the runtime/SDK (ffmpeg libraries, dav1d, libva,
  PulseAudio/PipeWire/ALSA, freetype/fontconfig/harfbuzz/fribidi,
  vulkan/shaderc/lcms2), so no codec stack is built here. Media Sort renders
  frames itself through libmpv's software render API, so mpv needs no
  display/GPU output of its own.
- **Hardware decoding (VA-API):** mpv gates VA-API behind one of its display
  back-ends, and both of those are missing a dependency in the runtime — its
  Wayland back-end wants `wayland-protocols` and its X11 back-end wants
  `libXpresent`. Both are built as small modules so `hwdec=auto-copy` works on
  Wayland and X11 sessions alike (software decoding when a GPU cannot do it).
- **ffmpeg CLI:** the runtime ships `/usr/bin/ffmpeg`, which is what
  `media-sort-backend`'s `find_ffmpeg()` picks up for video thumbnails.
- **Audio:** mpv is built *without* ALSA. The sandbox grants the PulseAudio
  socket and no raw ALSA access, so mpv's ALSA probe can only fail — it used to
  print a stream of `ALSA function 'snd_pcm_avail_delay' failed with error 'I/O
  error (5)'` on every file with an audio track before falling back to
  PulseAudio. Playback was never affected, only the log.
- **zimg:** built as a module because the runtime does not ship it, and mpv's
  software render path — the one this app drives — converts every frame with it.
  Without it mpv falls back to swscale, and a 4K video drops from ~30 fps to
  ~6.6 fps (hardware decoding on either way): ~7 ms versus ~66 ms per frame.
- **Trash:** deletions go through `org.freedesktop.portal.Trash`, so files land
  in the *host* trash where the desktop can see and empty them. Without it the
  `trash` crate would use `$XDG_DATA_HOME/Trash`, which a Flatpak remaps to a
  private directory inside `~/.var/app` that only survives until the app is
  uninstalled with `--delete-data`. Undo/restore finds the entry again by
  reading the host trash's `.trashinfo` metadata (the portal only deletes).
- **Open externally / reveal in file manager:** inside the sandbox both go
  through `org.freedesktop.portal.OpenURI` (`platform/desktop_portal.rs`). The
  file manager and the default handler are *host* applications, and the
  sandbox's session-bus policy lets the app reach only its own name,
  `org.freedesktop.DBus` and `org.freedesktop.portal.*` — so the native
  `dbus-send` (`org.freedesktop.FileManager1`) route cannot work there at all.
  `OpenURI` takes file descriptors and has no select-a-file call, so revealing
  opens the containing folder rather than highlighting the entry. Outside
  Flatpak the native `open`/`explorer`/`xdg-open` code runs unchanged.
- **The self-updater (`velopack` feature) is intentionally not built** — Flatpak
  owns updates. That is also why the manifest needs no network access.

## Permissions

The app is a media *sorter*: it moves, renames, copies and deletes files in a
folder the user opens, including into pinned destination folders elsewhere.
That is file-management work, so it needs real filesystem access rather than the
document-portal view of a single chosen folder, hence `--filesystem=home` plus
the removable-media mount points. `portal-storage-feasibility.md` records why
the document portal cannot replace that grant: the filesystem watcher sees no
change a host application makes, and the folder tree needs paths above the
folder the user picked. There is no `--filesystem=host` and no
network access. Device/audio access is `--device=dri` (wgpu/Vulkan rendering and
VA-API hardware decoding) and `--socket=pulseaudio`.

## NVIDIA

Nothing in the manifest can carry the NVIDIA userspace driver: it must match the
user's *kernel* driver, so Flatpak ships it as a runtime extension and picks it
from the host. Without it the sandbox falls back to Mesa's `llvmpipe`, which is
a CPU rasteriser — the window still works, but every frame (including mpv's
frame path) is composited on the CPU, which looks like a slideshow.

Tell the user to install the extension matching the drivers in `/sys/module/nvidia/version`
(Flatpak matches on that, checked against `libnvidia-glcore.so.<version>` in
`/usr/lib`):

```bash
# GL/EGL/Vulkan userspace — required for GPU rendering at all
flatpak install flathub org.freedesktop.Platform.GL.nvidia-<major>-<minor>-<patch>
# nvidia-vaapi-driver, so mpv's hwdec=auto-copy can decode on the GPU
flatpak install flathub org.freedesktop.Platform.VAAPI.nvidia//26.08
```

e.g. driver `615.78.08` → `org.freedesktop.Platform.GL.nvidia-615-78-08`. The GL
extension is published on branch `1.4`, which the 26.08 runtime's GL extension
point accepts (`versions = 26.08;26.08-extra;1.4`) — that is normal, not a
mismatch. Both installs are user-visible either system-wide or with
`--user` (the latter needs a user Flathub remote: `flatpak remote-add --user
flathub https://flathub.org/repo/flathub.flatpakrepo`).

Symptom to recognise: `flatpak run` logs
`Unable to find extension: VK_EXT_physical_device_drm`, `MESA-EGL: egl: failed
to create dri2 screen`, and a wgpu adapter list containing only `llvmpipe`.
That is an environment gap, not an app or manifest bug — the manifest already
gets `--device=dri` and Flatpak passes `/dev/nvidia*` in. The extension is bound
when the app is *launched*, so an instance that was already running when the
driver was installed keeps the software stack and has to be quit and started
again. A working setup shows `NVIDIA … DiscreteGpu` in the adapter list and
names that adapter on the `Selected:` line, and `libva` loading
`…/dri/nvidia-vaapi-driver/nvidia_drv_video.so` ("VA-API NVDEC driver") for
`hwdec=auto-copy`.

## Building locally

Requires `flatpak-builder` (or the `org.flatpak.Builder` Flatpak) and the SDK:

```bash
flatpak install flathub org.freedesktop.Sdk//26.08 \
  org.freedesktop.Sdk.Extension.rust-stable//26.08
```

Then, from the repository root:

```bash
flatpak-builder --user --install --force-clean build-dir flatpak/app.mediasort.MediaSort.yml
flatpak run app.mediasort.MediaSort
```

The manifest's app source points at the **released tag** (`v3.0.0`), so for a
local build of uncommitted work, temporarily point that source at this checkout
(e.g. a `type: dir` or local `type: git` source) instead.

CI runs this same build for any change to `Cargo.lock`, a `Cargo.toml` or
`flatpak/**`: `.github/workflows/flatpak-build.yml` rewrites the manifest's app
source to `type: dir` (so it builds the checkout, not the release tag) and runs
`flatpak-builder` against the same runtime. It is expensive, so it is not a
required check.

Lint the manifest before/after changes:

```bash
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest flatpak/app.mediasort.MediaSort.yml
flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo build-dir/repo
```

## Submitting to Flathub

Flathub hosts the manifest in its own repository and builds it on Flathub's
infrastructure — nothing here is published automatically.

1. **Cut a stable release that includes this packaging.** Flathub's stable
   repository accepts only stable software, and the manifest installs the
   metadata from the tagged source tree, so the source tag must contain
   `packaging/linux/app.mediasort.MediaSort.*` and `flatpak/`. `v3.0.0` predates
   them, so cut a follow-up (e.g. `scripts/release.sh 3.0.1`).
2. Point the manifest's `media-sort` source at that tag *and* its commit
   (Flathub wants both) and add the matching `<release>` to the metainfo.
3. Regenerate `cargo-sources.json` whenever `Cargo.lock` changes (see below).
4. Fork [`flathub/flathub`](https://github.com/flathub/flathub), create a branch
   named `app.mediasort.MediaSort`, add `app.mediasort.MediaSort.yml` and
   `cargo-sources.json` at the repository root, and open a pull request.
5. Flathub reviews the submission. After it is merged the manifest moves to
   `flathub/app.mediasort.MediaSort`, builds start automatically, and future
   updates are a pull request to that repository.

## Regenerating `cargo-sources.json`

Run this after any change to `Cargo.lock`:

```bash
python3 flatpak-cargo-generator.py Cargo.lock -o flatpak/cargo-sources.json
```

`flatpak-cargo-generator.py` comes from
[flatpak-builder-tools](https://github.com/flatpak/flatpak-builder-tools)
(`cargo/`, pinned in `.github/workflows/flatpak-sources.yml`), and needs
`aiohttp` and `tomlkit`. The `Flatpak Cargo Sources` workflow regenerates it and
fails if the committed file differs, so it cannot silently drift from
`Cargo.lock`.

## Version maintenance

Every release touches the Flatpak packaging:

- bump `tag` (and `commit`) of the `media-sort` source in the manifest;
- add a matching `<release version="…" date="…"/>` to the metainfo;
- regenerate `cargo-sources.json` if `Cargo.lock` changed.

The `x-checker-data` blocks let Flathub's external data checker open automated
bump PRs for mpv, libass, libplacebo and the app itself.

The `.github/workflows/flatpak-sources.yml` (`Flatpak Cargo Sources`) workflow
regenerates `flatpak/cargo-sources.json` from the PR's `Cargo.lock` and fails if
it does not match the committed file, so a dependency bump that forgets to
regenerate it is caught before the Flathub build. It uses the `pull_request`
event rather than `pull_request_target` (which runs with a write token and is
blocked by GitHub's default event policy for public repositories), so:

- a PR **from a branch in this repository** gets the regenerated file committed
  and pushed to the branch (plus a comment);
- a PR **from a fork** cannot be written to (the token is read-only), so the
  patch is put in the job summary and the regenerated file is uploaded as a
  `flatpak-cargo-sources` artifact — neither needs any local tooling.

Set a `FLATPAK_SOURCES_TOKEN` secret (fine-grained PAT or GitHub App token with
`contents: write` and `pull-requests: write`) so its pushes re-trigger CI —
pushes made with the default `GITHUB_TOKEN` do not.

## Known follow-ups

- **Screenshots:** `website/public/screenshots/*.png` are stills extracted from
  the rendered demo videos (the automation HUD was painted out). The metainfo
  references them at `https://mediasort.app/screenshots/…`, so the **website
  must be deployed before the Flathub build** — Flathub mirrors the URLs at
  build time and the build fails if they are unreachable.
- **Portals:** deletion goes through `org.freedesktop.portal.Trash`, and under
  Flatpak *open externally* and *reveal in file manager* go through
  `org.freedesktop.portal.OpenURI` — the session-bus policy allows an app to
  talk only to its own name, `org.freedesktop.DBus` and
  `org.freedesktop.portal.*`, so the native `dbus-send`
  (`org.freedesktop.FileManager1`) route cannot work there at all. `OpenURI`
  has no select-a-file call, so revealing opens the containing folder rather
  than highlighting the entry. Outside Flatpak both keep their native
  implementations.
