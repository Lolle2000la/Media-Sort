#!/usr/bin/env bash
#
# Build the entire Linux AppImage payload inside an Ubuntu 22.04 container:
# dav1d -> FFmpeg (shared, LGPL) -> libass -> zimg -> type wayland-protocols ->
# libplacebo -> libmpv, and then the Rust GUI binary itself.
#
# Everything is compiled here on purpose:
#
#   * The payload's glibc floor is the container's glibc (2.35 on jammy). The
#     AppImage used to be packed on ubuntu-24.04 (glibc 2.39) and failed to run
#     on older distributions.
#   * libmpv/ffmpeg are built from pinned upstream sources and installed into
#     /usr/local, so the Rust link (`libmpv-sys` passes a bare `-lmpv`) and
#     every pkg-config probe (dav1d!) resolve against OUR build, never a host
#     package. The release runner deliberately has no libmpv/libdav1d dev
#     packages installed, so a host library cannot leak in unnoticed.
#
# Usage, from a container started at the repository root (see README.md):
#
#   bash packaging/appimage/build-in-container.sh
#
# Environment:
#   TARGET           Rust target triple           (default: x86_64-unknown-linux-gnu)
#   PREFIX           install prefix for the stack (default: /usr/local)
#   BUILD_ROOT       cache/log dir                (default: $REPO/.appimage-build)
#   STAGE_DIR        where the bundle input lands (default: $REPO/target/appimage-stage)
#   JOBS             parallel make/ninja jobs     (default: nproc)
#   FORCE            rebuild everything, ignoring stamps
#   FORCE_COMPONENTS space-separated subset to rebuild (e.g. "mpv libplacebo")
#   HOST_UID/HOST_GID chown of the host-visible outputs (they are written as
#                    root because apt and /usr/local need root)
#
# Requires network access (apt, github/ffmpeg.org tarballs, crates.io, rustup).
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)
# shellcheck source=versions.env
. "$SCRIPT_DIR/versions.env"

TARGET=${TARGET:-x86_64-unknown-linux-gnu}
BUILD_ROOT=${BUILD_ROOT:-$REPO_ROOT/.appimage-build}
# The install prefix lives in the workspace, NOT in /usr/local: every CI step
# runs `docker run --rm`, so /usr/local is a fresh filesystem each time and the
# per-component stamps in $BUILD_ROOT would claim "already built" while their
# artifacts were gone. Keeping the prefix next to the stamps makes skipping
# correct, and the pkg-config/LIBRARY_PATH exports below make every build see it
# exactly like a system prefix.
PREFIX=${PREFIX:-$BUILD_ROOT/prefix}
STAGE_DIR=${STAGE_DIR:-$REPO_ROOT/target/appimage-stage}
JOBS=${JOBS:-$(nproc)}
FORCE=${FORCE:-0}
FORCE_COMPONENTS=${FORCE_COMPONENTS:-}
HOST_UID=${HOST_UID:-}
HOST_GID=${HOST_GID:-}

DOWNLOADS=$BUILD_ROOT/downloads
SRC=$BUILD_ROOT/src
LOGS=$BUILD_ROOT/logs
STAMPS=$BUILD_ROOT/stamps
mkdir -p "$DOWNLOADS" "$SRC" "$LOGS" "$STAMPS"

case "$TARGET" in
  x86_64-unknown-linux-gnu)  ARCH=x86_64;  NASM=yes ;;
  aarch64-unknown-linux-gnu) ARCH=aarch64; NASM=no  ;;
  *) echo "unsupported TARGET '$TARGET'" >&2; exit 2 ;;
esac

# Every component (meson, autotools, ffmpeg's configure and cargo's
# pkg-config probes) must see the prefix first, and staging has to find its
# shared libraries when it runs `ldd`. The multiarch entry covers a component
# that still installs into lib/<triplet> despite --libdir=lib.
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig:$PREFIX/lib/$ARCH-linux-gnu/pkgconfig:$PREFIX/share/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LIBRARY_PATH="$PREFIX/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# ---------------------------------------------------------------------------
# logging / helpers
# ---------------------------------------------------------------------------
CURRENT_COMPONENT="setup"
CURRENT_LOG=$LOGS/setup.log

log() { printf '[appimage] %s\n' "$*" >&2; }

