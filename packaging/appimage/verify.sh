#!/usr/bin/env bash
#
# Gate the AppImage payload. Fails the build unless all of the following hold:
#
#   a) no ELF in the payload requires a GLIBC_* version above the floor
#      (default 2.35, the jammy glibc the payload is built against) and none
#      carries a GLIBC_ABI_* reference (every one of those implies >= 2.36);
#   b) every DT_NEEDED of the packaged ELFs resolves from inside the AppDir,
#      except the host-provided allowlist in ./excludelist -- and every
#      host-provided library the payload still needs is reported as the floor;
#   c) every library name the shipped GUI binary mentions (its `strings`
#      manifest, i.e. linked *and* dlopen'd dependencies) is decided: bundled
#      in usr/lib, or on the host-provided allowlist with a reason;
#   d) usr/lib/libmpv.so.2 is present;
#   e) the bundled ffmpeg runs, encodes a PNG (video thumbnails), decodes it
#      back, and decodes a real AV1 file with the bundled libdav1d;
#   f) the GUI starts under a virtual X server without a single
#      "error while loading shared libraries", without a startup crash, and
#      without dying silently before it has had time to initialise (it may die
#      later for lack of a GPU, which is not this check's business).
#
# Usage: verify.sh <path-to-AppDir|path-to.AppImage> [--max-glibc X.Y] [--skip-gui]
#
# An .AppImage argument is unpacked with `--appimage-extract` first (no FUSE
# needed); the GUI check then runs the AppImage itself through
# `--appimage-extract-and-run`, which also proves the FUSE-less path works.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
EXCLUDELIST=$SCRIPT_DIR/excludelist

MAX_GLIBC=2.35
SKIP_GUI=0
TARGET=""

die() { printf 'verify: FAIL: %s\n' "$*" >&2; exit 1; }
log() { printf 'verify: %s\n' "$*"; }
ok()  { printf 'verify: ok: %s\n' "$*"; }

is_elf() {
  [ -f "$1" ] && [ "$(head -c 4 "$1" | od -An -c | tr -d ' \n')" = '177ELF' ]
}

is_excluded() {
  local name=$1
  awk -v n="$name" '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    { candidate = $1; sub(/:$/, "", candidate); if (candidate == n) found = 1 }
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

# One-line reason why a name is host-provided: the second field of its
# ./excludelist entry, or the family reason for the glob patterns above.
host_reason() {
  local name=$1 reason
  reason=$(awk -v n="$name" '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    { candidate = $1; sub(/:$/, "", candidate)
      if (candidate != n) next
      rest = $0
      sub(/^[[:space:]]*[^ ]*:?[[:space:]]*/, "", rest)
      if (rest != "") { print rest; exit }
    }
  ' "$EXCLUDELIST")
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

# True when <a> is a version >= <b> (both X.Y[.Z]).
version_ge() {
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1)" = "$1" ]
}

needed_of() {
  readelf -d "$1" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --max-glibc) MAX_GLIBC=$2; shift 2 ;;
    --skip-gui) SKIP_GUI=1; shift ;;
    -h|--help) sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) die "unknown option $1" ;;
    *) [ -z "$TARGET" ] || die "more than one payload given"; TARGET=$1; shift ;;
  esac
done
[ -n "$TARGET" ] || die "no AppDir/AppImage given (see --help)"
[ -e "$TARGET" ] || die "no such file: $TARGET"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/appimage-verify.XXXXXX")
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# ---------------------------------------------------------------------------
# resolve the payload directory
# ---------------------------------------------------------------------------
PAYLOAD=""
APPIMAGE=""
case "$TARGET" in
  *.AppImage)
    APPIMAGE=$(cd "$(dirname "$TARGET")" && pwd)/$(basename "$TARGET")
    [ -x "$APPIMAGE" ] || chmod +x "$APPIMAGE"
    log "extracting $(basename "$APPIMAGE") (no FUSE needed)"
    ( cd "$WORK" && "$APPIMAGE" --appimage-extract >/dev/null 2>&1 ) \
      || die "could not --appimage-extract $APPIMAGE"
    PAYLOAD=$WORK/squashfs-root
    ;;
  *)
    PAYLOAD=$(cd "$TARGET" && pwd)
    ;;
