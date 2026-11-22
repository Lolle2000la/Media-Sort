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

usage() {
  cat <<'EOF'
Regenerate flatpak/cargo-sources.json from Cargo.lock.

Usage:
  scripts/update-flatpak-sources.sh          # rewrite flatpak/cargo-sources.json
  scripts/update-flatpak-sources.sh --check  # exit 1 if the committed file is stale
  scripts/update-flatpak-sources.sh --help

The generator is pinned (flatpak-builder-tools) and fetched at run time. It
needs aiohttp and tomlkit; when the interpreter named by $PYTHON (default
python3) lacks them, a throwaway venv is built.
EOF
}

CHECK=0
case "${1:-}" in
  --check) CHECK=1 ;;
  -h|--help) usage; exit 0 ;;
  "") ;;
  *) printf 'error: unknown argument: %s (try --help)\n' "$1" >&2; exit 2 ;;
esac
[ "$#" -le 1 ] || { printf 'error: too many arguments (try --help)\n' >&2; exit 2; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

LOCK_FILE="Cargo.lock"
OUT_FILE="flatpak/cargo-sources.json"

[ -f "$LOCK_FILE" ] || { printf 'error: %s not found in %s\n' "$LOCK_FILE" "$REPO_ROOT" >&2; exit 1; }

WORK_DIR="$(mktemp -d)"
cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

GENERATOR="$WORK_DIR/flatpak-cargo-generator.py"
printf 'Downloading flatpak-cargo-generator.py @ %s\n' "$GENERATOR_REV" >&2
curl -fsSL -o "$GENERATOR" "$GENERATOR_URL"

# Reuse the ambient interpreter when it already has the generator's deps (CI
# installs them next to setup-python's python); otherwise build a throwaway venv.
PY="${PYTHON:-python3}"
if ! "$PY" -c 'import aiohttp, tomlkit' >/dev/null 2>&1; then
  if ! "$PY" -m venv "$WORK_DIR/venv" >/dev/null 2>&1; then
    printf 'error: %s lacks aiohttp/tomlkit and cannot create a venv (python3-venv missing?)\n' "$PY" >&2
    printf 'Install them (pip install aiohttp tomlkit) or point PYTHON at an interpreter that has them.\n' >&2
    exit 1
  fi
  PY="$WORK_DIR/venv/bin/python"
  "$PY" -m pip install --quiet --disable-pip-version-check aiohttp tomlkit
fi

# The generator clones any git dependency named in Cargo.lock; restricting git
# to https stops a crafted ext::/ssh/file URL from running arbitrary commands.
export GIT_ALLOW_PROTOCOL=https

if [ "$CHECK" -eq 1 ]; then
  TMP_OUT="$WORK_DIR/cargo-sources.json"
  "$PY" "$GENERATOR" "$LOCK_FILE" -o "$TMP_OUT"
  if ! diff -u "$OUT_FILE" "$TMP_OUT"; then
    printf 'error: %s is stale; run scripts/update-flatpak-sources.sh\n' "$OUT_FILE" >&2
    exit 1
  fi
  printf '%s is current (generator %s).\n' "$OUT_FILE" "$GENERATOR_REV"
  exit 0
fi

mkdir -p "$(dirname "$OUT_FILE")"
"$PY" "$GENERATOR" "$LOCK_FILE" -o "$OUT_FILE"
printf 'Wrote %s (generator %s).\n' "$OUT_FILE" "$GENERATOR_REV"