die() {
  printf '[appimage] ERROR: %s\n' "$*" >&2
  printf '[appimage] --- last 40 lines of %s ---\n' "$CURRENT_LOG" >&2
  tail -n 40 "$CURRENT_LOG" >&2 || true
  exit 1
}

# run_logged <component> <cmd...>
run_logged() {
  CURRENT_COMPONENT=$1
  shift
  CURRENT_LOG=$LOGS/$CURRENT_COMPONENT.log
  log "==> $CURRENT_COMPONENT: $*"
  if ! "$@" >>"$CURRENT_LOG" 2>&1; then
    die "$CURRENT_COMPONENT failed"
  fi
}

stamped() {
  local name=$1 version=$2
  case " $FORCE_COMPONENTS " in
    *" $name "*) return 1 ;;
  esac
  [ "$FORCE" != "1" ] || return 1
  [ -f "$STAMPS/$name-$version" ]
}

mark_stamped() {
  local name=$1 version=$2
  touch "$STAMPS/$name-$version"
}

# fetch <name> <url> <sha256> -> prints the tarball path
fetch() {
  local name=$1 url=$2 sha=$3
  local base
  base=$(basename "$url")
  # `fetch` is used as `$(fetch ...)`, so everything it prints but the path must
  # go to stderr (which `log` already does; the curl progress meter is quiet).
  local dest=$DOWNLOADS/$name-$base
  if [ -f "$dest" ] && [ "$(sha256sum "$dest" | cut -d' ' -f1)" = "$sha" ]; then
    printf '%s\n' "$dest"
    return 0
  fi
  log "downloading $name from $url"
  rm -f "$dest"
  if ! curl -fL --retry 3 --retry-delay 5 -o "$dest" "$url" >>"$LOGS/fetch.log" 2>&1; then
    die "download of $name failed ($url)"
  fi
  local got
  got=$(sha256sum "$dest" | cut -d' ' -f1)
  if [ "$got" != "$sha" ]; then
    rm -f "$dest"
    die "sha256 mismatch for $name: pinned $sha, got $got"
  fi
  printf '%s\n' "$dest"
}

# unpack <tarball> <component-dir-name>
unpack() {
  local tarball=$1 name=$2
  [ -n "$name" ] || die "unpack: empty component name"
  rm -rf "${SRC:?}/$name"
  mkdir -p "$SRC/$name"
  tar -xf "$tarball" -C "$SRC/$name" --strip-components=1
  printf '%s\n' "$SRC/$name"
}

# ---------------------------------------------------------------------------
# build dependencies
# ---------------------------------------------------------------------------
install_build_deps() {
  CURRENT_COMPONENT=apt
  CURRENT_LOG=$LOGS/apt.log
  local pkgs=(
    build-essential autoconf automake libtool pkg-config cmake git curl
    ca-certificates xz-utils python3 python3-pip python3-jinja2 ninja-build
    zlib1g-dev
    # libmpv: audio out + display back-ends (VA-API)
    libasound2-dev libpulse-dev
    libx11-dev libxext-dev libxrandr-dev libxpresent-dev libxss-dev
    # only ever dlopen'd by winit's x11-dl, but the AppImage has to ship them
    # (see the dlopened list in bundle.sh)
    libxcursor-dev libxi-dev libxinerama-dev
    libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev
    # building wayland itself (see build_wayland)
    libffi-dev libexpat1-dev
    libva-dev libdrm-dev
    # Vulkan *headers* only: libplacebo's public vulkan.h is included by its
    # Vulkan stub even with -Dvulkan=disabled, so the headers have to exist at
    # build time. Nothing links libvulkan.so.1 (mpv and libplacebo both have
    # Vulkan compiled out and wgpu dlopens the loader), so it never reaches the
    # payload -- the excludelist in bundle.sh refuses to ship it.
    libvulkan-dev
    # libass
    libfreetype-dev libfontconfig1-dev libfribidi-dev libharfbuzz-dev
  )
  if [ "$NASM" = yes ]; then pkgs+=(nasm); fi

  log "==> apt: installing ${#pkgs[@]} build packages"
  {
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends "${pkgs[@]}"
  } >>"$LOGS/apt.log" 2>&1 || die "apt-get install failed"

  # Ubuntu 22.04 ships meson 0.61.2; libplacebo requires >= 0.63 and mpv's
  # meson.options use constructs newer than 0.61 as well. A pip-installed
  # meson is deterministic enough here (the version is pinned) and lands in
  # /usr/local/bin, which precedes /usr/bin on PATH.
  local meson_version=none
  if command -v meson >/dev/null; then meson_version=$(meson --version); fi
  if [ "$meson_version" != "1.5.2" ]; then
    run_logged meson-pip python3 -m pip install --no-cache-dir "meson==1.5.2"
  fi
  log "    meson $(meson --version), ninja $(ninja --version), gcc $(gcc -dumpversion)"
}

