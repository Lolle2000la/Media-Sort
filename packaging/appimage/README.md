# Self-contained Linux AppImage payload

The release AppImage used to ship only the `media-sort-gui` binary and resolve
`libmpv`, `libdav1d`, `libasound` and `ffmpeg` from the host. On a machine
without `libmpv` it died with

```
error while loading shared libraries: libmpv.so.2: cannot open shared object file
```

and, because it was packed on `ubuntu-24.04`, it also required glibc 2.39 (the
AppImage catalog's automated test reported exactly that: *"references glibc
2.39"*). This directory fixes both problems:

* **everything is compiled in an `ubuntu:22.04` container** – the GUI binary,
  `libmpv`, FFmpeg, dav1d, libass, zimg, wayland, wayland-protocols and
  libplacebo – so the whole payload's glibc floor is **2.35**;
* **libmpv, the `ffmpeg` CLI and their dependency closure are bundled into the
  AppDir** with `$ORIGIN`-relative RPATHs, so nothing is resolved from the host
  except the host-provided allowlist in [`excludelist`](excludelist): glibc, the
  GPU driver stack, and the plugin hosts the host's own plugins bind to.

Every source is a sha256-pinned upstream tarball in [`versions.env`](versions.env);
no binaries are committed to git and nothing is downloaded at runtime.

## Files

| File | Purpose |
| --- | --- |
| `versions.env` | Every upstream pin in one place (mpv, FFmpeg, dav1d, libass, zimg, wayland, libplacebo, wayland-protocols) with URL + sha256 (+ release commit where upstream publishes one, and the `RUST_VERSION` the AppImage's own compiler is pinned to). **Bump versions here and nowhere else.** |
| `build-in-container.sh` | Runs in `ubuntu:22.04`: apt deps → dav1d → FFmpeg → libass → zimg → wayland → wayland-protocols → libplacebo → libmpv → `cargo build` → stage. Per-component logs, per-component stamps over the version, the source pin (URL + sha256) *and* the build flags (re-runs are cheap; a flag or pin change rebuilds). |
| `bundle.sh` | `stage` (container side) computes the closure into `target/appimage-stage`; `install` (host side) copies it into the AppDir, writes the RPATHs and fails on any unresolved `DT_NEEDED`. |
| `excludelist` | The host-provided allowlist: what must never be bundled (glibc/loader, the GL/Vulkan/GPU-driver stack, and the plugin hosts the host's plugins bind to) with a one-line reason per entry. |
| `excludelist.sh` | The policy *code* for that list (`is_excluded`, `host_reason`, the driver globs, the dlopen-only names), sourced by `bundle.sh` and `verify.sh` so the two cannot disagree about the same name. |
| `pack-appimage.sh` | The two-pass `vpk pack` flow (staging → pack → unpack → bundle → verify → desktop surgery → final pack), shared by `release.yml` and `appimage.yml`. Fills the runtime's AppImageUpdate information before the first pack and generates the `.zsync` after the last. |
| `appimage-update-info.sh` | Fills the `.upd_info` section of vpk's vendored runtime (so the released AppImage *and* the copy inside the `.nupkg` carry it), generates the `.zsync` from the final AppImage, and is the gate for both (`--selftest` probes the pure logic: channel selection, update-info string, section parsing, `.zsync` headers). |
| `verify.sh` | The gate: glibc floor (incl. `GLIBC_ABI_*`), NEEDED resolution, the library-name manifest of the whole payload, `libmpv.so.2` presence, real `ffmpeg` encode/decode (h264, hevc, vp9 and AV1 through libdav1d) plus its licence rule, GUI start under Xvfb, and (on a sealed AppImage) usable AppImageUpdate information with a matching `.zsync` beside it (`--selftest` probes the exit and licence predicates; the host floor is printed on every run). |
| `testdata/` | Committed decode fixtures (`h264.mp4`, `hevc.mp4`, `vp9.webm`, ~24 KB total) that `verify.sh` decodes with the bundled `ffmpeg`; its `README.md` has the exact regenerating commands. |

CI wiring: `.github/workflows/appimage.yml` (packaging-only check, **x64 and
arm64**) and the weekly bump (`.github/workflows/appimage-bump.yml`, below) run
the build/verify steps through the composite action
`.github/actions/appimage-gate`, so the two cannot disagree about what "passes".
The gate covers both shipped Linux arches: an arm64 build that only fails on the
release tag would block `publish` for every platform, so it is built here.

`.github/workflows/release.yml` (Linux matrix entries) deliberately keeps its
own **inline** copy of those steps instead: it runs inside a 5-platform matrix on
tag pushes, with the build interleaved with signing and uploads (see the note in
`.github/actions/appimage-gate/action.yml`). It calls the same
`pack-appimage.sh`, and it runs the same `verify.sh` — on the packed
`dist/MediaSort.AppDir` and again on the sealed `.AppImage` — so the tag build is
gated identically; only the wiring differs. Keep it that way: a change to the
flags or the gate belongs in `packaging/appimage/` and the composite action, not
in a fifth copy of the logic.

## Rebuilding locally

From the repository root (docker is the only host requirement):

```bash
docker run --rm \
  -v "$PWD:/work" -w /work \
  -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  -e TARGET=x86_64-unknown-linux-gnu \
  ubuntu:22.04 bash /work/packaging/appimage/build-in-container.sh
```

That writes `target/<target>/release/media-sort-gui` and the staged payload
(`target/appimage-stage/{lib,bin}`), and prints a summary of every pinned
version, sizes and log locations. Timings on a 32-core machine: ~4 min apt +
dav1d/FFmpeg/libass/zimg/wayland/libplacebo/mpv, ~4 min for the Rust build.

The container runs as root (apt needs it) and chowns its host-visible outputs
back to `HOST_UID:HOST_GID`. Its cache — downloaded tarballs, unpacked sources,
per-component logs, stamps and the **install prefix** — lives in
`.appimage-build/` (git-ignored). Re-running rebuilds nothing whose pin *and*
build flags are unchanged, and always re-runs `cargo build` (cargo is
incremental, so an unchanged tree only costs a fingerprint check — skipping it
on mere file presence silently shipped the binary from before a source edit).
A component stamp is keyed on the hash of the version, the source pin (its
`URL` + `SHA256`, i.e. the exact bytes that get built) **and** its build
arguments, so editing a meson/configure flag in `build-in-container.sh` -- or
repointing `MPV_URL`/`MPV_SHA256` at different content under an unchanged
`MPV_VERSION` -- rebuilds exactly that component. The prefix deliberately sits in
the workspace instead of `/usr/local`: every CI step is a `docker run --rm`, so
`/usr/local` is empty
again on the next run while the stamps would still say "already built".
`PKG_CONFIG_PATH`, `LIBRARY_PATH` and `LD_LIBRARY_PATH` point at it, and
`dav1d-sys` finds it through the same `pkg-config` path.
Force a rebuild of one component with
`docker run … -e FORCE_COMPONENTS="mpv libplacebo"`, or everything with
`-e FORCE=1`.

The Rust toolchain is pinned too: `RUST_VERSION` in [`versions.env`](versions.env)
(installed with `--default-toolchain "$RUST_VERSION"`, not `stable`), because
the AppImage is the one artifact compiled here rather than on a runner — a
floating `stable` would change the payload's ABI surface between two runs of the
same commit. **That pin governs the Linux AppImage only**; Windows, macOS and the
test matrices keep using `dtolnay/rust-toolchain@stable`.

Then bundle and verify against a real AppDir:

```bash
# inside an extracted AppImage AppDir (see release.yml) or a hand-made one
bash packaging/appimage/bundle.sh install squashfs-root target/appimage-stage
bash packaging/appimage/verify.sh squashfs-root
```

`verify.sh` also accepts a packed `.AppImage` (it extracts it with
`--appimage-extract`, so FUSE is not needed) and needs `xvfb` on the host for
the GUI check; `--skip-gui` disables that one check. On a packed `.AppImage` it
additionally requires the sibling `<name>.zsync` and hard-fails without it,
because a shipped AppImage must carry update information; on an extracted AppDir
it does not check that (an AppDir has no runtime to carry it).

To produce the sealed AppImage locally (`vpk`, `mksquashfs`, `patchelf`,
`readelf` from binutils and `zsyncmake` from zsync on PATH, `xvfb` for the gate
inside it):

```bash
TARGET_DIR=target/x86_64-unknown-linux-gnu/release \
  bash packaging/appimage/pack-appimage.sh 0.0.1-local linux-x64
```

That script is the two-pass flow `release.yml` and `appimage.yml` both call
(staging → `vpk pack` → `--appimage-extract` → `bundle.sh install` →
`verify.sh` → `.desktop` fixup → second `vpk pack`).

## Why a container, and why two phases

The payload's glibc floor is the glibc of whatever compiled it. Building on
`ubuntu-24.04` (2.39) is what broke the AppImage catalog test, so the build
runs in `ubuntu:22.04` (2.35). The same container also means the Rust link
cannot accidentally pick up a host `libmpv`: the release runner installs only
`squashfs-tools`, `patchelf`, `xvfb`, `xdotool`, `zsync` (for `zsyncmake`) and
`binutils` (for `readelf`), plus the host-provided libraries the gate needs to
start the app (`libasound2t64` — the Ubuntu 24.04 name of the ALSA runtime, where
`libasound2` is a virtual package — and `libdrm2`) — no `-dev` packages at all.

`patchelf` and the AppDir, however, live on the host (vpk produces the AppDir
there), so the work is split: the container *resolves and stages* the closure
(`bundle.sh stage`, using `ldd` against the real prefix) and the
host *installs and patches* it (`bundle.sh install`). Only the container ever
compiles anything.

The staged libraries are named by **SONAME** (`libmpv.so.2`,
`libavcodec.so.62`, …), which is what the loader looks for at runtime; the
versioned symlinks and `.so` development links stay in the prefix.

## Build flags, and why

FFmpeg (`--enable-shared --disable-static --enable-pic --disable-doc
--disable-debug --disable-ffplay --disable-ffprobe --enable-gpl
--enable-version3 --disable-autodetect --disable-avdevice --enable-zlib
--enable-libdav1d`):

* **GPLv3** – `--enable-gpl --enable-version3`. Media-Sort is
  AGPL-3.0-or-later, and GPLv3 §13 explicitly permits combining GPLv3 code with
  AGPLv3 code (the same reason mpv's GPL build is fine, below); mpv's own
  `-Dx11=enabled` already forces `-Dgpl=true` anyway, so the payload has been
  GPL for its X11 support from the start. `--enable-version3` is the part that
  matters: it upgrades the GPL parts from "v2 or later" to GPLv3, which combines
  unambiguously with AGPLv3. `verify.sh` enforces the pair — `--enable-gpl`
  without `--enable-version3` (a GPLv2-only codec stack, incompatible with
  AGPLv3) fails the gate.
* **No GPL encoder library is linked** – no x264, x265 or libxvid. Nothing in
  the app encodes video: `extract_frame` pipes a PNG out, and that is the native
  PNG encoder. x265 alone would add ~8 MB to the payload for no use. This is a
  size decision, not a licensing wall: the GPL build already ships, so enabling
  an encoder later needs a deliberate payload-size decision and nothing else.
* `--enable-zlib` is what makes the **PNG encoder** exist, which is what video
  thumbnails need (`ffmpeg -frames:v 1 out.png`).
* `--disable-autodetect` stops the build from linking whatever `-dev` package
  happens to be installed (libva, libx264, …); every external library is named
  explicitly. Likewise `--disable-avdevice`, which nothing uses.
* `--disable-ffprobe`: the thumbnail path pipes PNG straight out of `ffmpeg`,
  it never runs `ffprobe`.
* `--enable-libdav1d`: mpv's AV1 path and the Rust `dav1d-sys` AVIF decoder both
  use libdav1d, so it is pinned and built here (FFmpeg 9 also has a native AV1
  decoder, which is why `verify.sh` asserts the `libdav1d` decoder is present and
  decodes the fixture with it explicitly rather than relying on the native one).
  libvpx is *not* enabled – native VP8/VP9 decoders cover WebM – and GPL encoders
  are out.

mpv (`-Dlibmpv=true -Dcplayer=false -Dgl=disabled -Degl=disabled
-Dvulkan=disabled …`, see the script):

* `-Dcplayer=false`: the `mpv` CLI is useless in an AppImage and would be a
  second copy of everything.
* `-Dgpl` stays at its default (`true`): **mpv's X11 support is GPL-only** and
  `-Dgpl=false -Dx11=enabled` is a hard meson error. X11 (and with it VA-API on
  X11 sessions) is worth more than an LGPL mpv, and a GPL libmpv links into an
  AGPL-3.0-or-later application without a licensing conflict — see "Licensing"
  below. It is also why the FFmpeg build being GPLv3 costs nothing new.
* `-Dgl/-Degl/-Dvulkan/-Dshaderc=disabled`: the app drives libmpv's **software**
  render API (`MPV_RENDER_API_TYPE_SW`), so mpv needs no display output of its
  own. This is also what keeps libplacebo's Vulkan and shaderc dependencies out.
* `-Dx11=enabled -Dwayland=enabled -Dvaapi=enabled -Dvaapi-x11=enabled
  -Dvaapi-wayland=enabled` (with `-Dvaapi-drm=disabled`): mpv gates VA-API
  behind a display backend, so both are enabled to get hardware decoding on X11
  and Wayland sessions alike. That is why `wayland-protocols` is built (mpv
  needs >= 1.31, jammy ships 1.25) and `libXpresent`/`libXss` headers are
  installed.
* `-Dzimg=enabled`: the software renderer converts every frame with zimg when
  it is present and with swscale when it is not. At 4K that is ~7 ms vs ~66 ms
  per frame (30 fps vs ~6.6 fps with hardware decoding).
* `-Dalsa=enabled -Dpulse=enabled`: audio output for video. `pipewire` is
  disabled on purpose – every PipeWire desktop also exposes the PulseAudio
  socket, and it keeps `libpipewire` (and its dependency chain) out of the
  payload.
* `-Dlibarchive/-Duchardet/-Djpeg/-Dlcms2/-Dcdda/-Ddvdnav/-Dlibbluray/-Dlua/
  -Djavascript/-Dvapoursynth/-Dsixel/-Dcaca/-Drubberband/-Dxv/-Djack/-Dsndio=disabled`:
  features a file sorter does not use; each one is another shared library.
* `--default-library=shared`, `--libdir=lib` for every meson component (the
  long form, because meson's Debian default is `lib/<triplet>`); FFmpeg's
  autotools configure is the one exception and gets
  `--libdir="$PREFIX/lib"`, since it has no multiarch notion here.

**libplacebo cannot be skipped.** mpv 0.41.0's `meson.build` asks for it
unconditionally (`dependency('libplacebo', version: '>=6.338.2')`, no
`required:` and no `-Dlibplacebo` switch), so it is built with
`-Dvulkan=disabled -Dopengl=disabled -Dd3d11=disabled -Dshaderc=disabled
-Dglslang=disabled` – link-only, no GPU stack. libplacebo needs meson >= 0.63
and jammy ships 0.61.2, so the script pip-installs meson 1.5.2.

Also note: mpv 0.41 renamed `meson_options.txt` to `meson.options` and dropped
options older recipes pass (`-Dmanpage-build`, `-Dencoding`); passing one is a
hard meson error.

## Bumping versions

Everything lives in [`versions.env`](versions.env). Bumping is automated by
[`scripts/bump-appimage.sh`](../../scripts/bump-appimage.sh), which queries every
upstream, downloads the exact archive each new pin would point at, and computes
the sha256 from those bytes — the part Renovate cannot do (it can find a new
version, not a hash for an archive it never downloads).

```bash
scripts/bump-appimage.sh                # dry run: report table, downloads nothing
scripts/bump-appimage.sh --check        # exit 2 when a pin is behind, 0 when not
scripts/bump-appimage.sh --apply        # rewrite versions.env (temp file + mv)
scripts/bump-appimage.sh --component mpv --min-age-days 0 --apply
```

It covers `mpv`, `ffmpeg`, `dav1d`, `libass`, `zimg`, `wayland`,
`wayland-protocols`, `libplacebo` and `rust`, keeps each component's existing
URL shape (tag archive vs. release asset), updates the paired `*_COMMIT` where
`versions.env` records one, refuses prereleases unless `--allow-prerelease`, and
orders versions with `sort -V` (`1.5.4` is older than `1.5.10`), and never
stops a tag scan on a non-version tag (`main`, `latest`, `nightly` sorts above
every release, so it is skipped rather than mistaken for the end of the list).
`--json` prints exactly one JSON document — also with `--check`, where the exit
status carries the verdict — so its output pipes straight into `jq`. Timestamps
are parsed with GNU `date -d` (`gdate` is used when it is on `PATH`, i.e. macOS
with coreutils); a BSD `date` is refused up front, because it would silently make
every candidate look too young and report "0 updates". Comments,
ordering and formatting in `versions.env` are preserved. If **any** query, commit
lookup, download or hash fails, nothing is written at all — the file stays
byte-identical and the run exits non-zero. A component whose upstream cannot be
reached is reported as `unknown` and does not abort the others (in a dry run);
`--apply` refuses to write while any selected component is unknown.

### The 14-day soak

`--min-age-days` (default **14**) skips anything younger than that, matching the
repository's Renovate policy (`minimumReleaseAge` in `renovate.json`, see
AGENTS.md "Dependency updates (Renovate)"): a malicious or broken publish gets
the same two weeks to be caught before it can reach a pin. The date comes from
`published_at` for a GitHub release, the tag commit's `committer.date` for a
GitHub tag (the tags API has no date of its own), `commit.committed_date` for a
GitLab tag, the release tarball's `Last-Modified` for FFmpeg (ffmpeg.org's index
has no dates), and the stable channel manifest's `date` for Rust.
`--min-age-days 0` disables the soak for a deliberate same-day bump.

### The scheduled workflow

[`.github/workflows/appimage-bump.yml`](../../.github/workflows/appimage-bump.yml)
runs `--check` weekly (Mondays 06:00 UTC) and, when something is behind,
re-runs `--apply`, commits the result on `chore/appimage-bump`, runs the same
build + verify gate as [`appimage.yml`](../../.github/workflows/appimage.yml)
(the shared composite action `.github/actions/appimage-gate`), and then opens a
**draft** PR — or, when a PR with that head branch is still open, force-pushes
the branch and rewrites that PR's body instead of opening a second one. The job
fails when the gate fails, but the PR is still created: the failing pin
combination is then visible instead of silently retried.

Two details of that job are deliberate:

* the force-push takes its lease against the branch tip **read at job start**,
  not against a ref re-fetched right before the push (`--force-with-lease=<ref>:
  <tip>`), so a commit a maintainer pushed to `chore/appimage-bump` while the
  gate was running fails the push instead of being silently overwritten; with no
  bump branch at all it does a plain non-force push, which creates the branch
  and still refuses to overwrite one that appeared since;
* `--apply` with nothing left to write (a bump PR merged between `check` and
  `bump`, or a release that aged past the soak in between) is a **green no-op**
  with a notice, not a red run: `.github/workflows/appimage-bump.yml` only fails
  when apply claims updates it did not write.

`workflow_dispatch` checks out the ref it was dispatched on, so dispatching a
PR's branch runs that branch's scripts and workflow through the real gate before
merge; only a dispatch from `master` (like the schedule) commits, pushes or
touches the PR.

**`APPIMAGE_BUMP_TOKEN`.** A branch pushed with the workflow's own
`GITHUB_TOKEN` does not trigger other workflows, so the PR would arrive with no
status checks at all and the in-job gate would be the only thing that ever tested
it. Add a repository secret `APPIMAGE_BUMP_TOKEN` (a fine-grained PAT with
`Contents: read/write` and `Pull requests: read/write` on this repository) to push
and open the PR as a real user, so the normal workflows fire on it. Without the
secret everything still works — the in-job gate is authoritative — the PR just
carries no CI.

### Manual fallback

The script is a convenience, not a gate: `build-in-container.sh` only ever reads
`versions.env` and verifies the sha256 it finds there, so editing the file by hand
is still fully supported and is the right answer when a bump needs a combination
the script would not pick. Change the version **and** the URL/sha256 (and the
commit, where upstream publishes a tarball from a tag whose commit we can record)
— `curl -fL <url> | sha256sum` gives the value — then run
`.github/workflows/appimage.yml` (or the docker command above). If a component's
build fails, move to the previous release of that component rather than patching
its sources; keep the combination that the release actually ships. FFmpeg's major
version is the most likely to break mpv.

`RUST_VERSION` follows the same rule: bump it deliberately (and only after
checking the workspace still builds with that compiler), because the AppImage's
ABI surface is whatever that release produces. That pin governs the Linux
AppImage only; Windows, macOS and the test matrices keep using
`dtolnay/rust-toolchain@stable`.

### Flatpak drift

[`flatpak/app.mediasort.MediaSort.yml`](../../flatpak/app.mediasort.MediaSort.yml)
pins **the same projects separately**, and `bump-appimage.sh` deliberately does
not touch it: it is a separate Flathub submission built against a different
runtime (`org.freedesktop.Platform`, not Ubuntu 22.04), with its own tag/commit
pair per module. Bumping both from one query would propose a combination neither
was tested with, so the Flatpak manifest is bumped by hand and gated by
`.github/workflows/flatpak-build.yml`. The two pin sets are expected to drift;
when they do, the AppImage's `versions.env` is the one that defines what the
Linux release artifact contains.

Choices as of the initial version: **mpv v0.41.0** (the newest stable release),
**FFmpeg 9.0.2** (newest stable release, and mpv 0.41 builds against it),
**dav1d 1.5.4** (newest; also satisfies `dav1d-sys`'s `>= 1.3` pkg-config probe
so the Rust AVIF decoder links the bundled library), **libass 0.17.5**,
**zimg release-3.0.6**, **wayland 1.26.0** (mpv's Wayland backend needs
wayland-client >= 1.21, jammy ships 1.20), **libplacebo v7.360.1**,
**wayland-protocols 1.49**, and **Rust 1.99.1** for the compiler.

## What is bundled, and what is not

Bundled (`usr/lib`, by SONAME): `libmpv.so.2`, `libavcodec/format/util/filter`,
`libswscale`, `libswresample`, `libavfilter`, `libdav1d`, `libass`, `libzimg`,
`libplacebo`, `libfreetype`, `libharfbuzz`, `libfribidi`, `libfontconfig`,
`libexpat`, `libz`, `libgcc_s`, `libpulse`, `libX11`-family (including
`libXcursor`/`libXi`/`libX11-xcb`/`libxkbcommon-x11`, which winit only *dlopens*,
and `libXrandr`, which is a `DT_NEEDED` of libmpv -- nothing loads
`libXinerama.so.1`, so it is no longer staged), `libxcb`,
`libwayland-client`/`libwayland-cursor`, `libva*` (the API library, not the
driver), … plus `usr/bin/ffmpeg`.

Not bundled, by design (`excludelist`): glibc and the loader (`libc`,
`ld-linux-*`, `libm`, `libdl`, `libpthread`, `librt`, `libresolv`, `libnss_*`,
…), the graphics/driver stack (`libGL*`, `libEGL*`, `libOpenGL*`, `libGLX*`,
`libgbm*`, `libglapi*`, `libvulkan*`, `libwayland-egl*`,
`libdrm_amdgpu/intel/radeon/nouveau`, vendor drivers, `*_drv_video.so`), and the
**plugin hosts** described below (`libasound.so.2`, `libstdc++.so.6`,
`libdrm.so.2`). A GPU driver has to match the running kernel and hardware;
`libmpv` dlopens the VA-API driver at runtime, so only the API library ships.
`libwayland-egl.so.1` is Mesa's `wl_egl_window` glue and belongs with EGL, so it
is host-provided too.

### Plugin hosts stay on the host (measured, not assumed)

A library that the **host** dlopens a plugin into must not be bundled, even when
mpv or the app itself links it. The plugin binds to it *by SONAME*, and the
bundled (older) copy is already loaded under that SONAME by the time the plugin
resolves its symbols — so the plugin dies on a versioned symbol it needs:

```
/usr/lib/alsa-lib/libasound_module_pcm_pulse.so
  -> <AppDir>/usr/lib/libasound.so.2: version `ALSA_1.2.15' not found
/usr/lib/dri/radeonsi_drv_video.so
  -> <AppDir>/usr/lib/libstdc++.so.6: version `GLIBCXX_3.4.32' not found
     (required by /usr/lib/libSPIRV-Tools.so)
```

The first one is not cosmetic: the AppImage builds without the `pulse` feature,
so rodio → cpal → ALSA `default` → the host's pulse/pipewire plugin **is** the
audio path, and with the bundled `libasound` that plugin fails to load, leaving
audio silently dead on any current distribution. The second degenerates or kills
VA-API on Mesa systems. `libdrm.so.2` is host-provided for the same class of
reason: a host Mesa/VA-API driver links it, and bundling only works until that
driver needs a newer `drm*` symbol.

`libpulse.so.0` stays bundled, and it is the one **plugin-host exception** being
watched: the host's `libasound_module_pcm_pulse.so` has a hard `DT_NEEDED` on
`libpulse.so.0`, so it binds our copy exactly like `libasound.so.2` above. It is
bundled anyway because it is a hard `DT_NEEDED` of libmpv (`-Dpulse=enabled`) and
because loading the current distributions' plugin against it showed no symbol
drift (no `version 'PULSE_…' not found`). If a host plugin ever asks for a newer
`PULSE_*` version than our copy exports, it moves to the host-provided list.

The policy is reproducible in one command — with the payload's library directory
on `LD_LIBRARY_PATH`, load a host plugin the way the app's audio/driver stack
does:

```bash
AppDir=squashfs-root
# 1. fails against the old policy (libasound.so.2 bundled), succeeds when it is
#    host-provided:
LD_LIBRARY_PATH="$AppDir/usr/lib" \
  python3 -c 'import ctypes; ctypes.CDLL("/usr/lib/alsa-lib/libasound_module_pcm_pulse.so")'
# no `ALSA_*` version error may appear.
# 2. same for the C++ runtime and a host VA-API driver's helper library:
LD_LIBRARY_PATH="$AppDir/usr/lib" \
  python3 -c 'import ctypes; ctypes.CDLL("/usr/lib/libSPIRV-Tools.so")'
```

`verify.sh` enforces the consequence: the `lib*` names the shipped binary
mentions must be either bundled or listed in `excludelist` with a reason, and it
prints the resulting decision table on every run.

The floor that leaves for the host — glibc >= 2.35, `libstdc++.so.6`,
`libasound.so.2`, `libdrm.so.2` and the GPU/VA-API drivers — is what every
desktop distribution already ships; a bare `ubuntu:22.04` container has the
first two but not `libasound2`/`libdrm2`. `verify.sh` prints that floor on every
run (CI installs those libraries, so a "missing here" list alone would hide it),
noting for each entry whether it resolves on the machine running the gate.

Two entries that upstream's excludelist has, but this payload deliberately
ships:

* **`libgcc_s.so.1`** — the upstream AppImage excludelist would skip it, but
  newer distros ship a newer one and older ones can be too old for
  `libavcodec`'s `_Unwind_*` usage.
* **`libpulse.so.0`** — the one plugin-host exception: the host's ALSA pulse
  plugin binds our copy, but no symbol drift was measured and it is a hard
  `DT_NEEDED` of libmpv. See "Plugin hosts stay on the host" above.

Everything else in the upstream excludelist that is a hard `DT_NEEDED` of libmpv
(`libX11`, `libwayland-client`, `libfontconfig`, `libfreetype`, `libharfbuzz`,
`libfribidi`, `libz`) is bundled here too, because leaving them out is exactly
the bug this work fixes.

## Verification gate

`verify.sh` fails the build on any of:

* a `GLIBC_*` requirement above **2.35** in any packaged ELF, and any
  `GLIBC_ABI_*` requirement (they all imply >= 2.36; `-z pack-relative-relocs`
  adds `GLIBC_ABI_DT_RELR`, which the numeric `GLIBC_X.Y` pattern alone missed),
* a `DT_NEEDED` that resolves neither inside `usr/lib` nor through the
  excludelist (plus an `ldd` pass with an empty environment that catches
  "present but unusable"; a host-provided library missing on the *running* host
  is reported as part of the floor, not failed),
* a library name in the payload's `strings` manifest that is neither bundled nor
  on the host-provided allowlist with a reason. The manifest is derived from
  **every dynamic** payload ELF — the GUI binary and every bundled library;
  statically linked helpers (vpk's arm64 `UpdateNix` stub is static, the x64 one
  is not) have no loader and no RPATH, so they are skipped by both the manifest
  and the patchelf pass — plus the
  dlopen-only names in `excludelist.sh`, so a name that only appears *inside* a
  bundled library (`libva-x11` mentions `libatiadlxx.so`, `libva-wayland`
  `libwayland-emgd.so.1`) is decided too, and `bundle.sh`'s hand list cannot
  carry a library nothing loads without the gate noticing,
* a missing `usr/lib/libmpv.so.2`,
* `usr/bin/ffmpeg -version` failing, a GPLv2-only build (`--enable-gpl` without
  `--enable-version3`, which cannot combine with this AGPL application), a
  missing `h264`/`hevc`/`vp9`/`libdav1d` decoder, a failed PNG encode (the
  rawvideo pipe is used because `--disable-avdevice` removes the lavfi *input*), a failed
  mpeg4 decode round-trip, a failed `libdav1d` decode of the repository's real
  AV1 fixture, or a failed decode of any committed fixture in `testdata/`
  (h264, hevc, vp9 — one `ok:` line per codec),
* the GUI dying with `error while loading shared libraries` (or a versioned
  symbol failure) under `xvfb-run`, exiting 0, failing to execute (126/127),
  dying from SIGABRT/SIGFPE/SIGSEGV (134/136/139), exiting 101 (a Rust panic is
  always fatal — a startup panic in the window/render stack is precisely what
  this check exists to catch), or exiting early without one of the *specific*
  GPU/display diagnostics `gui_exit_is_expected()` matches (`no adapter was
  found`, `a suitable graphics adapter`, `Failed to open connection to X
  server`, …).
  Those patterns are deliberately narrow: the previous broad
  `adapter|vulkan|gpu|surface|egl|graphics|winit|display` alternation also
  matched panic *locations* like `panicked at iced_wgpu-…`, so an early panic
  in the window/render stack went green. `verify.sh --selftest` probes that
  predicate against fake logs and exit statuses without a payload. For an
  AppImage the GUI is launched with `--appimage-extract-and-run`; a later
  non-zero exit is only reported, since a GPU-less Xvfb cannot open a wgpu
  surface. When `xdotool` is present the check also reports whether a window
  named `Media Sort` appeared (informational only).

**What "decode works" means here.** The gate proves it with the payload's own
`ffmpeg`, not the host's: four real bitstreams are decoded end to end
(`-v error -i <file> -f null -`, failing on a non-zero exit *or* any stderr
line), each after asserting the file's reported `codec_name`:

| Codec | Fixture | Proof |
| --- | --- | --- |
| AV1 | `resources/MockState/mock 3.mp4` | decoded with an explicit `-c:v libdav1d`, so the bundled dav1d is exercised rather than FFmpeg's native AV1 decoder |
| H.264 | `testdata/h264.mp4` | native decoder |
| HEVC | `testdata/hevc.mp4` | native decoder |
| VP9 | `testdata/vp9.webm` | native decoder |

The three `testdata/` clips are generated *outside* the payload (the bundled
FFmpeg is built without x264/x265/vp9 encoders — nothing in the app encodes
video) and committed, because `crates/media-sort-backend/tests/fixtures/` holds
only images and audio and the AV1 clip alone says nothing about the other three
decoders. Regenerate them with the exact commands in
[`testdata/README.md`](testdata/README.md); they are ~24 KB in total. `ffprobe`
is not part of the payload (`--disable-ffprobe`), so the gate reads the codec from
`ffmpeg -i`'s stream banner — do not add ffprobe to make this easier.

The **host floor is printed on every run** — glibc (with the payload's maximum
requirement), `libstdc++.so.6`, `libasound.so.2`, `libdrm.so.2` and the
GPU/driver stack, each with a note on whether it resolves on the machine running
the gate. CI installs those libraries, so a "missing here" list alone used to
make the floor invisible in the release log.

## AppImageUpdate information and the `.zsync`

The AppImage carries
[AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate)
information, and a `.zsync` control file is published beside it, so
`AppImageUpdate-*.AppImage` can update it in place without downloading the whole
payload again.

**The runtime is patched before packing, not the finished AppImage.** `vpk pack`
prepends AppImageKit's type-2 runtime to the squashfs; that runtime already
carries a 1024-byte `.upd_info` ELF section, but vpk leaves it all zeroes
(`--appimage-updateinformation` prints an empty line). vpk has no option to fill
it, and upstream declined zsync support
([velopack#351](https://github.com/velopack/velopack/issues/351)), so
`appimage-update-info.sh patch-runtime` writes the string into vpk's **vendored
copy** of the runtime (`<dotnet tools>/.store/vpk/*/vendor/appimagekit-runtime-*`)
and `pack-appimage.sh` calls it before the first pack. Everything downstream —
pass 1, pass 2, the shipped `Releases/MediaSort-linux-*.AppImage`, the
byte-identical copy velopack wraps into the `.nupkg` as
`lib/app/MediaSort.AppImage`, and the SHA1/SHA256/Size hashes in
`releases.*.json` — is then derived from the patched bytes.

The trap this avoids: velopack's Linux self-updater extracts the `*.AppImage`
from the `.nupkg` and moves it over the running AppImage. Patching only the
finished `Releases/*.AppImage` would look correct until the first in-app update,
after which the AppImage has an empty `.upd_info` again and AppImageUpdate
silently stops working. Patching the `.nupkg` afterwards would instead mean
recomputing those hashes and re-signing. The patch is idempotent (it zeroes the
section before writing, so a shorter string leaves no stale tail) and proves
itself by running the runtime with `--appimage-updateinformation`.

The update-information string is
`gh-releases-zsync|<owner>|<repo>|<channel>|<asset>.zsync`. Channel selection is
**strict**: a stable version (no `-`) gets `latest`, a prerelease gets
`latest-pre`, so a stable user is never offered a prerelease. It is baked in per
build; there is no runtime substitution. The asset field is the exact `.zsync`
name — both architectures are attached to the same release, and a wildcard would
let AppImageUpdate pick the lexicographically greatest match, i.e. the wrong
architecture.

After the second pack, `pack-appimage.sh` runs
`zsyncmake -u <AppImage basename>` on the **final** bytes, verifies the control
file names that AppImage and carries its SHA-1, and gates the sealed AppImage
with `check`. `release.yml` uploads `Releases/*.AppImage.zsync` next to the
`.sig` files, after `vpk upload github`; `verify.sh` runs the same check on the
sealed AppImage, so a build without update information or without a matching
sibling `.zsync` fails instead of shipping.

The vpk version is deliberately not pinned, so vpk changing its vendor layout
(or shipping a runtime without `.upd_info`) makes the script fail with an
actionable message rather than ship an AppImage that cannot update itself. A
local `pack-appimage.sh` run leaves the runtime in that machine's vpk install
patched, so later AppImages built with the same vpk carry this repository's
update string until vpk is reinstalled or updated; CI is unaffected because every
job installs a fresh tool. Use `APPIMAGE_RUNTIME_PATH` to exercise the patch
against a copy instead of the installed runtime. The dependence on vpk's
internals is the accepted cost of not post-processing the `.nupkg`; a patched
`.nupkg` is the alternative to revisit if that layout ever becomes unstable.

## Licensing

The application is **AGPL-3.0-or-later**. The bundled components are:

| Component | License |
| --- | --- |
| FFmpeg (`--enable-gpl --enable-version3`) | **GPL-3.0-or-later** |
| mpv / libmpv (`-Dgpl=true`, mpv's default: its X11 code is GPL-only) | **GPL-2.0-or-later** (LGPL-2.1-or-later for the non-GPL parts) |
| dav1d | BSD-2-Clause |
| libass | ISC |
| zimg | WTFPL (zlib-style, permissive) |
| libplacebo | LGPL-2.1-or-later |
| wayland | MIT |
| wayland-protocols | MIT |
| libgcc_s, libpulse (bundled) | GPL-3.0-with-GCC-exception / LGPL-2.1-or-later |
| libstdc++ (host-provided) | GPL-3.0-with-GCC-exception |
| libasound, libdrm (host-provided) | LGPL-2.1-or-later / MIT |
| glibc (host) | LGPL-2.1 |

**The two GPL components are FFmpeg and mpv/libmpv, and both are fine.**
Media Sort is AGPL-3.0-or-later, and GPLv3 §13 explicitly permits combining
GPLv3 code with AGPLv3 code, so a GPLv3 FFmpeg and a GPL libmpv link into this
application without a licensing conflict — the application's own AGPL is the
stronger license of the combined work.

* **mpv** is GPL unavoidably in practice: its X11 support (and therefore VA-API
  on X11 sessions) is GPL-only, so `-Dgpl=false -Dx11=enabled` fails the meson
  configure. It is GPL-2.0-or-**later**, so the `or later` clause already allows
  treating it as GPLv3.
* **FFmpeg** is built `--enable-gpl --enable-version3`, i.e. GPLv3, which is the
  combination GPLv3 §13 blesses outright. Nothing here needs a GPL codec —
  FFmpeg got no GPL *encoder* (no x264/x265/libxvid; the app never encodes
  video) — so the GPL part is the codec stack itself, and the flag is a
  deliberate decision, not an accident: `--enable-version3` is what makes it
  v3 rather than v2-only. An LGPL-only build was the earlier policy and it was
  wrong: it encoded a constraint the application's own license does not impose.
  Do not "restore" it. `verify.sh` now enforces the *correct* rule: a build
  configured with `--enable-gpl` must also have `--enable-version3` (a
  GPLv2-only stack cannot combine with AGPLv3); an LGPL build still passes.

**What the GPL obligation amounts to in practice:** unmodified upstream
sources, already pinned by URL + sha256 in [`versions.env`](versions.env) (and
by the flags in `build-in-container.sh`, which is itself AGPL-licensed
repository source). No patches are applied to FFmpeg or mpv; no GPL code of ours
is produced. So there is no distribution obligation beyond the one AGPL already
imposes — making the corresponding source available — and it is satisfied by the
same pinned sources this build reads. Keep that property when adding
dependencies: anything GPL-only needs a licensing decision before it may be
bundled, and if it ever stops being true that a component is unmodified
upstream, that component's patches have to be published with it.
