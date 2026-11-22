#!/usr/bin/env bash
#
# Build the Flatpak vendored-crate tarball from Cargo.lock.
#
# Flathub builds offline, so the manifest needs every crate Cargo.lock resolves
# to plus a `cargo/config.toml` that points Cargo at them. This runs
# `cargo vendor --locked`, normalizes the printed source-replacement config to
# the path the manifest layout expects, and packs the result as a deterministic
# tarball whose root is `cargo/` (`cargo/config.toml` + `cargo/vendor/<crate>`). The
# manifest consumes it as a `type: archive` source with `strip-components: 0`,
# landing it at the module build root exactly where the old generated
# cargo-sources.json used to go.
#
# The tarball is attached to the GitHub release (release.yml), so the manifest
# references a released asset rather than a checked-in generated file; Flathub's
# external data checker bumps that URL/sha256 along with the app's tag.
#
# Usage:
#   scripts/build-cargo-vendor.sh [output-path]   # default: <repo>/cargo-vendor.tar.gz
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/cargo-vendor.tar.gz}"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null || die "cargo not found on PATH"
command -v sha256sum >/dev/null || die "sha256sum not found on PATH"
tar --version 2>/dev/null | grep -q 'GNU tar' \
  || die "GNU tar is required for a reproducible archive (use gtar on macOS)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/cargo"
# `cargo vendor` prints the source-replacement config naming the directory it was
# given; rewrite that line to the path the manifest layout uses (relative to the
# build root), so an absolute temp path never reaches the tarball.
cargo vendor --locked --manifest-path "$ROOT/Cargo.toml" "$WORK/cargo/vendor" \
  | sed 's|^directory = .*|directory = "cargo/vendor"|' > "$WORK/cargo/config.toml" \
  || die "cargo vendor failed"

[ -s "$WORK/cargo/config.toml" ] || die "cargo vendor printed no source-replacement config"
grep -q '^directory = "cargo/vendor"$' "$WORK/cargo/config.toml" \
  || die "could not normalize the vendor directory in cargo/config.toml"
{ [ -d "$WORK/cargo/vendor" ] && [ -n "$(ls -A "$WORK/cargo/vendor")" ]; } \
  || die "cargo vendor produced no crates"

mkdir -p "$(dirname "$OUT")"
# Deterministic archive: fixed entry order, mtime and ownership, and gzip with no
# name/timestamp. The root is `cargo/`, so consumers use `strip-components: 0`.
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  -C "$WORK" -cf - cargo | gzip -n -9 > "$OUT" \
  || die "failed to write $OUT"

[ -s "$OUT" ] || die "$OUT is empty"

printf 'Wrote %s (%s bytes)\n' "$OUT" "$(wc -c < "$OUT" | tr -d '[:space:]')"
printf 'sha256: %s\n' "$(sha256sum "$OUT" | cut -d' ' -f1)"
