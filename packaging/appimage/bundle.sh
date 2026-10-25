#!/usr/bin/env bash
#
# Assemble the AppImage payload: copy the runtime dependency closure of libmpv
# and ffmpeg into the AppDir and point everything at it with an $ORIGIN-relative
# RPATH, so nothing is resolved from the host except glibc itself and the GPU
# driver stack (see ./excludelist).
#
# Two modes, because the compilation happens inside an Ubuntu 22.04 container
# while patchelf and the AppDir live on the host:
#
#   bundle.sh stage   <stage-dir> <prefix> <binary>...
#       Resolve the dependency closure of <prefix>/lib/libmpv.so.2, of every
#       <binary>, and of <prefix>/bin/ffmpeg, then copy it into
#       <stage-dir>/lib (named by SONAME) and <stage-dir>/bin. This runs inside
#       the container (see build-in-container.sh), where ldd sees the real
#       prefix. The GUI binary is a closure root only -- it is placed in the
#       AppDir by vpk, not copied here.
#
#   bundle.sh install <appdir> <stage-dir>
#       Copy <stage-dir>/lib/*.so* into <appdir>/usr/lib and <stage-dir>/bin/*
#       into <appdir>/usr/bin, set RPATH=$ORIGIN on usr/lib and
#       RPATH=$ORIGIN/../lib on usr/bin, then fail if any DT_NEEDED of a packaged
#       ELF would still be loaded from outside the AppDir (except excludelist).
#
# Usage (full release path):
#   bash packaging/appimage/bundle.sh stage   target/appimage-stage \
#        .appimage-build/prefix \
#        target/x86_64-unknown-linux-gnu/release/media-sort-gui
#   bash packaging/appimage/bundle.sh install squashfs-root target/appimage-stage
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
EXCLUDELIST=$SCRIPT_DIR/excludelist

die() { printf 'bundle: ERROR: %s\n' "$*" >&2; exit 1; }
log() { printf 'bundle: %s\n' "$*"; }

is_elf() {
  [ -f "$1" ] && [ "$(head -c 4 "$1" | od -An -c | tr -d ' \n')" = '177ELF' ]
}

# Excluded from the payload: see ./excludelist for the reasoning. The glob
# patterns below are the driver/vendor libraries that must never be shipped,
# even though a NEEDED walk would never reach most of them (they are dlopen'd).
is_excluded() {
  local name=$1
  awk -v n="$name" '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    $1 == n { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "$EXCLUDELIST" && return 0
  case "$name" in
    libGL*.so*|libEGL*.so*|libOpenGL*.so*|\
    libgbm*.so*|libglapi*.so*|libvulkan*.so*|\
    libnvidia*.so*|libcuda*.so*|libmali*.so*|libd3d*.so*|libwsl*.so*|\
    *_drv_video.so|*_driver_video.so)
      return 0 ;;
  esac
  return 1
}

# DT_NEEDED entries of an ELF, one per line.
needed_of() {
  readelf -d "$1" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'
}

# The SONAME of an ELF, or the file name as a fallback.
soname_of() {
  local so
  so=$(readelf -d "$1" 2>/dev/null | sed -n 's/.*(SONAME).*\[\(.*\)\]/\1/p' | head -n 1)
  [ -n "$so" ] || so=$(basename "$1")
  printf '%s\n' "$so"
}

