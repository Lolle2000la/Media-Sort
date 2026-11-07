#!/usr/bin/env bash
#
# Give the AppImage its AppImageUpdate information and its .zsync.
#
#   appimage-update-info.sh patch-runtime <vpk-channel> <update-info>
#   appimage-update-info.sh write-zsync   <appimage>
#   appimage-update-info.sh check         <appimage>
#   appimage-update-info.sh update-info-string <owner> <repo> <version> <asset>
#   appimage-update-info.sh --selftest
#
# `vpk pack` concatenates a vendored AppImageKit type-2 runtime with a squashfs,
# and that runtime carries a 1024-byte `.upd_info` section that vpk leaves all
# zeroes (`--appimage-updateinformation` prints an empty line). AppImageUpdate
# reads exactly that section, so the flow fills it *in vpk's copy of the runtime
# before packing* rather than patching the finished AppImage: the standalone
# `Releases/*.AppImage`, the byte-identical copy velopack wraps into the
# `.nupkg` (lib/app/MediaSort.AppImage) and the SHA1/SHA256/Size hashes vpk
# writes into releases.*.json all derive from that one file. Patching only the
# finished AppImage would silently undo itself on the first in-app self-update,
# when velopack extracts the .nupkg's copy over the running binary; patching the
# .nupkg instead would mean recomputing those hashes and re-signing.
#
# `write-zsync` must run on the *final* AppImage bytes (after the second vpk
# pass) and emits `<AppImage>.zsync` beside it. Both files are release assets,
# and the update-information string points AppImageUpdate at the .zsync.
#
# The channel in the update-information string is strict: a stable version
# (no `-`) gets `latest`, a prerelease gets `latest-pre`, so a stable user is
# never offered a prerelease. It is baked in per build; there is no runtime
# substitution. The asset glob is the exact .zsync name, because both
# architectures are attached to the same release and a wildcard would let
# AppImageUpdate pick the lexicographically greatest match, i.e. the wrong arch.
#
# Environment:
#   APPIMAGE_RUNTIME_PATH  explicit path to a runtime file, instead of looking
#                          it up in the vpk tool store (for local testing)
#
# Requires `readelf` (binutils); `write-zsync` additionally requires
# `zsyncmake` (the zsync package). Run from the repository root.
set -euo pipefail

export LC_ALL=C

die() { printf 'appimage-update-info: ERROR: %s\n' "$*" >&2; exit 1; }
log() { printf 'appimage-update-info: %s\n' "$*"; }
ok()  { printf 'appimage-update-info: ok: %s\n' "$*"; }

# ---------------------------------------------------------------------------
# pure helpers (probed by --selftest)
# ---------------------------------------------------------------------------

# The AppImageUpdate channel for a version: strict prerelease separation, so a
# stable AppImage can never be updated onto a prerelease build. This MUST stay
# coupled to release.yml's authoritative classifier (the
# `[[ "$RAW_VERSION" == *"-"* ]]` / `--prerelease` decision in the publish job):
# a version is a prerelease exactly when its tag contains `-`.
channel_for_version() { # <version>
  case "$1" in
    *-*) printf 'latest-pre\n' ;;
    *)   printf 'latest\n' ;;
  esac
}

# gh-releases-zsync|<owner>|<repo>|<channel>|<asset>. The asset is matched
# against release asset names (AppImageUpdate prefixes its glob with `*`), so it
# has to be the exact `<AppImage>.zsync` name and not a wildcard.
update_info_string() { # <owner> <repo> <version> <asset>
  local owner=$1 repo=$2 version=$3 asset=$4
  [ -n "$owner" ] || die "empty repository owner"
  [ -n "$repo" ] || die "empty repository name"
  [ -n "$asset" ] || die "empty asset name"
  printf 'gh-releases-zsync|%s|%s|%s|%s\n' \
    "$owner" "$repo" "$(channel_for_version "$version")" "$asset"
}

