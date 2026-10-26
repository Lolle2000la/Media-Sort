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
| `build-in-container.sh` | Runs in `ubuntu:22.04`: apt deps → dav1d → FFmpeg → libass → zimg → wayland → wayland-protocols → libplacebo → libmpv → `cargo build` → stage. Per-component logs, per-component stamps over version *and* build flags (re-runs are cheap, a flag change rebuilds). |
| `bundle.sh` | `stage` (container side) computes the closure into `target/appimage-stage`; `install` (host side) copies it into the AppDir, writes the RPATHs and fails on any unresolved `DT_NEEDED`. |
| `excludelist` | The host-provided allowlist: what must never be bundled (glibc/loader, the GL/Vulkan/GPU-driver stack, and the plugin hosts the host's plugins bind to) with a one-line reason per entry. |
| `pack-appimage.sh` | The two-pass `vpk pack` flow (staging → pack → unpack → bundle → verify → desktop surgery → final pack), shared by `release.yml` and `appimage.yml`. |
| `verify.sh` | The gate: glibc floor (incl. `GLIBC_ABI_*`), NEEDED resolution, the `strings` manifest of the shipped binary, `libmpv.so.2` presence, real `ffmpeg` encode/decode, GUI start under Xvfb. |

CI wiring: `.github/workflows/release.yml` (Linux matrix entries) and
`.github/workflows/appimage.yml` (packaging-only check, x86_64).

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
A component stamp is keyed on the version **and** a hash of its build arguments,
so editing a meson/configure flag in `build-in-container.sh` rebuilds exactly
that component. The prefix deliberately sits in the workspace instead of
`/usr/local`: every CI step is a `docker run --rm`, so `/usr/local` is empty
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
the GUI check; `--skip-gui` disables that one check.

To produce the sealed AppImage locally (vpk, `mksquashfs` and `patchelf` on
PATH, `xvfb` for the gate inside it):

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
`squashfs-tools`, `patchelf`, `xvfb` and the host-provided libraries the gate
needs to start the app (`libasound2`, `libdrm2`) — no `-dev` packages at all.

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
--disable-debug --disable-ffplay --disable-ffprobe --disable-autodetect
--disable-avdevice --enable-zlib --enable-libdav1d`):

* **LGPL only** – `--enable-gpl` is never passed, so no x264/x265. The app never
  encodes video (and mpv, which is GPL for its X11 support, still cannot gain
  GPL codecs this way); `verify.sh` fails the build if a future flag change
  drags GPL code into FFmpeg.
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
  X11 sessions) is worth more than an LGPL mpv, and GPL-2.0-or-later libmpv
  links into an AGPL-3.0-or-later application without a licensing conflict —
  see "Licensing" below.
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

## Version bumps

Everything lives in [`versions.env`](versions.env):

1. Change the version **and** the URL/sha256 (and the commit, where upstream
   publishes a tarball from a tag whose commit we can record). `curl -fL <url> |
   sha256sum` gives the value.
2. Run `.github/workflows/appimage.yml` (or the docker command above).
3. If a component's build fails, move to the previous release of that component
   rather than patching its sources. Keep the combination that the release
   actually ships; FFmpeg's major version is the most likely to break mpv.

`RUST_VERSION` follows the same rule: bump it deliberately (and only after
checking the workspace still builds with that compiler), because the AppImage's
ABI surface is whatever that release produces.

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
`libexpat`, `libz`, `libgcc_s`, `libpulse`, `libX11`-family (including the
`libXcursor`/`libXi`/`libXinerama` libraries winit only *dlopens*), `libxcb`,
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

`libpulse.so.0` stays bundled: it is a *client* library (it talks to the socket,
nothing binds to our copy), and it is a hard `DT_NEEDED` of libmpv.

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
first two but not `libasound2`/`libdrm2`, and `verify.sh` then reports exactly
those as missing host-provided libraries.

Two entries that upstream's excludelist has, but this payload deliberately
ships:

* **`libgcc_s.so.1`** — the upstream AppImage excludelist would skip it, but
  newer distros ship a newer one and older ones can be too old for
  `libavcodec`'s `_Unwind_*` usage.
* **`libpulse.so.0`** — a client library, not a plugin host: nothing on the host
  binds to our copy, and it is a hard `DT_NEEDED` of libmpv.

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
  is reported as the documented floor, not failed),
* a library name in the shipped binary's `strings` manifest that is neither
  bundled nor on the host-provided allowlist with a reason,
* a missing `usr/lib/libmpv.so.2`,
* `usr/bin/ffmpeg -version` failing, a GPL build, a missing
  `h264`/`hevc`/`vp9`/`libdav1d` decoder, a failed PNG encode (the rawvideo pipe
  is used because `--disable-avdevice` removes the lavfi *input*), a failed
  mpeg4 decode round-trip, or a failed `libdav1d` decode of the repository's real
  AV1 fixture,
* the GUI dying with `error while loading shared libraries` (or a versioned
  symbol failure) under `xvfb-run`, exiting 0, failing to execute (126/127),
  dying from SIGABRT/SIGFPE/SIGSEGV (134/136/139), or exiting before ~3 s without
  a GPU/graphics complaint. For an AppImage it is launched with
  `--appimage-extract-and-run`; a later non-zero exit *with* a GPU complaint is
  only reported, since a GPU-less Xvfb cannot open a wgpu surface. When
  `xdotool` is present the check also reports whether a window named
  `Media Sort` appeared (informational only).

## Licensing

The application is **AGPL-3.0-or-later**. The bundled components are:

| Component | License |
| --- | --- |
| FFmpeg (built without `--enable-gpl`) | **LGPL-2.1-or-later** |
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

The one GPL component is **mpv/libmpv**, and it is unavoidable: mpv's X11
support (and therefore VA-API on X11 sessions) is GPL-only, so `-Dgpl=false`
together with `-Dx11=enabled` fails the meson configure. That is compatible
with shipping the app: mpv is GPL-2.0-or-**later**, the `or later` clause
allows treating it as GPL-3.0, and AGPL-3.0 explicitly permits combining with
GPL-3.0 code — the application's own license, AGPL-3.0-or-later, is the
stronger one for the combined work.

FFmpeg deliberately stays **LGPL** (`--enable-gpl` is never passed, so no
x264/x265): the app never encodes video, and keeping the *codec stack* LGPL
means a future change of heart about the app's license does not require
rebuilding it. The AppImage is a mere aggregation of these libraries with the
AGPL application, and their licenses only ask for attribution and the
availability of their sources — which `versions.env` pins exactly. Keep that
property when adding dependencies: anything GPL-only needs a licensing decision
before it may be bundled.