# ---------------------------------------------------------------------------
# dav1d: needed by FFmpeg (libavcodec AV1) and, via pkg-config, by the Rust
# `dav1d-sys` crate (AVIF image decoding).
# ---------------------------------------------------------------------------
build_dav1d() {
  if stamped dav1d "$DAV1D_VERSION"; then log "==> dav1d $DAV1D_VERSION (cached)"; return; fi
  local tarball src build="$SRC/dav1d-build"
  tarball=$(fetch dav1d "$DAV1D_URL" "$DAV1D_SHA256")
  src=$(unpack "$tarball" dav1d)
  rm -rf "$build"
  run_logged dav1d meson setup "$build" "$src" \
    --buildtype=release --default-library=shared --prefix="$PREFIX" --libdir=lib \
    -Denable_tools=false -Denable_tests=false -Denable_docs=false
  run_logged dav1d ninja -C "$build" -j "$JOBS"
  run_logged dav1d meson install -C "$build"
  mark_stamped dav1d "$DAV1D_VERSION"
}

# ---------------------------------------------------------------------------
# FFmpeg, LGPL (no --enable-gpl => no x264/x265), shared.
#
# Only the `ffmpeg` CLI is shipped: the app spawns it for video thumbnails, so
# it needs the lavfi source, the PNG encoder (zlib) and the image2 muxer, and
# real decoding of the container formats it is handed. `--disable-autodetect`
# keeps the build from silently linking whatever -dev package happens to be
# installed (libva, libx264, ...): everything external is named explicitly.
# ---------------------------------------------------------------------------
build_ffmpeg() {
  if stamped ffmpeg "$FFMPEG_VERSION"; then log "==> ffmpeg $FFMPEG_VERSION (cached)"; return; fi
  local tarball src build="$SRC/ffmpeg-build"
  tarball=$(fetch ffmpeg "$FFMPEG_URL" "$FFMPEG_SHA256")
  src=$(unpack "$tarball" ffmpeg)
  rm -rf "$build"
  mkdir -p "$build"
  (
    cd "$build"
    run_logged ffmpeg "$src/configure" \
      --prefix="$PREFIX" --libdir="$PREFIX/lib" \
      --enable-shared --disable-static --enable-pic \
      --disable-doc --disable-debug \
      --disable-ffplay --disable-ffprobe \
      --disable-autodetect \
      --disable-avdevice \
      --enable-zlib \
      --enable-libdav1d
  )
  run_logged ffmpeg make -C "$build" -j "$JOBS"
  run_logged ffmpeg make -C "$build" install
  mark_stamped ffmpeg "$FFMPEG_VERSION"
}

# ---------------------------------------------------------------------------
# libass: unconditional mpv dependency (PGS/ASS subtitle rendering).
# ---------------------------------------------------------------------------
build_libass() {
  if stamped libass "$LIBASS_VERSION"; then log "==> libass $LIBASS_VERSION (cached)"; return; fi
  local tarball src
  tarball=$(fetch libass "$LIBASS_URL" "$LIBASS_SHA256")
  src=$(unpack "$tarball" libass)
  (
    cd "$src"
    run_logged libass ./configure \
      --prefix="$PREFIX" --disable-static --enable-shared \
      --enable-fontconfig --enable-harfbuzz
    run_logged libass make -j "$JOBS"
    run_logged libass make install
  )
  mark_stamped libass "$LIBASS_VERSION"
}

# ---------------------------------------------------------------------------
# zimg: mpv's software scaler. Without it the software render API falls back to
# swscale: at 4K that is ~66 ms instead of ~7 ms per frame (≈6.6 fps vs 30 fps
# with hardware decoding), which makes phone/camera footage unwatchable.
# ---------------------------------------------------------------------------
build_zimg() {
  if stamped zimg "$ZIMG_VERSION"; then log "==> zimg $ZIMG_VERSION (cached)"; return; fi
  local tarball src
  tarball=$(fetch zimg "$ZIMG_URL" "$ZIMG_SHA256")
  src=$(unpack "$tarball" zimg)
  (
    cd "$src"
    run_logged zimg ./autogen.sh
    run_logged zimg ./configure --prefix="$PREFIX" --disable-static --enable-shared
    run_logged zimg make -j "$JOBS"
    run_logged zimg make install
  )
  mark_stamped zimg "$ZIMG_VERSION"
}

