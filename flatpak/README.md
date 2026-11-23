# Flatpak / Flathub packaging

This directory holds the Flathub packaging for **Media Sort**
([Flathub](https://flathub.org)):

| File | Purpose |
| --- | --- |
| `app.mediasort.MediaSort.yml` | The Flatpak build manifest (the file submitted to Flathub). |

The vendored Rust crates are **not** checked in: the manifest's second source is
a `cargo-vendor.tar.gz` release asset built by `scripts/build-cargo-vendor.sh`
and attached to each release by `release.yml` (see below). Flathub builds
offline from it.

Upstream metadata lives with the other Linux packaging files:

- `packaging/linux/app.mediasort.MediaSort.metainfo.xml` — AppStream metadata (Flathub store listing).
- `packaging/linux/app.mediasort.MediaSort.desktop` — desktop entry.
- `packaging/linux/icon.svg` — the icon (installed as `app.mediasort.MediaSort.svg`).

## Application id

The id is **`app.mediasort.MediaSort`**, the reverse-DNS of `mediasort.app`
(`website/public/CNAME`), which makes the app eligible for the Flathub
*Verified* badge: add the token Flathub gives you to
`website/public/.well-known/org.flathub.VerifiedApps.txt` and click *Verify*. The
id is also the runtime Wayland/X11 app id, so keep the metadata file names, the
metainfo `<id>`, the desktop `Icon=` and the manifest `app-id` in sync.

## How it is built

- **Runtime/SDK:** `org.freedesktop.Platform` / `org.freedesktop.Sdk` `26.08`.
  Flathub requires the current runtime; bump this when a new yearly branch lands.
- **Rust:** the `org.freedesktop.Sdk.Extension.rust-stable` SDK extension.
- **libmpv:** not in the runtime, so `mpv` is built from source together with its
  unconditional build dependencies that the runtime lacks — `libass` and
  `libplacebo`. Everything they link against comes from the runtime/SDK (ffmpeg,
  dav1d, libva, PulseAudio/PipeWire/ALSA, freetype/fontconfig/harfbuzz/fribidi,
  vulkan/shaderc/lcms2), so no codec stack is built here. The app drives libmpv's
  software render API, so mpv needs no display/GPU output of its own.
- **Hardware decoding (NVDEC / VA-API):** mpv gates VA-API behind a display
  back-end, and the runtime misses a dependency for each — `wayland-protocols`
  for Wayland and `libXpresent` for X11. Both are built as small modules so
  copy-back decoding (`nvdec-copy,vaapi-copy,...`) works on either session, while
  skipping FFmpeg's `vulkan-copy` decoder (`auto-copy` would probe it first, and
  it crashes on rapid teardown).
- **zimg:** built as a module; the software render path converts every frame with
  it, and swscale instead collapses 4K throughput (~7 ms vs ~66 ms per frame), so
  phone/camera 4K footage is unwatchable without it.
- **ffmpeg CLI:** the runtime's `/usr/bin/ffmpeg` is what `find_ffmpeg()` uses for
  video thumbnails.
- **Audio:** the manifest builds `media-sort-backend`'s `pulse` feature, so audio
  writes straight to PulseAudio. Inside the sandbox cpal's ALSA stream cannot
  deliver through the ALSA pulse plugin (every read fails with
  `snd_pcm_avail_delay ... 'I/O error (5)'`) while mpv and ffmpeg use the same
  socket fine; without the feature the player falls back to cpal and the default
  device. Video sound was never affected (mpv talks to PulseAudio directly), and
  the snap enables the same feature.
- **No self-updater:** the `velopack` feature is not built — Flatpak owns updates,
  which is also why the manifest needs no network access.

## Sandbox integration

Everything that reaches the host goes through the freedesktop portals: deletion
uses `org.freedesktop.portal.Trash` (so files land in the *host* trash, not the
sandbox-private `~/.var/app` one), and *open externally* / *reveal in file
manager* use `org.freedesktop.portal.OpenURI` (the session bus only exposes the
portal, so the native `dbus-send` route cannot work). Because the app sorts real
files it needs `--filesystem=home` and the removable-media mounts rather than a
document-portal view. Outside Flatpak the native implementations run unchanged;
the details live in `AGENTS.md` and the code.

## Permissions

The app moves, renames, copies and deletes files in a folder the user opens,
including into pinned destinations elsewhere, so it needs real filesystem access:
the document portal's single-folder FUSE view breaks the watcher (inotify), turns
cross-grant moves into copy-then-delete, and has no paths above the picked
folder. Hence `--filesystem=home` plus the removable-media mounts, with
`--device=dri` (wgpu/Vulkan and VA-API) and `--socket=pulseaudio`; there is no
`--filesystem=host` and no network.

`--filesystem=home` is the grant Flathub scrutinises: the justification above
must be in the submission and a human reviewer approves it — a clean
`flatpak-builder-lint` run does not by itself accept it.

## NVIDIA

Nothing in the manifest can carry the NVIDIA userspace driver: it must match the
user's kernel driver, so Flatpak ships it as a runtime extension. Without it the
sandbox uses Mesa `llvmpipe` and every frame composites on the CPU (a slideshow).
Install the extension matching `/sys/module/nvidia/version`:

```bash
# GL/EGL/Vulkan userspace — required for GPU rendering at all
flatpak install flathub org.freedesktop.Platform.GL.nvidia-<major>-<minor>-<patch>
# nvidia-vaapi-driver, so mpv's vaapi-copy decoder can decode on the GPU
flatpak install flathub org.freedesktop.Platform.VAAPI.nvidia//26.08
```

e.g. driver `615.78.08` → `org.freedesktop.Platform.GL.nvidia-615-78-08`; the GL
extension's `1.4` branch is normal for the `26.08` runtime. Both installs are
user-visible system-wide or with `--user` (which needs a user Flathub remote:
`flatpak remote-add --user flathub https://flathub.org/repo/flathub.flatpakrepo`).
Symptom of a missing extension: `flatpak run` logs
`Unable to find extension: VK_EXT_physical_device_drm` and
`MESA-EGL: egl: failed to create dri2 screen`, and the wgpu adapter list shows
only `llvmpipe` — an environment gap, not an app or manifest bug (the manifest
already gets `--device=dri` and Flatpak passes `/dev/nvidia*` in). The extension
binds when the app is launched, so an instance that was already running when the
driver was installed keeps the software stack and must be restarted. A working
setup shows `NVIDIA … DiscreteGpu` in the adapter list and `libva` loading
`…/nvidia_drv_video.so` for the `vaapi-copy` decoder.

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

The checked-in manifest points at a released tag and that release's
`cargo-vendor.tar.gz` asset, so a build of uncommitted work must point **both**
sources at local paths — exactly what `.github/workflows/flatpak-build.yml` does
for any change to `Cargo.lock`, a `Cargo.toml`, `flatpak/**` or
`scripts/build-cargo-vendor.sh`:

```bash
bash scripts/build-cargo-vendor.sh "$PWD/cargo-vendor.tar.gz"
# in a copy of the manifest: the app source becomes `type: dir` / `path: ..`,
# and the vendor source becomes `type: archive` with
# `path: /abs/path/cargo-vendor.tar.gz` and `strip-components: 0`.
```

That CI build is expensive, so it is not a required check. The module builds with
`cargo build --release --offline --locked` and `CARGO_HOME` pointing at the
extracted `cargo/`. Lint the manifest (the tool ships in `org.flatpak.Builder`):

```bash
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest flatpak/app.mediasort.MediaSort.yml
flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo build-dir/repo
```

## Submitting to Flathub

Flathub hosts the manifest and builds it on its infrastructure; nothing here is
published automatically.

1. **Cut a stable release that contains this packaging** — Flathub's stable repo
   accepts only stable software, and the manifest installs metadata from the
   tagged tree, so the tag must contain `packaging/linux/app.mediasort.MediaSort.*`
   and `flatpak/`.
2. **Point both manifest sources at that release.** The `media-sort` source takes
   the tag *and* its commit (`git rev-parse <tag>^{commit}`) — the commit is
   required, not preferred: Flathub's linter rejects a tag-only git source with
   `module-media-sort-source-git-no-commit-with-tag`. The vendor source takes the
   same tag's `cargo-vendor.tar.gz` URL and its sha256 (download it and
   `sha256sum` it, or let the checker fill it in after the first submission). The
   metainfo `<release>` entry is written by `scripts/release.sh` when the tag is
   cut.
