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
  `libmpv`, FFmpeg, dav1d, libass, zimg, wayland-protocols and libplacebo – so
  the whole payload's glibc floor is **2.35**;
* **libmpv, the `ffmpeg` CLI and their dependency closure are bundled into the
  AppDir** with `$ORIGIN`-relative RPATHs, so nothing is resolved from the host
  except glibc and the GPU driver stack.

Every source is a sha256-pinned upstream tarball in [`versions.env`](versions.env);
no binaries are committed to git and nothing is downloaded at runtime.

## Files

| File | Purpose |
| --- | --- |
| `versions.env` | Every upstream pin in one place (mpv, FFmpeg, dav1d, libass, zimg, libplacebo, wayland-protocols) with URL + sha256 + release commit. **Bump versions here and nowhere else.** |
| `build-in-container.sh` | Runs in `ubuntu:22.04`: apt deps → dav1d → FFmpeg → libass → zimg → wayland-protocols → libplacebo → libmpv → `cargo build` → stage. Per-component logs, per-component stamps (re-runs are cheap). |
| `bundle.sh` | `stage` (container side) computes the closure into `target/appimage-stage`; `install` (host side) copies it into the AppDir, writes the RPATHs and fails on any unresolved `DT_NEEDED`. |
| `excludelist` | What must never be bundled: glibc/loader and the GL/Vulkan/GPU-driver stack. |
| `verify.sh` | The gate: glibc floor, NEEDED resolution, `libmpv.so.2` presence, real `ffmpeg` encode/decode, GUI start under Xvfb. |

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
`.appimage-build/` (git-ignored), so re-running is a no-op unless a pin changed.
The prefix deliberately sits in the workspace instead of `/usr/local`: every CI
step is a `docker run --rm`, so `/usr/local` is empty again on the next run
while the stamps would still say "already built". `PKG_CONFIG_PATH`,
`LIBRARY_PATH` and `LD_LIBRARY_PATH` point at it, and `dav1d-sys` finds it
through the same `pkg-config` path.
Force a rebuild of one component with
`docker run … -e FORCE_COMPONENTS="mpv libplacebo"`, or everything with
`-e FORCE=1`.

Then bundle and verify against a real AppDir:

```bash
# inside an extracted AppImage AppDir (see release.yml) or a hand-made one
bash packaging/appimage/bundle.sh install squashfs-root target/appimage-stage
bash packaging/appimage/verify.sh squashfs-root
```

`verify.sh` also accepts a packed `.AppImage` (it extracts it with
`--appimage-extract`, so FUSE is not needed) and needs `xvfb` on the host for
the GUI check; `--skip-gui` disables that one check.

## Why a container, and why two phases

The payload's glibc floor is the glibc of whatever compiled it. Building on
`ubuntu-24.04` (2.39) is what broke the AppImage catalog test, so the build
runs in `ubuntu:22.04` (2.35). The same container also means the Rust link
cannot accidentally pick up a host `libmpv`: the release runner installs only
`squashfs-tools`, `patchelf` and `xvfb`, no `-dev` packages at all.

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
* `--enable-libdav1d`: AV1 has no usable native decoder. libvpx is *not*
  enabled – native VP8/VP9 decoders cover WebM – and GPL encoders are out.

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
* `--default-library=shared`, `--libdir=<prefix>/lib` (a plain prefix, not
  Flatpak's `--libdir=lib` long form – there is no multiarch SDK here).

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

1. Change the version **and** the URL/sha256 (and the commit, for a git tag
   pin). `curl -fL <url> | sha256sum` gives the value.
2. Run `.github/workflows/appimage.yml` (or the docker command above).
3. If a component's build fails, move to the previous release of that component
   rather than patching its sources. Keep the combination that the release
   actually ships; FFmpeg's major version is the most likely to break mpv.

Choices as of the initial version: **mpv v0.41.0** (the newest stable release),
**FFmpeg 9.0.2** (newest stable release, and mpv 0.41 builds against it),
**dav1d 1.5.4** (newest; also satisfies `dav1d-sys`'s `>= 1.3` pkg-config probe
so the Rust AVIF decoder links the bundled library), **libass 0.17.5**,
**zimg release-3.0.6**, **libplacebo v7.360.1**, **wayland-protocols 1.49**.

## What is bundled, and what is not

Bundled (`usr/lib`, by SONAME): `libmpv.so.2`, `libavcodec/format/util/filter/
device/…`, `libswscale`, `libswresample`, `libavfilter`, `libdav1d`, `libass`,
`libzimg`, `libplacebo`, `libfreetype`, `libharfbuzz`, `libfribidi`,
`libfontconfig`, `libexpat`, `libz`, `libgcc_s`, `libasound`, `libpulse`,
`libX11`-family, `libxcb`, `libwayland-*`, `libva*` (the API library, not the
driver), … plus `usr/bin/ffmpeg`.

Not bundled, by design (`excludelist`): glibc and the loader (`libc`,
`ld-linux-*`, `libm`, `libdl`, `libpthread`, `librt`, `libresolv`, `libnss_*`,
…), and the graphics/driver stack (`libGL*`, `libEGL*`, `libOpenGL*`,
`libGLX*`, `libdrm*`, `libgbm*`, `libglapi*`, `libvulkan*`, vendor drivers,
`*_drv_video.so`). A GPU driver has to match the running kernel and hardware;
`libmpv` dlopens the VA-API driver at runtime, so only the API library ships.

`libgcc_s.so.1` *is* bundled (the upstream AppImage excludelist would skip it);
newer distros ship a newer one and older ones can be too old for
`libavcodec`'s `_Unwind_*` usage. Everything else in the upstream excludelist
that is a hard `DT_NEEDED` of libmpv (`libasound`, `libX11`, `libwayland-client`,
`libfontconfig`, `libfreetype`, `libharfbuzz`, `libfribidi`, `libz`) is bundled
here too, because leaving them out is exactly the bug this work fixes.

## Verification gate

`verify.sh` fails the build on any of:

* a `GLIBC_*` requirement above **2.35** in any packaged ELF,
* a `DT_NEEDED` that resolves neither inside `usr/lib` nor through the
  excludelist (plus an `ldd` pass with an empty environment that catches
  "present but unusable"),
* a missing `usr/lib/libmpv.so.2`,
* `usr/bin/ffmpeg -version` failing, a GPL build, a failed
  `-f lavfi -i testsrc -frames:v 1 out.png`, a failed decode round-trip,
* the GUI dying with `error while loading shared libraries` under `xvfb-run`
  (for an AppImage it is launched with `--appimage-extract-and-run`; a
  non-timeout exit is only *reported*, since a GPU-less Xvfb is not a loader
  problem).

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
| wayland-protocols | MIT |
| libgcc_s, glibc (host) | GPL-3.0-with-GCC-exception / LGPL-2.1 |

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