# vpk's channel name -> the vendored runtime filename.
runtime_name_for_channel() { # <vpk-channel>
  case "$1" in
    linux-x64)   printf 'appimagekit-runtime-x86_64\n' ;;
    linux-arm64) printf 'appimagekit-runtime-aarch64\n' ;;
    *) return 1 ;;
  esac
}

# Does an update-information string use a scheme AppImageUpdate understands?
update_info_prefix_ok() { # <string>
  case "$1" in
    'zsync|'*|'gh-releases-zsync|'*|'pling-v1-zsync|'*) return 0 ;;
    *) return 1 ;;
  esac
}

# Offset and size (both hex) of the .upd_info section from `readelf -SW`
# output. The optional `[ 3]` index is stripped first: readelf pads small
# indices with a space (`[ 3]`), which would otherwise shift every column.
parse_upd_info() { # <readelf -SW output>
  printf '%s\n' "$1" \
    | sed -nE 's/^[[:space:]]*(\[[[:space:]]*[0-9]+\][[:space:]]+)?\.upd_info[[:space:]]+[^[:space:]]+[[:space:]]+[^[:space:]]+[[:space:]]+([0-9a-fA-F]+)[[:space:]]+([0-9a-fA-F]+).*/\2 \3/p' \
    | head -n 1
}

# One header of a zsync control file (`Filename:`, `SHA-1:`, ...). The headers
# are plain text lines before the binary checksum block.
zsync_header() { # <zsync file> <header name>
  sed -n "s/^$2: //p" "$1" | head -n 1 | tr -d '\r'
}

# ---------------------------------------------------------------------------
# runtime lookup and patching
# ---------------------------------------------------------------------------

# The store `dotnet tool install -g vpk` puts next to the tool. Deliberately
# derived from the *resolved* vpk path and from nowhere else: a vpk shim earlier
# in PATH must never make us patch another install's runtime (vpk would then
# pack an unpatched one and the failure would only surface later as an empty
# `.upd_info`). Pure, so --selftest can probe it.
vpk_store_for_bin() { # <resolved vpk path>
  printf '%s/.store/vpk\n' "$(dirname "$1")"
}

# The store for a vpk *as found on PATH*: resolve the tool through any symlinks
# first, then derive the store from the real binary's directory. Locating the
# store is what has to do the resolution, so the regression this guards against
# (using the shim's directory) fails --selftest, not only coreutils.
vpk_store_for_shim() { # <vpk path as found on PATH>
  local resolved
  resolved=$(readlink -f "$1") || return 1
  vpk_store_for_bin "$resolved"
}

# Locate vpk's vendored runtime. `dotnet tool install -g vpk` lays the tool out
# under <tools>/.store/vpk/<version>/vpk/<version>/vendor/. Fail loudly when
# that layout changes rather than guessing: a silent miss would ship an AppImage
# without update information, which is the defect this whole script exists for.
locate_runtime() { # <runtime filename>
  local name=$1 vpk_bin store candidates=()
  if [ -n "${APPIMAGE_RUNTIME_PATH:-}" ]; then
    [ -f "$APPIMAGE_RUNTIME_PATH" ] \
      || die "APPIMAGE_RUNTIME_PATH=$APPIMAGE_RUNTIME_PATH does not exist"
    printf '%s\n' "$APPIMAGE_RUNTIME_PATH"
    return 0
  fi
  vpk_bin=$(command -v vpk) || die "vpk is not on PATH (install it with: dotnet tool install -g vpk)"
  # Resolve symlinks first: the runtime vpk concatenates is the one beside the
  # real tool, not beside a shim that happens to be earlier in PATH.
  store=$(vpk_store_for_shim "$vpk_bin") \
    || die "cannot resolve the vpk on PATH to a real file: $vpk_bin"
  [ -d "$store" ] \
    || die "the vpk on PATH is $vpk_bin, but its tool store $store does not exist (self-contained or shimmed vpk?) -- set APPIMAGE_RUNTIME_PATH to the runtime file vpk actually uses"
  while IFS= read -r candidate; do
    candidates+=("$candidate")
  done < <(find "$store" -type f -name "$name" -path '*/vendor/*' 2>/dev/null | sort)
  case ${#candidates[@]} in
    0) die "vpk's vendored AppImage runtime '$name' was not found under $store (vpk vendor layout changed?) -- set APPIMAGE_RUNTIME_PATH to the runtime file vpk actually uses" ;;
    1) printf '%s\n' "${candidates[0]}" ;;
    *) printf 'appimage-update-info: ERROR: more than one vendored %s under %s, cannot tell which one vpk will use:\n' "$name" "$store" >&2
       printf '  %s\n' "${candidates[@]}" >&2
       die "set APPIMAGE_RUNTIME_PATH to the runtime vpk actually uses" ;;
  esac
}