# ---------------------------------------------------------------------------
# wayland: mpv 0.41 requires wayland-client >= 1.21 and jammy ships 1.20.0.
# Without it mpv drops its Wayland backend entirely, and with it VA-API
# hardware decoding on native Wayland sessions (its X11 backend would only
# cover XWayland), so the library is built here.
# ---------------------------------------------------------------------------
build_wayland() {
  if stamped wayland "$WAYLAND_VERSION"; then log "==> wayland $WAYLAND_VERSION (cached)"; return; fi
  local tarball src build="$SRC/wayland-build"
  tarball=$(fetch wayland "$WAYLAND_URL" "$WAYLAND_SHA256")
  src=$(unpack "$tarball" wayland)
  rm -rf "$build"
  run_logged wayland meson setup "$build" "$src" \
    --buildtype=release --default-library=shared --prefix="$PREFIX" --libdir=lib \
    -Ddocumentation=false -Dtests=false -Ddtd_validation=false
  run_logged wayland ninja -C "$build" -j "$JOBS"
  run_logged wayland meson install -C "$build"
  mark_stamped wayland "$WAYLAND_VERSION"
}

# ---------------------------------------------------------------------------
# wayland-protocols: mpv's Wayland backend requires >= 1.31 (jammy ships 1.25)
# and it is what unlocks VA-API hardware decoding on Wayland sessions.
# ---------------------------------------------------------------------------
build_wayland_protocols() {
  if stamped wayland-protocols "$WAYLAND_PROTOCOLS_VERSION"; then
    log "==> wayland-protocols $WAYLAND_PROTOCOLS_VERSION (cached)"; return
  fi
  local tarball src build="$SRC/wayland-protocols-build"
  tarball=$(fetch wayland-protocols "$WAYLAND_PROTOCOLS_URL" "$WAYLAND_PROTOCOLS_SHA256")
  src=$(unpack "$tarball" wayland-protocols)
  rm -rf "$build"
  # `-Dtests=false` is what lets this build with jammy's wayland-scanner 1.20:
  # from 1.40 on the scanner is only needed for the installed *tests*, which
  # want >= 1.25, and mpv only needs the protocol XML/pc files. Building the
  # protocols (instead of using jammy's 1.25) is required because mpv asks for
  # >= 1.31.
  run_logged wayland-protocols meson setup "$build" "$src" \
    --buildtype=release --prefix="$PREFIX" --libdir=lib \
    -Dtests=false
  run_logged wayland-protocols meson install -C "$build"
  mark_stamped wayland-protocols "$WAYLAND_PROTOCOLS_VERSION"
}

# ---------------------------------------------------------------------------
# libplacebo: mpv 0.41.0's meson.build asks for it unconditionally
# (`dependency('libplacebo', version: '>=6.338.2')`, no `required:` and no
# -Dlibplacebo switch), so "skip libplacebo" is not possible with this mpv.
# It is only needed to link, though: the app drives libmpv's *software* render
# API, so Vulkan/OpenGL/D3D11 support is switched off (which also removes the
# shaderc/glslang dependency and libplacebo's Vulkan requirement).
# ---------------------------------------------------------------------------
build_libplacebo() {
  if stamped libplacebo "$LIBPLACEBO_VERSION"; then
    log "==> libplacebo $LIBPLACEBO_VERSION (cached)"; return
  fi
  local tarball src build="$SRC/libplacebo-build"
  tarball=$(fetch libplacebo "$LIBPLACEBO_URL" "$LIBPLACEBO_SHA256")
  src=$(unpack "$tarball" libplacebo)
  rm -rf "$build"
  run_logged libplacebo meson setup "$build" "$src" \
    --buildtype=release --default-library=shared --prefix="$PREFIX" --libdir=lib \
    -Ddemos=false -Dtests=false \
    -Dvulkan=disabled -Dopengl=disabled -Dd3d11=disabled \
    -Dshaderc=disabled -Dglslang=disabled -Dlcms=disabled -Dxxhash=disabled
  run_logged libplacebo ninja -C "$build" -j "$JOBS"
  run_logged libplacebo meson install -C "$build"
  mark_stamped libplacebo "$LIBPLACEBO_VERSION"
}