esac
[ -d "$PAYLOAD/usr" ] || die "$PAYLOAD does not look like an AppDir (no usr/)"
log "payload: $PAYLOAD ($(du -sh "$PAYLOAD" | cut -f1))"

mapfile -d '' ELFS < <(find "$PAYLOAD" -type f -print0 | while IFS= read -r -d '' f; do
  is_elf "$f" && printf '%s\0' "$f"
done)
[ ${#ELFS[@]} -gt 0 ] || die "no ELF files in the payload"

# ---------------------------------------------------------------------------
# (a) glibc floor
# ---------------------------------------------------------------------------
max_glibc=0.0
worst=""
abi_refs=()
for elf in "${ELFS[@]}"; do
  v=$(readelf --version-info "$elf" 2>/dev/null | grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' \
        | sed 's/^GLIBC_//' | sort -V | tail -n 1 || true)
  if [ -n "$v" ]; then
    if [ "$(printf '%s\n%s\n' "$max_glibc" "$v" | sort -V | tail -n 1)" = "$v" ] \
       && [ "$v" != "$max_glibc" ]; then
      max_glibc=$v
      worst=$elf
    fi
  fi
  # A GLIBC_ABI_* requirement (e.g. GLIBC_ABI_DT_RELR, added by
  # -z pack-relative-relocs) is versioned differently and slipped past the
  # GLIBC_X.Y pattern above: every one of them implies glibc >= 2.36, i.e. above
  # this payload's floor, so they are collected and reported separately.
  while IFS= read -r abi; do
    [ -n "$abi" ] || continue
    abi_refs+=("${elf#"$PAYLOAD"/}: $abi (implies glibc >= 2.36)")
  done < <(readelf --version-info "$elf" 2>/dev/null | grep -oE 'GLIBC_ABI_[A-Za-z0-9_]+' | sort -u || true)
done
[ "$max_glibc" != "0.0" ] || die "found no GLIBC_* version references at all -- suspicious"
if [ ${#abi_refs[@]} -gt 0 ] && ! version_ge "$MAX_GLIBC" 2.36; then
  printf 'verify: FAIL: GLIBC_ABI_* version requirements, above the %s floor:\n' "$MAX_GLIBC" >&2
  printf '  %s\n' "${abi_refs[@]}" >&2
  exit 1
fi
if ! version_ge "$MAX_GLIBC" "$max_glibc"; then
  die "max GLIBC requirement is $max_glibc (in ${worst#"$PAYLOAD"/}), above the $MAX_GLIBC floor"
fi
if [ ${#abi_refs[@]} -gt 0 ]; then
  ok "${#abi_refs[@]} GLIBC_ABI_* requirement(s) present and allowed by the $MAX_GLIBC floor (they imply 2.36)"
else
  ok "no GLIBC_ABI_* requirement (nothing needs glibc >= 2.36)"
fi
ok "max GLIBC_* version requirement: $max_glibc (limit $MAX_GLIBC), worst file ${worst#"$PAYLOAD"/}"

# ---------------------------------------------------------------------------
# (b) nothing is resolved from the host
# ---------------------------------------------------------------------------
unresolved=()
escapes=()
host_required=()
for elf in "${ELFS[@]}"; do
  while IFS= read -r want; do
    [ -n "$want" ] || continue
    if [ -e "$PAYLOAD/usr/lib/$want" ]; then
      continue
    fi
    if is_excluded "$want"; then
      continue
    fi
    unresolved+=("${elf#"$PAYLOAD"/} needs $want")
  done < <(needed_of "$elf")
done

# The loader's own view, with an empty environment except our lib dir: catches
# a NEEDED that exists in the AppDir but still fails to load (missing libm
# transitively, wrong arch, ...). A host-provided name that is missing *here*
# is not a bundling bug -- it is part of the documented host floor, so it is
# collected and reported instead of failed (CI runners have all of them; a
# minimal container legitimately does not).
for elf in "$PAYLOAD"/usr/bin/*; do
  [ -f "$elf" ] || continue
  is_elf "$elf" || continue
  while IFS= read -r line; do
    case "$line" in
      *"not found"*)
        notfound_name=$(printf '%s\n' "$line" | awk '{print $1}')
        if is_excluded "$notfound_name"; then
          host_required+=("$notfound_name ($(host_reason "$notfound_name"))")
        else
          unresolved+=("$(basename "$elf"): $line")
        fi ;;
      *"=> /"*)
        # The loader line ("/lib64/ld-linux-x86-64.so.2 -> ...") has a path as
        # its first field; only real DT_NEEDED entries (bare SONAMEs) matter.
        case "$line" in
          /*|*" "/*) continue ;;
        esac
        path=$(printf '%s\n' "$line" | awk '{print $3}')
        name=$(printf '%s\n' "$line" | awk '{print $1}')
        case "$path" in
          "$PAYLOAD"/*) ;;
          *) if ! is_excluded "$name"; then escapes+=("$(basename "$elf"): $name -> $path"); fi ;;
        esac ;;
    esac
  done < <(env -i LD_LIBRARY_PATH="$PAYLOAD/usr/lib" ldd "$elf" 2>/dev/null || true)
done

if [ ${#unresolved[@]} -gt 0 ]; then
  printf 'verify: FAIL: unresolved dependencies:\n' >&2
  printf '  %s\n' "${unresolved[@]}" >&2
  exit 1
fi
if [ ${#escapes[@]} -gt 0 ]; then
  printf 'verify: FAIL: libraries loaded from the host:\n' >&2
  printf '  %s\n' "${escapes[@]}" >&2
  exit 1
fi
ok "dependency closure resolves inside the payload (${#ELFS[@]} ELFs checked)"

if [ ${#host_required[@]} -gt 0 ]; then
  mapfile -t host_required < <(printf '%s\n' "${host_required[@]}" | sort -u)
  log "host-provided libraries missing on this host (part of the documented floor, not a failure):"
  printf 'verify:   %s\n' "${host_required[@]}"
fi

# ---------------------------------------------------------------------------
# (c) every library name the shipped binary mentions is decided
# ---------------------------------------------------------------------------
GUI_ELF=$PAYLOAD/usr/bin/media-sort-gui
[ -f "$GUI_ELF" ] || die "usr/bin/media-sort-gui is missing, so the dlopen manifest cannot be derived"
# Derived from the shipped ELF, not from a hand-written list: the manifest is
# every library name the binary carries, i.e. its DT_NEEDED entries *and* the
# names winit/iced/wgpu/x11-dl/rfd dlopen at runtime (libXcursor, libEGL,
# libwayland-egl, libxkbcommon-x11, ...). A new dependency therefore forces a
# decision here instead of shipping green.
# The base-name character class deliberately excludes '.': `strings` emits long
# runs without separators, and a greedy `[A-Za-z0-9_+.-]+` glues "libXi.so.6"
# and "libX11-xcb.so.1" into a single bogus name.
manifest=$(strings "$GUI_ELF" | grep -oE 'lib[A-Za-z0-9_+-]+\.so(\.[0-9]+)*' | sort -u)
[ -n "$manifest" ] || die "the strings manifest of usr/bin/media-sort-gui is empty -- suspicious"
manifest_bundled=()
manifest_host=()
manifest_undecided=()
while IFS= read -r name; do
  [ -n "$name" ] || continue
  if [ -e "$PAYLOAD/usr/lib/$name" ]; then
    manifest_bundled+=("$name")
    continue
  fi
  # A bare "libfoo.so" is a link-time name; it is decided when the payload
  # carries the versioned SONAME the loader would pick for it.
  if [ "${name%.so}" != "$name" ] && compgen -G "$PAYLOAD/usr/lib/$name".[0-9]* >/dev/null; then
    manifest_bundled+=("$name (as $(cd "$PAYLOAD/usr/lib" && ls "$name".[0-9]* | head -n 1))")
    continue
  fi
  if is_excluded "$name"; then
    manifest_host+=("$name -- $(host_reason "$name")")
    continue
  fi
  manifest_undecided+=("$name")
done <<<"$manifest"

log "dlopen manifest of usr/bin/media-sort-gui ($(printf '%s\n' "$manifest" | wc -l) names):"
for entry in "${manifest_bundled[@]}"; do
  printf 'verify:   bundled        %s\n' "$entry"
done
for entry in "${manifest_host[@]}"; do
  printf 'verify:   host-provided  %s\n' "$entry"
done
if [ ${#manifest_undecided[@]} -gt 0 ]; then
  printf 'verify: FAIL: library names mentioned by usr/bin/media-sort-gui are neither bundled nor on the host-provided allowlist:\n' >&2
  printf '  %s\n' "${manifest_undecided[@]}" >&2
  printf 'verify: bundle them (bundle.sh stage) or add them to packaging/appimage/excludelist with a one-line reason.\n' >&2
  exit 1
fi
ok "every library name in the strings manifest is decided (${#manifest_bundled[@]} bundled, ${#manifest_host[@]} host-provided)"

# ---------------------------------------------------------------------------
# (d) libmpv is in the payload
# ---------------------------------------------------------------------------
[ -e "$PAYLOAD/usr/lib/libmpv.so.2" ] || die "usr/lib/libmpv.so.2 is missing"
ok "usr/lib/libmpv.so.2 present ($(du -h "$PAYLOAD/usr/lib/libmpv.so.2" | cut -f1))"

# ---------------------------------------------------------------------------
# (e) ffmpeg really works: version, PNG encode (thumbnails), decode round-trip,
#     and a real AV1 decode with the bundled libdav1d
# ---------------------------------------------------------------------------
FFMPEG=$PAYLOAD/usr/bin/ffmpeg
[ -x "$FFMPEG" ] || die "usr/bin/ffmpeg is missing"
# A synthetic 320x240 RGB frame, fed as rawvideo: no lavfi *input device*
# (that lives in libavdevice, which is deliberately not built) and no test
# pattern generator, while still exercising the real codec/muxer code.
head -c $((320 * 240 * 3)) /dev/zero | tr '\0' '\377' >"$WORK/frame.rgb"
run_ffmpeg() { env -i HOME="$WORK" PATH=/usr/bin:/bin TMPDIR="$WORK" "$FFMPEG" -hide_banner -nostdin "$@"; }
run_ffmpeg -version >"$WORK/ffmpeg-version.txt" 2>&1 || die "ffmpeg -version failed"
grep -qi 'ffmpeg version' "$WORK/ffmpeg-version.txt" || die "ffmpeg -version output looks wrong"
if grep -q -- '--enable-gpl' "$WORK/ffmpeg-version.txt"; then
  die "ffmpeg was configured with --enable-gpl; the AppImage ships an LGPL build"
fi
ok "ffmpeg -version: $(grep -oiE 'ffmpeg version [^ ]+' "$WORK/ffmpeg-version.txt" | head -n 1)"

# A synthetic 320x240 RGB frame, fed as rawvideo: no lavfi *input device*
# (that would need libavdevice, which is deliberately not built) and no test
# pattern generator, while still exercising the real encoder/muxer code.
run_ffmpeg -f rawvideo -pixel_format rgb24 -video_size 320x240 -i /dev/stdin \
  -frames:v 1 -y "$WORK/out.png" <"$WORK/frame.rgb" >"$WORK/ffmpeg-encode.log" 2>&1 \
  || die "ffmpeg failed to encode a PNG (video thumbnails would not work)"
[ "$(head -c 8 "$WORK/out.png" | od -An -tx1 | tr -d ' \n')" = '89504e470d0a1a0a' ] \
  || die "ffmpeg's PNG output has no PNG signature"
[ "$(stat -c %s "$WORK/out.png")" -gt 100 ] || die "ffmpeg's PNG output is empty"
ok "ffmpeg PNG encode works ($(stat -c %s "$WORK/out.png") bytes)"

run_ffmpeg -f rawvideo -pixel_format rgb24 -video_size 320x240 -i /dev/stdin \
  -c:v mpeg4 -y "$WORK/roundtrip.mp4" <"$WORK/frame.rgb" >"$WORK/ffmpeg-mux.log" 2>&1 \
  || die "ffmpeg could not encode a test video"
# This is exactly the shape of the thumbnail call: demux a container, decode a
# frame, write a PNG.
run_ffmpeg -i "$WORK/roundtrip.mp4" -frames:v 1 -y "$WORK/decoded.png" >"$WORK/ffmpeg-decode.log" 2>&1 \
  || die "ffmpeg could not decode the test video back"
[ -s "$WORK/decoded.png" ] || die "ffmpeg's decode round-trip produced nothing"
ok "ffmpeg container decode round-trip works (mp4/mpeg4 -> PNG)"

# A real file the app itself opens. `crates/media-sort-backend/tests/fixtures/`
# contains only images and audio, no video, so this is the repository's own AV1
# clip (Big Buck Bunny, 1280x720, 10 s). Synthetic mpeg4 above would prove
# nothing here: with --disable-autodetect, dropping the H.264/HEVC/VP9 decoders
# would still pass that round-trip.
FIXTURE="$SCRIPT_DIR/../../resources/MockState/mock 3.mp4"
[ -f "$FIXTURE" ] || die "missing decode fixture: $FIXTURE"
# FFmpeg 9 has a native AV1 decoder, so decoding the file alone would not prove
# the bundled libdav1d is there and works (mpv's AV1 path and the Rust
# dav1d-sys AVIF decoder both use it). Assert the decoders the AppImage promises
# first, then decode the real file with libdav1d explicitly.
run_ffmpeg -decoders >"$WORK/ffmpeg-decoders.txt" 2>&1 || die "ffmpeg -decoders failed"
for dec in h264 hevc vp9 libdav1d; do
  grep -qE "[[:space:]]V[A-Za-z.]*[[:space:]]+${dec}[[:space:]]" "$WORK/ffmpeg-decoders.txt" \
    || die "the bundled ffmpeg has no '$dec' decoder (--disable-autodetect dropped it)"
done
ok "bundled decoders present: h264, hevc, vp9, libdav1d"
fixture_info=$(run_ffmpeg -i "$FIXTURE" 2>&1 || true)
fixture_codec=$(printf '%s\n' "$fixture_info" \
  | sed -n 's/.*Stream .*Video: \([a-zA-Z0-9]*\).*/\1/p' | head -n 1)
case "$fixture_codec" in
  h264|hevc|av1|vp9) ;;
  *) die "decode fixture $FIXTURE reports codec '${fixture_codec:-unknown}', not one of h264/hevc/av1/vp9" ;;
esac
run_ffmpeg -v error -c:v libdav1d -i "$FIXTURE" -f null - >"$WORK/ffmpeg-fixture-decode.log" 2>&1 \
  || { cat "$WORK/ffmpeg-fixture-decode.log" >&2; die "ffmpeg could not decode $FIXTURE with the bundled libdav1d"; }
if [ -s "$WORK/ffmpeg-fixture-decode.log" ]; then
  cat "$WORK/ffmpeg-fixture-decode.log" >&2
  die "ffmpeg reported errors while decoding $FIXTURE"
fi
ok "ffmpeg decodes a real $fixture_codec fixture with libdav1d ($(basename "$FIXTURE"), $(stat -c %s "$FIXTURE") bytes)"

# ---------------------------------------------------------------------------
# (f) the GUI gets past the dynamic loader, and does not crash or exit silently
# ---------------------------------------------------------------------------
if [ "$SKIP_GUI" = 1 ]; then
  log "skipping GUI launch check (--skip-gui)"
else
  command -v xvfb-run >/dev/null || die "xvfb-run is required for the GUI check (install xvfb, or pass --skip-gui)"

  if [ -n "$APPIMAGE" ]; then
    gui_cmd=("$APPIMAGE" --appimage-extract-and-run)
    gui_name=$(basename "$APPIMAGE")
  else
    gui_cmd=("$PAYLOAD/usr/bin/media-sort-gui")
    gui_name=media-sort-gui
  fi

  # Window probe: informational only, never fatal. A GPU-less Xvfb cannot open a
  # wgpu surface, but winit creates the window before that, so its presence is a
  # useful signal. It runs inside the same xvfb-run session (the probe needs the
  # DISPLAY xvfb-run picked) and is skipped when xdotool is not installed.
  probe=0
  if command -v xdotool >/dev/null; then probe=1; fi
  window_log=$WORK/gui-window.log
  : >"$window_log"

  gui_start_ms=$(date +%s%3N)
  set +e
  env -i HOME="$WORK" PATH=/usr/bin:/bin TMPDIR="$WORK" XDG_RUNTIME_DIR="$WORK" \
    PROBE="$probe" WINDOW_LOG="$window_log" \
    timeout 20 xvfb-run -a bash -c '
      "$@" &
      app=$!
      if [ "${PROBE:-0}" != 1 ]; then wait "$app"; exit $?; fi
      n=0
      while [ "$n" -lt 15 ]; do
        n=$((n + 1))
        kill -0 "$app" 2>/dev/null || break
        sleep 1
        if [ -n "$(xdotool search --name "Media Sort" 2>/dev/null | head -n 1)" ]; then
          printf "window \"Media Sort\" present after %ss\n" "$n" >>"$WINDOW_LOG"
          break
        fi
      done
      wait "$app"
    ' gui-exec "${gui_cmd[@]}" >"$WORK/gui.log" 2>&1
  gui_status=$?
  set -e
  gui_ms=$(( $(date +%s%3N) - gui_start_ms ))

  # A versioned-symbol failure ("version `ALSA_1.2.15' not found") is a loader
  # error too, and it is what a bundled plugin host produces (see excludelist
  # section 3): fail on it like on a missing library.
  if grep -qiE 'error while loading shared libraries|cannot open shared object file|symbol lookup error|version .[A-Za-z0-9_]+. not found' "$WORK/gui.log"; then
    cat "$WORK/gui.log" >&2
    die "$gui_name failed to start: shared library problem"
  fi

  if [ -s "$window_log" ]; then
    log "window probe: $(cat "$window_log")"
  elif [ "$probe" = 1 ]; then
    log "window probe: no window named \"Media Sort\" appeared (informational; a GPU-less Xvfb may never open one)"
  else
    log "window probe: xdotool not installed, skipping (informational only)"
  fi

  case "$gui_status" in
    124)
      ok "$gui_name still running after ${gui_ms} ms under Xvfb (expected for a GUI)"
      ;;
    0)
      cat "$WORK/gui.log" >&2
      die "$gui_name exited 0 after ${gui_ms} ms under Xvfb: a GUI that quit immediately is a regression, not a pass"
      ;;
    126|127)
      cat "$WORK/gui.log" >&2
      die "$gui_name could not be executed under Xvfb (status $gui_status: not executable / command not found)"
      ;;
    134|136|139)
      cat "$WORK/gui.log" >&2
      case "$gui_status" in
        134) sig=SIGABRT ;;
        136) sig=SIGFPE ;;
        *)   sig=SIGSEGV ;;
      esac
      die "$gui_name died from $sig (status $gui_status) after ${gui_ms} ms under Xvfb"
      ;;
    *)
      # A GPU-less Xvfb has no Vulkan/GL device, so iced can legitimately exit;
      # that is not what this gate is about. But an *early* exit without a
      # driver/graphics complaint is not that: it is a crash or a silent quit
      # that used to pass as "no loader errors".
      if [ "$gui_ms" -lt 3000 ] \
         && ! grep -qiE 'adapter|vulkan|gpu|surface|egl|gl context|graphics|winit|display' "$WORK/gui.log"; then
        cat "$WORK/gui.log" >&2
        die "$gui_name exited with status $gui_status after ${gui_ms} ms without a GPU/graphics complaint: an early crash or silent exit, not the expected GPU-less failure"
      fi
      log "note: $gui_name exited with status $gui_status after ${gui_ms} ms under Xvfb (no GPU);"
      log "      no loader errors, which is what this check enforces. Last lines:"
      sed -n '$p' "$WORK/gui.log" | sed 's/^/      /'
      ;;
  esac
fi

log "payload summary: $(du -sh "$PAYLOAD" | cut -f1) total, $(ls "$PAYLOAD/usr/lib" | wc -l) libraries, ffmpeg $(du -h "$PAYLOAD/usr/bin/ffmpeg" | cut -f1)"
log "PASS"