# `patch-runtime <vpk-channel> <update-info>`: zero the .upd_info section, write
# the string NUL-padded, then prove it by asking the runtime itself. Running the
# runtime directly works because the section lives in the ELF header part; no
# squashfs is needed. Zeroing first keeps this idempotent: a shorter string must
# not leave a stale tail from an earlier, longer one.
cmd_patch_runtime() {
  local channel=$1 info=$2 runtime layout offset_hex size_hex offset size need got
  [ -n "$channel" ] || die "patch-runtime: no channel given"
  [ -n "$info" ] || die "patch-runtime: empty update-information string"
  command -v readelf >/dev/null || die "readelf is required (install binutils)"
  local runtime_name
  runtime_name=$(runtime_name_for_channel "$channel") \
    || die "patch-runtime: unknown vpk channel '$channel' (expected linux-x64 or linux-arm64)"
  runtime=$(locate_runtime "$runtime_name")
  [ -f "$runtime" ] || die "no runtime file at $runtime"
  layout=$(readelf -SW "$runtime" 2>/dev/null) \
    || die "readelf cannot read $runtime"
  read -r offset_hex size_hex <<<"$(parse_upd_info "$layout")" || true
  [ -n "${offset_hex:-}" ] && [ -n "${size_hex:-}" ] \
    || die "$runtime has no .upd_info section (vpk's vendored runtime changed?)"
  [[ $offset_hex =~ ^[0-9a-fA-F]+$ ]] || die "unparseable .upd_info offset '$offset_hex' in $runtime"
  [[ $size_hex =~ ^[0-9a-fA-F]+$ ]] || die "unparseable .upd_info size '$size_hex' in $runtime"
  offset=$((16#$offset_hex))
  size=$((16#$size_hex))
  need=$(printf '%s' "$info" | wc -c)
  # One byte stays NUL, which is what makes the section a C string.
  [ "$need" -lt "$size" ] \
    || die "update-information string is $need bytes but .upd_info holds only $size (needs a NUL terminator)"

  log "patching $runtime (.upd_info: offset $offset, size $size)"
  dd if=/dev/zero of="$runtime" bs=1 seek="$offset" count="$size" conv=notrunc status=none
  printf '%s' "$info" | dd of="$runtime" bs=1 seek="$offset" conv=notrunc status=none

  got=$("$runtime" --appimage-updateinformation 2>/dev/null) \
    || die "patched runtime $runtime cannot be executed with --appimage-updateinformation (wrong architecture?)"
  [ "$got" = "$info" ] \
    || die "runtime reports '$got' after patching, expected '$info'"
  ok "runtime now reports: $got"
}

# ---------------------------------------------------------------------------
# .zsync
# ---------------------------------------------------------------------------

verify_zsync() { # <appimage> <zsync>
  local appimage=$1 zsync=$2 base want_sha got_name got_sha
  base=$(basename "$appimage")
  [ -f "$zsync" ] || die "no zsync file at $zsync"
  want_sha=$(sha1sum "$appimage" | cut -d' ' -f1)
  got_name=$(zsync_header "$zsync" Filename)
  got_sha=$(zsync_header "$zsync" SHA-1)
  [ "$got_name" = "$base" ] \
    || die "$zsync names '$got_name', expected the AppImage basename '$base'"
  [ -n "$got_sha" ] || die "$zsync has no SHA-1 header"
  [ "$got_sha" = "$want_sha" ] \
    || die "$zsync SHA-1 $got_sha does not match $base ($want_sha)"
}

# `write-zsync <appimage>`: generate the zsync control file from the final
# AppImage and prove it describes exactly those bytes. `-u` is the relative
# download URL appimagetool itself uses; GitHub serves the .zsync next to the
# AppImage, so resolving it against the .zsync's own URL is correct.
cmd_write_zsync() {
  local appimage=$1 zsync
  [ -n "$appimage" ] || die "write-zsync: no AppImage given"
  [ -f "$appimage" ] || die "write-zsync: no such file: $appimage"
  command -v zsyncmake >/dev/null || die "zsyncmake is required (install the zsync package)"
  zsync=$appimage.zsync
  log "generating $(basename "$zsync") from $(basename "$appimage")"
  zsyncmake -u "$(basename "$appimage")" -o "$zsync" "$appimage" >/dev/null
  verify_zsync "$appimage" "$zsync"
  ok "$(basename "$zsync") describes $(basename "$appimage")"
}

# `check <appimage>`: the gate. The update-information section must be non-empty
# and use a known scheme, and the .zsync beside it must describe the exact
# AppImage bytes. Called by verify.sh on the sealed AppImage.
cmd_check() {
  local appimage=$1 info zsync
  [ -n "$appimage" ] || die "check: no AppImage given"
  [ -f "$appimage" ] || die "check: no such file: $appimage"
  [ -x "$appimage" ] || die "check: $appimage is not executable"
  info=$("$appimage" --appimage-updateinformation 2>/dev/null) \
    || die "check: could not read the update information from $appimage"
  [ -n "$info" ] \
    || die "check: .upd_info in $appimage is empty, so AppImageUpdate cannot update it"
  update_info_prefix_ok "$info" \
    || die "check: unrecognised update information '$info' (expected zsync|, gh-releases-zsync| or pling-v1-zsync|)"
  zsync=$appimage.zsync
  [ -f "$zsync" ] \
    || die "check: no $(basename "$zsync") beside $appimage (publish it with the AppImage)"
  verify_zsync "$appimage" "$zsync"
  ok "$(basename "$appimage"): $info"
  ok "$(basename "$zsync") is present and matches"
}

# ---------------------------------------------------------------------------
# selftest: the pure logic only (no vpk, no AppImage, no zsyncmake)
# ---------------------------------------------------------------------------
selftest() {
  local dir failures=0
  dir=$(mktemp -d "${TMPDIR:-/tmp}/appimage-update-info-selftest.XXXXXX")

  expect() { # <label> <expected> <got>
    if [ "$2" = "$3" ]; then
      ok "selftest: $1 -> $3"
    else
      printf 'appimage-update-info: FAIL: selftest %s: expected %q, got %q\n' \
        "$1" "$2" "$3" >&2
      failures=$((failures + 1))
    fi
  }

  # Stable vs prerelease channel selection. A missing `-` arm here is what would
  # offer stable users a prerelease.
  expect "channel 3.0.1" latest "$(channel_for_version 3.0.1)"
  expect "channel 3.0.0-alpha.19" latest-pre "$(channel_for_version 3.0.0-alpha.19)"
  expect "channel 3.0.0-rc.1" latest-pre "$(channel_for_version 3.0.0-rc.1)"

  # Whole update-information strings, including the exact asset name in the
  # last field (a wildcard here would let AppImageUpdate pick the other arch).
  expect "update info stable" \
    'gh-releases-zsync|Lolle2000la|Media-Sort|latest|MediaSort-linux-x64.AppImage.zsync' \
    "$(update_info_string Lolle2000la Media-Sort 3.0.1 MediaSort-linux-x64.AppImage.zsync)"
  expect "update info prerelease" \
    'gh-releases-zsync|Lolle2000la|Media-Sort|latest-pre|MediaSort-linux-arm64.AppImage.zsync' \
    "$(update_info_string Lolle2000la Media-Sort 3.0.0-alpha.19 MediaSort-linux-arm64.AppImage.zsync)"

  # Runtime mapping: both shipped arches, and an unknown channel must be
  # rejected rather than silently mapped to a wrong runtime.
  expect "runtime for linux-x64" appimagekit-runtime-x86_64 "$(runtime_name_for_channel linux-x64)"
  expect "runtime for linux-arm64" appimagekit-runtime-aarch64 "$(runtime_name_for_channel linux-arm64)"
  expect "runtime for unknown channel" rejected \
    "$(runtime_name_for_channel linux-riscv64 && echo accepted || echo rejected)"

  # The runtime lookup is derived from the *resolved* vpk path and from nowhere
  # else. The first probe pins the store to the tool's own directory (so a
  # shim's store can never fall back to $HOME/.dotnet/tools); the second feeds a
  # symlink straight into vpk_store_for_shim -- the function locate_runtime uses
  # -- so deleting the resolution there makes this probe fail. Expected values
  # are canonicalised because TMPDIR itself may be reached through a symlink.
  expect "store beside vpk" "/opt/tools/.store/vpk" "$(vpk_store_for_bin /opt/tools/vpk)"
  mkdir -p "$dir/shim" "$dir/real/tools"
  : >"$dir/real/tools/vpk"
  ln -s ../real/tools/vpk "$dir/shim/vpk"
  expect "store beside resolved vpk" "$(readlink -f "$dir/real/tools")/.store/vpk" \
    "$(vpk_store_for_shim "$dir/shim/vpk")"

  # .upd_info parsing, including a space-padded section index (`[ 3]`) and the
  # real 1024-byte section of vpk's x86_64 runtime.
  local sample
  sample='  [ 3] .note.gnu.property PROGBITS 0000000000000338 000338 000020 00   A  0  0  8
  [23] .upd_info         PROGBITS 00000000000e3910 0e3910 000400 00  WA  0  0  1'
  expect ".upd_info offset/size" "0e3910 000400" "$(parse_upd_info "$sample")"
  expect ".upd_info absent" "" "$(parse_upd_info '  [ 1] .text PROGBITS 0000000000001000 001000 000100 00 AX 0 0 16')"

  # Scheme acceptance.
  local info expected got
  for info in \
    'pass|zsync|https://example.test/file.zsync' \
    'pass|gh-releases-zsync|owner|repo|latest|app.AppImage.zsync' \
    'pass|pling-v1-zsync|https://example.test/file.zsync' \
    'fail|' \
    'fail|https://example.test/file.zsync' \
    'fail|gh-releases-zsync'; do
    expected=${info%%|*}
    info=${info#*|}
    got=fail
    update_info_prefix_ok "$info" && got=pass
    expect "scheme '$info'" "$expected" "$got"
  done

  # zsync header parsing against a control file shaped like zsyncmake's output.
  cat >"$dir/control" <<'EOF'
zsync: 0.6.2
Filename: MediaSort-linux-x64.AppImage
MTime: Mon, 05 Jan 2026 10:00:00 +0000
Blocksize: 2048
Length: 123456
URL: MediaSort-linux-x64.AppImage
SHA-1: 0123456789abcdef0123456789abcdef01234567
EOF
  expect "zsync Filename" "MediaSort-linux-x64.AppImage" "$(zsync_header "$dir/control" Filename)"
  expect "zsync SHA-1" "0123456789abcdef0123456789abcdef01234567" "$(zsync_header "$dir/control" SHA-1)"

  rm -rf "$dir"
  [ "$failures" -eq 0 ] || die "$failures selftest probe(s) decided the wrong way"
  ok "selftest: every probe decided as intended"
}

usage() {
  sed -n '2,39p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit 2
}

case "${1:-}" in
  patch-runtime)        [ $# -eq 3 ] || usage; cmd_patch_runtime "$2" "$3" ;;
  write-zsync)          [ $# -eq 2 ] || usage; cmd_write_zsync "$2" ;;
  check)                [ $# -eq 2 ] || usage; cmd_check "$2" ;;
  update-info-string)   [ $# -eq 5 ] || usage; update_info_string "$2" "$3" "$4" "$5" ;;
  --selftest)           selftest ;;
  -h|--help)            usage ;;
  *) usage ;;
esac