# ---------------------------------------------------------------------------
# mpv: libmpv only (no `mpv` CLI), software render API, VA-API on both X11 and
# Wayland. Every optional feature the app never uses is switched off, because
# each one is another shared library in the AppImage.
#
# -Dgpl is left at its default (true) on purpose: mpv's X11 support is
# GPL-only, and `-Dgpl=false -Dx11=enabled` is a hard meson error. X11 (and its
# VA-API backend) matters, and a GPL-2.0-or-later libmpv links into this
# AGPL-3.0-or-later app without a licensing problem, so mpv stays a GPL build
# while FFmpeg is LGPL (see README "Licensing").
#
# Note: mpv 0.41 dropped `meson_options.txt` (renamed to `meson.options`) and
# with it several options older recipes pass (`-Dmanpage-build`,
# `-Dencoding`); unknown options are a hard meson error, so they are gone here.
# ---------------------------------------------------------------------------
build_mpv() {
  if stamped mpv "$MPV_VERSION"; then log "==> mpv $MPV_VERSION (cached)"; return; fi
  local tarball src build="$SRC/mpv-build"
  tarball=$(fetch mpv "$MPV_URL" "$MPV_SHA256")
  src=$(unpack "$tarball" mpv)
  rm -rf "$build"
  run_logged mpv meson setup "$build" "$src" \
    --buildtype=release --default-library=shared --prefix="$PREFIX" --libdir=lib \
    -Dlibmpv=true \
    -Dcplayer=false \
    -Dbuild-date=false \
    -Dtests=false \
    -Dfuzzers=false \
    -Dzimg=enabled \
    -Dx11=enabled \
    -Dwayland=enabled \
    -Dvaapi=enabled \
    -Dvaapi-x11=enabled \
    -Dvaapi-wayland=enabled \
    -Dvaapi-drm=disabled \
    -Ddmabuf-wayland=disabled \
    -Dgl=disabled \
    -Degl=disabled \
    -Dvulkan=disabled \
    -Dshaderc=disabled \
    -Dspirv-cross=disabled \
    -Dplain-gl=disabled \
    -Dgbm=disabled \
    -Ddrm=disabled \
    -Dvdpau=disabled \
    -Djpeg=disabled \
    -Dlcms2=disabled \
    -Dlibarchive=disabled \
    -Duchardet=disabled \
    -Dlibavdevice=disabled \
    -Dlibbluray=disabled \
    -Ddvdnav=disabled \
    -Dcdda=disabled \
    -Ddvbin=disabled \
    -Dcaca=disabled \
    -Dsixel=disabled \
    -Dsdl2-video=disabled \
    -Dsdl2-audio=disabled \
    -Dlua=disabled \
    -Djavascript=disabled \
    -Dvapoursynth=disabled \
    -Dx11-clipboard=disabled \
    -Dalsa=enabled \
    -Dpulse=enabled \
    -Dpipewire=disabled \
    -Djack=disabled \
    -Dsndio=disabled \
    -Doss-audio=disabled \
    -Drubberband=disabled \
    -Dxv=disabled
  run_logged mpv ninja -C "$build" -j "$JOBS"
  run_logged mpv meson install -C "$build"
  mark_stamped mpv "$MPV_VERSION"
}

