#!/usr/bin/env bash
#
# Install a built .snap and check that it resolves and starts inside its own
# confinement.
#
# Usage: scripts/check-snap-confinement.sh <file.snap>
#
# This covers the part an ordinary build cannot see: the snap links against the
# Ubuntu archive's libraries rather than the runner's, and takes its graphics
# userspace from the `gpu-2404` content provider rather than shipping it (see
# AGENTS.md). Needs sudo to install the snap and that provider.
#
# Called both by `.github/workflows/snap.yml` (the recipe check) and by
# `.github/workflows/snap-release.yml` (which must not publish a snap that fails
# it), so it lives here rather than inline in either workflow.
set -euo pipefail

snap_file="${1:?usage: check-snap-confinement.sh <file.snap>}"
if [ ! -f "$snap_file" ]; then
  echo "::error::$snap_file does not exist"
  exit 1
fi

echo "Installing $snap_file"
sudo snap install --dangerous "$snap_file"
snap list media-sort

# The snap carries no graphics userspace of its own: it comes from the
# `gpu-2404` content provider, which the Store installs as a default-provider
# dependency. A local `--dangerous` install does not pull it in, so do it here --
# without it the app cannot start.
#
# This one fetch is retried: it is the only Store call in this script that a
# flake can retry safely (`sudo snap install --dangerous` above fails outright
# if the revision is already present, so that one must not be repeated). Skip
# when the provider is already installed, so the retry is idempotent.
provider_installed=false
for attempt in 1 2 3; do
  if snap list mesa-2404 >/dev/null 2>&1; then
    provider_installed=true
    break
  fi
  if sudo snap install mesa-2404; then
    provider_installed=true
    break
  fi
  echo "::warning::installing mesa-2404 failed (attempt $attempt/3)"
  [ "$attempt" -eq 3 ] || sleep 20
done
if [ "$provider_installed" != true ]; then
  echo "::error::could not install the mesa-2404 provider after 3 attempts"
  exit 1
fi
if ! snap connections media-sort | grep -q "mesa-2404:gpu-2404"; then
  sudo snap connect media-sort:gpu-2404 mesa-2404
fi
snap connections media-sort | grep gpu-2404

echo "Checking the payload inside the sandbox"
sudo snap run --shell media-sort <<'SNAP_SH'
set -euo pipefail

if ldd "$SNAP/bin/media-sort-gui" | grep -q "not found"; then
  echo "::error::the staged binary has unresolved libraries inside the snap:"
  ldd "$SNAP/bin/media-sort-gui" | grep "not found"
  exit 1
fi

# The provider supplies these libraries, so a copy left here means the gpu-2404
# cleanup did not run: the provider is their single source, and a leftover copy is
# exactly the driver mismatch that interface exists to prevent. Globs rather than
# absolute paths, so this checks the architecture's own triplet, and no `find` --
# a confined shell's `find` should not be what this check depends on.
leftovers=()
for path in \
  "$SNAP/usr/lib"/*/libEGL_mesa.so* \
  "$SNAP/usr/lib"/*/libGLX_mesa.so* \
  "$SNAP/usr/lib"/*/libva.so.2* \
  "$SNAP/usr/lib"/*/libgallium-*.so \
  "$SNAP/usr/lib"/*/libdrm_*.so* \
  "$SNAP/usr/lib"/*/dri \
  "$SNAP/usr/lib"/*/vdpau
do
  if [ -e "$path" ] || [ -L "$path" ]; then
    leftovers+=("$path")
  fi
done
if [ "${#leftovers[@]}" -gt 0 ]; then
  echo "::error::gpu-2404-cleanup left provider-supplied libraries behind:"
  printf '  %s\n' "${leftovers[@]}"
  exit 1
fi

# ffmpeg links libva/libvdpau, so it only runs with the provider's environment --
# the same wrapper the app's command-chain goes through.
echo "bundled ffmpeg: $("$SNAP/bin/gpu-2404-wrapper" "$SNAP/bin/ffmpeg" -version | head -1)"
SNAP_SH

# `snap run` is the path users take, so it should reach the dynamic loader
# without complaining. With no display the app cannot open its window and exits
# non-zero -- that is expected; only a loader failure is a problem.
output="$(timeout 60 snap run media-sort 2>&1 || true)"
printf '%s\n' "$output" | tail -5
if printf '%s' "$output" | grep -q "error while loading shared libraries"; then
  echo "::error::the app could not load its libraries through 'snap run'"
  exit 1
fi
# The wrapper bails out before exec when the provider is missing, which would
# otherwise look like "the app just exits" here.
if printf '%s' "$output" | grep -q "gpu-2404 interface isn't connected"; then
  echo "::error::the gpu-2404 interface is not connected, so the app refused to start"
  exit 1
fi
