#!/usr/bin/env bash
#
# The host-provided library policy, shared by bundle.sh (which refuses to stage
# these names) and verify.sh (which prints the resulting decision table), so the
# two cannot disagree about the same name.
#
# Sourced, never executed: it only defines APPIMAGE_DLOPEN_ONLY, EXCLUDELIST and
# the lookup functions. The reasoning behind every entry lives in ./excludelist.

APPIMAGE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
EXCLUDELIST=${EXCLUDELIST:-$APPIMAGE_DIR/excludelist}

# Libraries the payload has to carry that nothing links against: they are
# dlopen'd at runtime, so a DT_NEEDED walk never reaches them. winit's x11-dl
# loads the X11 ones lazily on X11 sessions (and xkbcommon-x11 for keymaps), and
# without them the AppImage dies on startup with "libXcursor.so.1: cannot open
# shared object file" on a system that does not happen to have them installed.
# bundle.sh stages them and verify.sh requires each of them to be bundled.
# A library that *is* also a DT_NEEDED of the payload (libXrandr.so.2 is one of
# libmpv) does not belong here: the closure walk already covers it, and listing
# it here would only hide that fact.
# shellcheck disable=SC2034  # used by the two sourcing scripts
APPIMAGE_DLOPEN_ONLY=(libXcursor.so.1 libXi.so.6 libX11-xcb.so.1 libxkbcommon-x11.so.0)

# The driver/vendor families that must never be shipped, even though a NEEDED
# walk would not reach most of them (they are dlopen'd): they have to match the
# host kernel driver and GPU.
matches_driver_glob() {
  case "$1" in
    libGL*.so*|libEGL*.so*|libOpenGL*.so*|\
    libgbm*.so*|libglapi*.so*|libvulkan*.so*|\
    libnvidia*.so*|libcuda*.so*|libmali*.so*|libd3d*.so*|libwsl*.so*|\
    *_drv_video.so|*_driver_video.so)
      return 0 ;;
  esac
  return 1
}

# Prints the one-line reason of an exact ./excludelist entry (nothing when the
# entry carries none). Returns 0 when the name is listed at all, 1 otherwise.
excludelist_entry_reason() {
  awk -v n="$1" '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    { candidate = $1; sub(/:$/, "", candidate)
      if (candidate != n) next
      found = 1
      rest = $0
      sub(/^[[:space:]]*[^ ]*:?[[:space:]]*/, "", rest)
      if (rest != "" && !printed) { print rest; printed = 1 }
    }
    END { exit(found ? 0 : 1) }
  ' "$EXCLUDELIST"
}

# True when a name is host-provided: either an exact ./excludelist entry or one
# of the driver/vendor glob families.
is_excluded() {
  local name=$1
  excludelist_entry_reason "$name" >/dev/null && return 0
  matches_driver_glob "$name"
}

# One-line reason why a name is host-provided: the second field of its
# ./excludelist entry, or the family reason for the glob patterns above.
host_reason() {
  local name=$1 reason
  # `|| true`: a name that is only covered by a driver glob has no exact entry,
  # and excludelist_entry_reason() signals that with status 1. Under `set -e`
  # that would abort this function instead of falling through to the globs.
  reason=$(excludelist_entry_reason "$name") || true
  if [ -n "$reason" ]; then
    printf '%s' "$reason"
    return 0
  fi
  case "$name" in
    libOpenGL*.so*)
      printf 'GPU: OpenGL entry point, resolves to the host kernel driver' ;;
    libGL*.so*)
      printf 'GPU: GL/GLX/GLdispatch entry point, resolves to the host kernel driver' ;;
    libEGL*.so*|libglapi*.so*|libgbm*.so*)
      printf 'GPU: EGL/GBM/Mesa entry point, must match the host driver' ;;
    libvulkan*.so*|*_drv_video.so|*_driver_video.so)
      printf 'GPU: driver/ICD, must match the host kernel driver and GPU' ;;
    ld-linux*.so.*|ld64.so.2)
      printf 'glibc loader: must be the running system'"'"'s' ;;
    libnvidia*.so*|libcuda*.so*|libmali*.so*|libd3d*.so*|libwsl*.so*)
      printf 'GPU: vendor driver/userspace, must match the host kernel driver' ;;
    *)
      printf 'host-provided (see ./excludelist)' ;;
  esac
}