# ---------------------------------------------------------------------------
# Rust toolchain + the GUI binary. Built here so the binary's glibc floor is
# the container's (2.35), and so it links the libmpv we just built.
# ---------------------------------------------------------------------------
build_rust() {
  export CARGO_HOME=$BUILD_ROOT/cargo
  export RUSTUP_HOME=$BUILD_ROOT/rustup
  export HOME=${HOME:-$BUILD_ROOT/home}
  mkdir -p "$HOME"
  export PATH="$CARGO_HOME/bin:$PATH"

  if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
    CURRENT_COMPONENT=rustup
    CURRENT_LOG=$LOGS/rustup.log
    log "==> installing Rust toolchain (stable, minimal profile)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$BUILD_ROOT/rustup-init.sh" \
      >>"$LOGS/rustup.log" 2>&1 || die "downloading rustup-init failed"
    if ! sh "$BUILD_ROOT/rustup-init.sh" -y --profile minimal --default-toolchain stable \
        --no-modify-path >>"$LOGS/rustup.log" 2>&1; then
      die "rustup install failed"
    fi
  fi

  local out="$REPO_ROOT/target/$TARGET/release/media-sort-gui"
  if [ "$FORCE" != "1" ] && [ -x "$out" ]; then
    log "==> media-sort-gui (cached: $out)"
    return
  fi

  # libmpv-sys links a bare `-lmpv` (no pkg-config), so the linker has to be
  # pointed at our prefix explicitly (LIBRARY_PATH/PKG_CONFIG_PATH are exported
  # at the top of the script). dav1d comes in through pkg-config.
  export RUSTFLAGS="-L native=$PREFIX/lib"
  export CARGO_TARGET_DIR="$REPO_ROOT/target"

  CURRENT_COMPONENT=cargo
  CURRENT_LOG=$LOGS/cargo.log
  log "==> cargo build --release --target $TARGET -p media-sort-gui --features velopack"
  (
    cd "$REPO_ROOT"
    cargo build --release --target "$TARGET" -p media-sort-gui --features velopack
  ) >>"$CURRENT_LOG" 2>&1 || die "cargo build failed"
  log "    $(ls -lh "$out" | awk '{print $5}') $out"
}

# ---------------------------------------------------------------------------
# Stage the runtime closure for bundle.sh (which runs on the host, where
# patchelf is available and the AppDir lives).
# ---------------------------------------------------------------------------
stage_payload() {
  local binary=$REPO_ROOT/target/$TARGET/release/media-sort-gui
  local ffmpeg=$PREFIX/bin/ffmpeg
  [ -x "$binary" ] || die "missing $binary"
  [ -x "$ffmpeg" ] || die "missing $ffmpeg"
  rm -rf "$STAGE_DIR"
  run_logged bundle bash "$SCRIPT_DIR/bundle.sh" stage "$STAGE_DIR" "$PREFIX" "$binary" "$ffmpeg"
}

# ---------------------------------------------------------------------------
# summary
# ---------------------------------------------------------------------------
summary() {
  local binary=$REPO_ROOT/target/$TARGET/release/media-sort-gui
  echo
  echo "================ AppImage build summary ================"
  printf '%-18s %s\n' "container glibc" "$(ldd --version | head -n 1 | awk '{print $NF}')"
  printf '%-18s %s\n' "gcc" "$(gcc -dumpversion)"
  printf '%-18s %s\n' "meson" "$(meson --version)"
  for pair in "dav1d:$DAV1D_VERSION" "ffmpeg:$FFMPEG_VERSION" "libass:$LIBASS_VERSION" \
              "zimg:$ZIMG_VERSION" "wayland:$WAYLAND_VERSION" \
              "wayland-protocols:$WAYLAND_PROTOCOLS_VERSION" \
              "libplacebo:$LIBPLACEBO_VERSION" "mpv:$MPV_VERSION"; do
    printf '%-18s %s\n' "${pair%%:*}" "${pair#*:}"
  done
  printf '%-18s %s\n' "binary" "$(ls -lh "$binary" | awk '{print $5}')"
  printf '%-18s %s\n' "stage dir" "$STAGE_DIR ($(du -sh "$STAGE_DIR" | cut -f1))"
  printf '%-18s %s files\n' "staged libs" "$(find "$STAGE_DIR/lib" -name '*.so*' | wc -l)"
  printf '%-18s %s\n' "logs" "$LOGS"
  echo "Next (on the host): packaging/appimage/bundle.sh install <AppDir> $STAGE_DIR"
  echo "========================================================"
}

chown_outputs() {
  if [ -n "$HOST_UID" ] && [ "$(id -u)" = "0" ]; then
    log "==> chown -R $HOST_UID:$HOST_GID on host-visible outputs"
    chown -R "$HOST_UID:$HOST_GID" \
      "$STAGE_DIR" "$BUILD_ROOT" "$REPO_ROOT/target" 2>/dev/null || true
  fi
}

main() {
  log "repo:     $REPO_ROOT"
  log "target:   $TARGET ($ARCH)"
  log "prefix:   $PREFIX"
  log "build:    $BUILD_ROOT"
  log "stage:    $STAGE_DIR"

  install_build_deps
  build_dav1d
  build_ffmpeg
  build_libass
  build_zimg
  build_wayland
  build_wayland_protocols
  build_libplacebo
  build_mpv
  build_rust
  stage_payload
  summary
  chown_outputs
}

main "$@"
