#!/usr/bin/env bash
#
# Gate the AppImage payload. Fails the build unless all of the following hold:
#
#   a) no ELF in the payload requires a GLIBC_* version above the floor
#      (default 2.35, the jammy glibc the payload is built against);
#   b) every DT_NEEDED of the packaged ELFs resolves from inside the AppDir,
#      except the glibc family and GPU driver stack in ./excludelist;
#   c) usr/lib/libmpv.so.2 is present;
#   d) the bundled ffmpeg runs, encodes a PNG (video thumbnails) and decodes it
#      back, with an empty environment;
#   e) the GUI starts under a virtual X server without a single
#      "error while loading shared libraries" (it may die later for lack of a
#      GPU, which is not this check's business).
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
    $1 == n { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "$EXCLUDELIST" && return 0
  case "$name" in
    libGL*.so*|libEGL*.so*|libOpenGL*.so*|\
    libdrm*.so*|libgbm*.so*|libglapi*.so*|libvulkan*.so*|\
    libnvidia*.so*|libcuda*.so*|libmali*.so*|libd3d*.so*|libwsl*.so*|\
    *_drv_video.so|*_driver_video.so)
      return 0 ;;
  esac
  return 1
}

needed_of() {
  readelf -d "$1" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --max-glibc) MAX_GLIBC=$2; shift 2 ;;
    --skip-gui) SKIP_GUI=1; shift ;;
    -h|--help) sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
for elf in "${ELFS[@]}"; do
  v=$(readelf --version-info "$elf" 2>/dev/null | grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' \
        | sed 's/^GLIBC_//' | sort -V | tail -n 1 || true)
  [ -n "$v" ] || continue
  if [ "$(printf '%s\n%s\n' "$max_glibc" "$v" | sort -V | tail -n 1)" = "$v" ] \
     && [ "$v" != "$max_glibc" ]; then
    max_glibc=$v
    worst=$elf
  fi
done
[ "$max_glibc" != "0.0" ] || die "found no GLIBC_* version references at all -- suspicious"
if [ "$(printf '%s\n%s\n' "$max_glibc" "$MAX_GLIBC" | sort -V | tail -n 1)" != "$MAX_GLIBC" ]; then
  die "max GLIBC requirement is $max_glibc (in ${worst#"$PAYLOAD"/}), above the $MAX_GLIBC floor"
fi
ok "max GLIBC_* version requirement: $max_glibc (limit $MAX_GLIBC), worst file ${worst#"$PAYLOAD"/}"

# ---------------------------------------------------------------------------
# (b) nothing is resolved from the host
# ---------------------------------------------------------------------------
unresolved=()
escapes=()
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
# transitively, wrong arch, ...).
for elf in "$PAYLOAD"/usr/bin/*; do
  [ -f "$elf" ] || continue
  is_elf "$elf" || continue
  while IFS= read -r line; do
    case "$line" in
      *"not found"*)
        unresolved+=("$(basename "$elf"): $line") ;;
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

# ---------------------------------------------------------------------------
# (c) libmpv is in the payload
# ---------------------------------------------------------------------------
[ -e "$PAYLOAD/usr/lib/libmpv.so.2" ] || die "usr/lib/libmpv.so.2 is missing"
ok "usr/lib/libmpv.so.2 present ($(du -h "$PAYLOAD/usr/lib/libmpv.so.2" | cut -f1))"

# ---------------------------------------------------------------------------
# (d) ffmpeg really works: version, PNG encode (thumbnails), decode round-trip
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

# ---------------------------------------------------------------------------
# (e) the GUI gets past the dynamic loader
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

  set +e
  env -i HOME="$WORK" PATH=/usr/bin:/bin TMPDIR="$WORK" XDG_RUNTIME_DIR="$WORK" \
    timeout 20 xvfb-run -a "${gui_cmd[@]}" >"$WORK/gui.log" 2>&1
  gui_status=$?
  set -e

  if grep -qiE 'error while loading shared libraries|cannot open shared object file|symbol lookup error' "$WORK/gui.log"; then
    cat "$WORK/gui.log" >&2
    die "$gui_name failed to start: shared library problem"
  fi
  if [ "$gui_status" = 124 ]; then
    ok "$gui_name still running after 20s under Xvfb (expected for a GUI)"
  else
    # A GPU-less Xvfb has no Vulkan/GL device, so iced can legitimately exit
    # here; that is not what this gate is about.
    log "note: $gui_name exited with status $gui_status under Xvfb (no GPU);"
    log "      no loader errors, which is what this check enforces. Last lines:"
    sed -n '$p' "$WORK/gui.log" | sed 's/^/      /'
  fi
fi

log "payload summary: $(du -sh "$PAYLOAD" | cut -f1) total, $(ls "$PAYLOAD/usr/lib" | wc -l) libraries, ffmpeg $(du -h "$PAYLOAD/usr/bin/ffmpeg" | cut -f1)"
log "PASS"
