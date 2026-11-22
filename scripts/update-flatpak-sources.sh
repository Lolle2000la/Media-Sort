#!/usr/bin/env bash
#
# Regenerate flatpak/cargo-sources.json from Cargo.lock.
#
# The Flathub build is offline, so flatpak/cargo-sources.json carries every
# crate the lock resolves to. It is a pure function of Cargo.lock, so it drifts
# whenever the lock does. This script regenerates it with a pinned copy of
# flatpak-builder-tools' generator; --check compares the committed file against
# a freshly generated one without ever writing to the tree.
#
# Exit codes (a caller needs to tell drift from a flake so it can retry):
#   0  success (with --check: the committed file is current)
#   1  --check only: flatpak/cargo-sources.json is stale
#   2  any other failure -- infrastructure (generator download, venv/pip, the
#      generator run) or bad usage
#
# Usage:
#   scripts/update-flatpak-sources.sh          # rewrite flatpak/cargo-sources.json
#   scripts/update-flatpak-sources.sh --check  # exit 1 if the committed file is stale
#
# CI regenerates the file before the Flatpak build (flatpak-build.yml), and
# scripts/release.sh regenerates and commits it, while release.yml's
# verify-version runs --check so a hand-cut tag cannot ship a stale file.
set -euo pipefail

# Pinned to a commit: the committed file is only meaningful if the generator
# that produced it is reproducible.
GENERATOR_REV="74697c75b630d7330e77250fc13cb5ea688d9479"
GENERATOR_URL="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/${GENERATOR_REV}/cargo/flatpak-cargo-generator.py"

# The generator's two dependencies, pinned by hand and deliberately not left
# unbounded: Renovate does not manage a throwaway venv's contents, so bump these
# here. Each is the newest release that is at least 14 days old (the repository's
# soak) and runs on Python 3.12.
AIOHTTP_VERSION="3.14.5"
TOMLKIT_VERSION="0.15.2"

EXIT_DRIFT=1
EXIT_INFRA=2

infra_err() { printf 'error: %s\n' "$*" >&2; exit "$EXIT_INFRA"; }

usage() {
  cat <<'EOF'
Regenerate flatpak/cargo-sources.json from Cargo.lock.

Usage:
  scripts/update-flatpak-sources.sh          # rewrite flatpak/cargo-sources.json
  scripts/update-flatpak-sources.sh --check  # exit 1 if the committed file is stale
  scripts/update-flatpak-sources.sh --help

The generator is pinned (flatpak-builder-tools) and fetched at run time. Its
dependencies (aiohttp, tomlkit) are pinned to fixed versions and installed into
a throwaway venv unless the interpreter named by $PYTHON (default python3)
already imports both.

Exit codes:
  0  success (with --check: the committed file is current)
  1  --check only: flatpak/cargo-sources.json is stale
  2  infrastructure failure (generator download, venv/pip, generator run) or bad usage
EOF
}

CHECK=0
case "${1:-}" in
  --check) CHECK=1 ;;
  -h|--help) usage; exit 0 ;;
  "") ;;
  *) printf 'error: unknown argument: %s (try --help)\n' "$1" >&2; exit "$EXIT_INFRA" ;;
esac
[ "$#" -le 1 ] || { printf 'error: too many arguments (try --help)\n' >&2; exit "$EXIT_INFRA"; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

LOCK_FILE="Cargo.lock"
OUT_FILE="flatpak/cargo-sources.json"

[ -f "$LOCK_FILE" ] || infra_err "$LOCK_FILE not found in $REPO_ROOT"

WORK_DIR="$(mktemp -d)" || infra_err "cannot create a temporary directory"
cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

GENERATOR="$WORK_DIR/flatpak-cargo-generator.py"
printf 'Downloading flatpak-cargo-generator.py @ %s\n' "$GENERATOR_REV" >&2
curl -fsSL -o "$GENERATOR" "$GENERATOR_URL" \
  || infra_err "failed to download the generator from $GENERATOR_URL"

# Reuse the ambient interpreter only when it already imports both dependencies
# (an escape hatch for a developer who has them); the default path is a pinned
# throwaway venv, so the generator runs against known versions.
PY="${PYTHON:-python3}"
if ! "$PY" -c 'import aiohttp, tomlkit' >/dev/null 2>&1; then
  VENV="$WORK_DIR/venv"
  "$PY" -m venv "$VENV" >/dev/null 2>&1 \
    || infra_err "$PY lacks aiohttp/tomlkit and cannot create a venv (is python3-venv installed?)"
  PY="$VENV/bin/python"
  "$VENV/bin/pip" install --quiet --disable-pip-version-check \
    "aiohttp==$AIOHTTP_VERSION" "tomlkit==$TOMLKIT_VERSION" \
    || infra_err "failed to install aiohttp==$AIOHTTP_VERSION and tomlkit==$TOMLKIT_VERSION"
fi

# The generator clones any git dependency named in Cargo.lock; restricting git
# to https stops a crafted ext::/ssh/file URL from running arbitrary commands.
export GIT_ALLOW_PROTOCOL=https

if [ "$CHECK" -eq 1 ]; then
  TMP_OUT="$WORK_DIR/cargo-sources.json"
  "$PY" "$GENERATOR" "$LOCK_FILE" -o "$TMP_OUT" \
    || infra_err "flatpak-cargo-generator.py failed on $LOCK_FILE"
  # Drift, not a flake: exit 1 so a caller can fail immediately instead of retrying.
  if ! diff -u "$OUT_FILE" "$TMP_OUT"; then
    printf 'error: %s is stale; run scripts/update-flatpak-sources.sh\n' "$OUT_FILE" >&2
    exit "$EXIT_DRIFT"
  fi
  printf '%s is current (generator %s).\n' "$OUT_FILE" "$GENERATOR_REV"
  exit 0
fi

mkdir -p "$(dirname "$OUT_FILE")"
"$PY" "$GENERATOR" "$LOCK_FILE" -o "$OUT_FILE" \
  || infra_err "flatpak-cargo-generator.py failed on $LOCK_FILE"
printf 'Wrote %s (generator %s).\n' "$OUT_FILE" "$GENERATOR_REV"
