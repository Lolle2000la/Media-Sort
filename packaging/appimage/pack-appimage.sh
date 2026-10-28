#!/usr/bin/env bash
#
# Pack the self-contained Linux AppImage, in the two passes vpk needs.
#
#   pack-appimage.sh <version> <channel>
#
# The flow (shared by .github/workflows/release.yml and
# .github/workflows/appimage.yml so the layout and the .desktop surgery cannot
# drift apart):
#
#   1. stage just the compiled binary into dist/linux-staging and let vpk
#      generate the AppDir skeleton + update manifests;
#   2. unpack that intermediate AppImage (`--appimage-extract`, so no FUSE),
#      drop the baseline outputs and bundle the payload into the AppDir with
#      `bundle.sh install` (libmpv, ffmpeg and their closure, $ORIGIN RPATHs);
#   3. gate the payload with `verify.sh` *before* it is sealed;
#   4. surgically fix vpk's desktop entry (keeping its vendor metadata) and
#      mirror it to usr/share/applications;
#   5. re-pack the finished AppDir into the AppImage that ships.
#
# Environment:
#   TARGET_DIR  directory holding the built media-sort-gui
#               (default: target/x86_64-unknown-linux-gnu/release)
#   STAGE_DIR   payload staged by `bundle.sh stage` inside the container
#               (default: target/appimage-stage)
#
# Requires `vpk`, `mksquashfs` (squashfs-tools) and `patchelf` on PATH, plus the
# staged payload. Run from the repository root.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

die() { printf 'pack-appimage: ERROR: %s\n' "$*" >&2; exit 1; }
log() { printf 'pack-appimage: %s\n' "$*"; }

# The one AppImage in Releases/: the intermediate artifact after pass 1, the
# shippable one after pass 2. Fails when the count is not exactly one, so a
# stale file from an earlier run can never be picked (or shipped) by accident.
sole_appimage() {
  local found=()
  mapfile -t found < <(ls Releases/*.AppImage 2>/dev/null || true)
  [ ${#found[@]} -eq 1 ] \
    || die "expected exactly one AppImage in Releases/, found ${#found[@]}: ${found[*]:-none}"
  printf '%s\n' "${found[0]}"
}

usage() {
  sed -n '2,28p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit 2
}

VERSION=${1:-}
CHANNEL=${2:-}
[ -n "$VERSION" ] || usage
[ -n "$CHANNEL" ] || usage

TARGET_DIR=${TARGET_DIR:-target/x86_64-unknown-linux-gnu/release}
STAGE_DIR=${STAGE_DIR:-target/appimage-stage}

for tool in vpk mksquashfs patchelf; do
  command -v "$tool" >/dev/null || die "$tool is required on PATH"
done
[ -x "$TARGET_DIR/media-sort-gui" ] || die "no $TARGET_DIR/media-sort-gui (build the payload in the Ubuntu 22.04 container first)"
[ -d "$STAGE_DIR/lib" ] || die "no staged payload in $STAGE_DIR (run packaging/appimage/build-in-container.sh first)"

# Pass 1: let vpk build the AppDir skeleton from the bare binary. Any AppImage a
# previous run left in Releases/ goes first: the lookup below has to find exactly
# the one this pass produced, and `ls Releases/*.AppImage | head -n 1` would
# otherwise silently pick a stale artifact on a documented re-run.
mkdir -p dist/linux-staging Releases
cp "$TARGET_DIR/media-sort-gui" dist/linux-staging/
rm -f Releases/*.AppImage

log "pass 1: vpk pack $VERSION ($CHANNEL) from the bare binary"
vpk pack -u MediaSort -v "$VERSION" -p dist/linux-staging -e media-sort-gui \
  --icon packaging/linux/icon.svg --channel "$CHANNEL" --runtime "$CHANNEL"

# Unpack the intermediate AppImage artifact; exactly one must exist (sole_appimage
# dies inside its subshell, so the `|| exit 1` propagates that here).
APPIMAGE_PATH=$(sole_appimage) || exit 1
chmod +x "$APPIMAGE_PATH"
"$APPIMAGE_PATH" --appimage-extract

# Purge the baseline packages to prevent circular duplication (the AppImages are
# already gone, above). The second pass writes the packages that ship.
rm -f Releases/*.nupkg

# Bundle the payload: target/appimage-stage was produced inside the Ubuntu 22.04
# container by build-in-container.sh. bundle.sh installs it into usr/lib +
# usr/bin and writes the $ORIGIN RPATHs, so nothing is resolved from the host
# except the host-provided allowlist in packaging/appimage/excludelist.
bash "$SCRIPT_DIR/bundle.sh" install squashfs-root "$STAGE_DIR"

# Gate the payload before sealing it: glibc floor, NEEDED resolution, the strings
# manifest, ffmpeg encode/decode, and a GUI start under Xvfb.
bash "$SCRIPT_DIR/verify.sh" squashfs-root

# Surgically modify the vpk-generated desktop file (preserves vendor metadata).
TARGET_DESKTOP=squashfs-root/MediaSort.desktop
[ -f "$TARGET_DESKTOP" ] || die "vpk did not produce $TARGET_DESKTOP"
sed -i 's/^Name=.*/Name=Media Sort/' "$TARGET_DESKTOP"
sed -i 's/^Exec=.*/& %U/' "$TARGET_DESKTOP"
sed -i 's/^StartupWMClass=.*/StartupWMClass=MediaSort/' "$TARGET_DESKTOP"
printf '\n%s\n' "MimeType=inode/directory;" >> "$TARGET_DESKTOP"

# Mirror the adjusted launcher to the standard layout path.
mkdir -p squashfs-root/usr/share/applications
cp "$TARGET_DESKTOP" squashfs-root/usr/share/applications/MediaSort.desktop

# Re-stage into a clean AppDir context and execute the final compilation.
rm -rf dist
mkdir -p dist
mv squashfs-root dist/MediaSort.AppDir

log "pass 2: vpk pack $VERSION ($CHANNEL) from the finished AppDir"
vpk pack -u MediaSort -v "$VERSION" -p dist/MediaSort.AppDir -e media-sort-gui \
  --channel "$CHANNEL" --runtime "$CHANNEL"

final_appimage=$(sole_appimage) || exit 1
log "done: $final_appimage"