# Resolved dependency paths of an ELF as the loader sees them right now
# (i.e. including the prefix we just installed into).
resolved_deps() {
  ldd "$1" 2>/dev/null | awk '
    /=>/ {
      # "libfoo.so.1 => /path/to/libfoo.so.1 (0x...)"
      name = $1; path = $3;
      if (path ~ /^\//) print name "\t" path;
    }
  '
}

# ---------------------------------------------------------------------------
# stage
# ---------------------------------------------------------------------------
stage() {
  local stage_dir=$1 prefix=$2
  shift 2
  [ $# -gt 0 ] || die "stage: no binaries given"
  local lib_dir=$stage_dir/lib bin_dir=$stage_dir/bin
  rm -rf "$stage_dir"
  mkdir -p "$lib_dir" "$bin_dir"

  # meson on Debian defaults --libdir to lib/<triplet>; build-in-container.sh
  # passes --libdir=lib, but probe both so a component that ignores it cannot
  # produce a mysteriously empty stage.
  local libmpv candidates=("$prefix"/lib/libmpv.so.2 "$prefix"/lib/*/libmpv.so.2
                           "$prefix"/lib/libmpv.so "$prefix"/lib/*/libmpv.so)
  for candidate in "${candidates[@]}"; do
    [ -e "$candidate" ] && { libmpv=$candidate; break; }
  done
  [ -n "${libmpv:-}" ] || die "stage: no libmpv under $prefix/lib (did the mpv build fail?)"

  # Libraries the GUI dlopens at runtime instead of linking against, so a
  # DT_NEEDED walk never sees them: winit's x11-dl loads Xcursor/Xi/Xinerama/
  # Xrandr/xkbcommon-x11 lazily on X11. Without them the AppImage dies on
  # startup with "libXcursor.so.1: cannot open shared object file" on a system
  # that does not happen to have them installed (verify.sh's GUI check found
  # exactly that), which is the same class of bug as the missing libmpv.
  local dlopened=(libXcursor.so.1 libXi.so.6 libXinerama.so.1 libXrandr.so.2
                  libX11-xcb.so.1 libxkbcommon-x11.so.0)
  local queue=("$libmpv" "$prefix/bin/ffmpeg" "$@") extra missing=()
  for extra in "${dlopened[@]}"; do
    local resolved
    resolved=$(ldconfig -p 2>/dev/null | awk -v n="$extra" '$1 == n { print $NF; exit }')
    if [ -n "$resolved" ] && [ -e "$resolved" ]; then
      queue+=("$resolved")
    else
      missing+=("$extra")
    fi
  done
  if [ ${#missing[@]} -gt 0 ]; then
    log "WARNING: dlopen-only libraries not found on this system: ${missing[*]}"
  fi

  local idx=0
  # Breadth-first over the loader's view of each root; new libraries are
  # appended to the queue so their own dependencies are walked too.
  while [ $idx -lt ${#queue[@]} ]; do
    local elf=${queue[$idx]}
    idx=$((idx + 1))
    is_elf "$elf" || die "stage: not an ELF file: $elf"
    # A root that is itself a shared library (libmpv, and the dlopen-only
    # libraries above) has to be staged as well, not only its dependencies --
    # executables are skipped here and copied into bin/ further down.
    local own
    own=$(readelf -d "$elf" 2>/dev/null | sed -n 's/.*(SONAME).*\[\(.*\)\]/\1/p' | head -n 1)
    if [ -n "$own" ] && [ ! -e "$lib_dir/$own" ] && ! is_excluded "$own"; then
      cp -L --preserve=mode,timestamps "$elf" "$lib_dir/$own"
      log "staged lib $own (from $elf)"
    fi
    while IFS=$'\t' read -r want path; do
      local name
      name=$(soname_of "$path")
      if is_excluded "$want" || is_excluded "$name"; then
        continue
      fi
      if [ -e "$lib_dir/$name" ]; then
        continue
      fi
      cp -L --preserve=mode,timestamps "$path" "$lib_dir/$name"
      log "staged lib $name (from $path)"
      queue+=("$lib_dir/$name")
    done < <(resolved_deps "$elf")
  done

  # The GUI binary is provided by vpk; everything else executable is ours.
  local exe
  for exe in "$prefix/bin/ffmpeg" "$@"; do
    [ "$(basename "$exe")" = media-sort-gui ] && continue
    cp -L --preserve=mode,timestamps "$exe" "$bin_dir/$(basename "$exe")"
    log "staged bin $(basename "$exe")"
  done

  {
    echo "# generated by packaging/appimage/bundle.sh stage"
    echo "# $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    for so in "$lib_dir"/*.so*; do
      printf '%s\t%s\n' "$(basename "$so")" "$(sha256sum "$so" | cut -d' ' -f1)"
    done
  } >"$stage_dir/manifest.txt"

  log "staged $(find "$lib_dir" -name '*.so*' | wc -l) libraries ($(du -sh "$lib_dir" | cut -f1)) into $stage_dir"
}

# ---------------------------------------------------------------------------
# install
# ---------------------------------------------------------------------------
install_payload() {
  local appdir=$1 stage_dir=$2
  [ -d "$appdir" ] || die "install: no such AppDir: $appdir"
  [ -d "$stage_dir/lib" ] || die "install: no staged libraries in $stage_dir/lib (run the 'stage' mode first)"

  local lib_dir=$appdir/usr/lib bin_dir=$appdir/usr/bin
  mkdir -p "$lib_dir" "$bin_dir"
  cp -L --preserve=mode,timestamps "$stage_dir"/lib/*.so* "$lib_dir/"
  if compgen -G "$stage_dir/bin/*" >/dev/null; then
    cp -L --preserve=mode,timestamps "$stage_dir"/bin/* "$bin_dir/"
  fi
  chmod 0755 "$bin_dir"/* 2>/dev/null || true
  log "copied $(find "$lib_dir" -name '*.so*' | wc -l) libraries into $lib_dir"

  local elf
  while IFS= read -r elf; do
    patchelf --force-rpath --set-rpath '$ORIGIN' "$elf"
  done < <(find "$lib_dir" -maxdepth 1 -name '*.so*' -type f)

  while IFS= read -r elf; do
    is_elf "$elf" || continue
    patchelf --force-rpath --set-rpath '$ORIGIN/../lib' "$elf"
  done < <(find "$bin_dir" -maxdepth 1 -type f)
  log "set \$ORIGIN RPATHs"

  verify_resolution "$appdir"
}

# Every DT_NEEDED of a packaged ELF must resolve inside the AppDir, except for
# the glibc family and the GPU driver stack in ./excludelist (those come from
# the host by design).
verify_resolution() {
  local appdir=$1
  local failures=() checked=0 elf want
  while IFS= read -r elf; do
    while IFS= read -r want; do
      [ -n "$want" ] || continue
      checked=$((checked + 1))
      if [ -e "$appdir/usr/lib/$want" ]; then
        continue
      fi
      if is_excluded "$want"; then
        continue
      fi
      failures+=("$(basename "$elf") needs $want")
    done < <(needed_of "$elf")
  done < <(find "$appdir/usr/lib" "$appdir/usr/bin" -maxdepth 1 -type f)

  if [ ${#failures[@]} -gt 0 ]; then
    printf 'bundle: ERROR: unresolved dependencies after bundling:\n' >&2
    printf '  %s\n' "${failures[@]}" >&2
    exit 1
  fi
  log "dependency check passed ($checked NEEDED entries, all resolvable from the AppDir or the excludelist)"
}

usage() {
  sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit 2
}

main() {
  local mode=${1:-}
  shift || true
  case "$mode" in
    stage) [ $# -ge 3 ] || usage; stage "$@" ;;
    install) [ $# -eq 2 ] || usage; install_payload "$@" ;;
    -h|--help|help) usage ;;
    *) usage ;;
  esac
}

main "$@"