3. **Add the manifest** to a branch of your
   [`flathub/flathub`](https://github.com/flathub/flathub) fork named
   `app.mediasort.MediaSort` (the vendor tarball is fetched from the release, not
   committed) and open a pull request.
4. **Review.** Once merged the manifest lives in `flathub/app.mediasort.MediaSort`,
   builds run automatically, and later updates are pull requests there. The first
   submission also gets the `--filesystem=home` justification reviewed.

## The vendored-crate tarball

Flathub builds offline, so it needs every crate `Cargo.lock` resolves to. That is
not a checked-in file: `scripts/build-cargo-vendor.sh` runs
`cargo vendor --locked` and packs a deterministic tarball whose root is `cargo/`
(`cargo/config.toml` plus `cargo/vendor/<crate>`), consumed as a `type: archive`
source with `strip-components: 0`.

It is built automatically: `release.yml`'s `publish` job builds it from the tag's
lock and attaches it to the release (failing the run if it cannot), and
`.github/workflows/flatpak-build.yml` builds it from the checked-out lock for
validation. Build it locally with:

```bash
bash scripts/build-cargo-vendor.sh              # -> ./cargo-vendor.tar.gz (gitignored)
bash scripts/build-cargo-vendor.sh /tmp/x.tar.gz
```

The archive is reproducible (sorted entries, fixed mtime/ownership, `gzip -n`), so
its sha256 is stable for a given lock; a tarball matches the manifest's pinned
sha256 only when built from the same tag.

## Version maintenance

- the **dependency modules** — `mpv`, `libass`, `libplacebo`, `zimg`,
  `wayland-protocols`, `libXpresent` — are bumped automatically:
  `scripts/bump-flatpak.py` re-checks each upstream with the repository's
  14-day soak and rewrites the module's tag *and* the commit that tag resolves
  to (Flathub rejects a tag-only source), and
  `.github/workflows/flatpak-bump.yml` runs it weekly, builds the result with
  the same gate as `flatpak-build.yml`, and opens a draft PR on
  `chore/flatpak-bump`. Run it by hand with `scripts/bump-flatpak.py` (report),
  `--check` (exit 2 when behind), or `--apply`;
- the **`media-sort` source** (`tag` and `commit`) and the **vendor source**
  (`url`/`sha256`) are release-owned: bump them together when a release is cut
  — by hand, or let Flathub's external data checker do it (`x-checker-data` is
  on both sources);
- `release.yml` attaches `cargo-vendor.tar.gz` to every tag;
- the metainfo `<release>` entry is written by `scripts/release.sh` when the tag
  is cut, so only a hand-made tag needs it added manually;
- bump the Freedesktop runtime when a new yearly branch lands.

## Known follow-ups

- **Screenshots:** `website/public/screenshots/*.png` are stills from the rendered
  demo videos; the metainfo references them at
  `https://mediasort.app/screenshots/…`, so the **website must be deployed before
  the Flathub build** — Flathub mirrors the URLs and the build fails if they are
  unreachable.
