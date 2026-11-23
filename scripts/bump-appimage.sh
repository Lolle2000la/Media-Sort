#!/usr/bin/env bash
# =============================================================================
# bump-appimage.sh -- find, report and apply upstream version bumps for the
# self-contained Linux AppImage payload, plus the dav1d pin the Windows test job
# in .github/workflows/ci.yml builds from.
#
# `packaging/appimage/versions.env` pins nine upstreams, and Renovate cannot
# maintain them: it can compute a new version, but not a new sha256 for an
# archive it never downloads. This script does that part. It only ever touches
# packaging/appimage/versions.env and the DAV1D_TAG/DAV1D_COMMIT pair in
# scripts/ci-pins.env -- comments, ordering and formatting are preserved -- and
# never writes a version whose archive it did not download and hash itself.
#
# What it covers, and where each answer comes from:
#
#   component          upstream query                               URL written
#   ------------------ -------------------------------------------- -------------
#   mpv                GitHub releases/latest                        tag archive
#   libass             GitHub releases/latest                        release asset
#   libplacebo         GitHub releases/latest                        tag archive
#   dav1d              GitHub tags?per_page=100 (the tag is the pin)  tag archive
#   zimg               GitHub tags?per_page=100 (the tag is the pin)  tag archive
#   wayland            GitLab repository/tags                        GitLab archive
#   wayland-protocols  GitLab repository/tags                        GitLab archive
#   ffmpeg             https://ffmpeg.org/releases/ index            upstream tarball
#   rust               channel-rust-stable.toml                      (no URL/sha256)
#
# `dav1d-ci` is the same upstream as `dav1d` but a pin of its own: the tag and
# commit in scripts/ci-pins.env, which .github/workflows/ci.yml's
# `Build dav1d (Windows)` step sources for the Windows test job. It appears in a
# run that selects `dav1d`, is resolved independently (the two pins legitimately
# sit on different releases) and is always written as a pair: the commit is the
# release tag's *peeled* commit, because the step fetches
# `archive/<commit>.tar.gz` and an archive URL for an annotated tag's own sha
# 404s. The pin sits outside `.github/workflows/` on purpose: a push that
# creates or modifies a file there needs the `workflows` permission, which
# `GITHUB_TOKEN` cannot hold, so a pin inline in ci.yml could never be bumped by
# the workflow that runs this script.
#
# GitHub goes through `gh api` when it is on PATH (the runners have it, already
# authenticated), otherwise `curl` + `jq` with `GITHUB_TOKEN` when that is set.
# GitLab and the FFmpeg index are plain `curl`; GitLab needs `jq`.
#
# Timestamps are parsed with GNU `date -d` (`gdate` is used when it is on PATH,
# which is how macOS gets there through coreutils); a BSD `date` is refused up
# front, because it would silently make every candidate look "too young" and
# report 0 updates.
#
# Soak period: every candidate carries a date and anything younger than
# --min-age-days (default 14, the same soak Renovate applies to this
# repository -- see AGENTS.md "Dependency updates (Renovate)") is skipped, so a
# malicious or broken publish gets the same two weeks to be caught before it can
# reach a pin. The date is `published_at` for a GitHub release, the tag commit's
# `committer.date` for a GitHub tag (the tags API carries no date of its own),
# `commit.committed_date` for a GitLab tag, the release tarball's
# `Last-Modified` for FFmpeg (ffmpeg.org's index lists no dates, and the file's
# mtime is its release/upload date), and the stable channel manifest's
# top-level `date` for Rust.
#
# Usage:
#   scripts/bump-appimage.sh [options]
#
#   (default)            dry run: print the report table, download nothing
#   --apply              rewrite packaging/appimage/versions.env in place
#   --check              print only whether updates are available; exit 2 when
#                        there are, 0 when there are none (never downloads)
#   --component NAME     limit the run to one component (repeatable):
#                        mpv ffmpeg dav1d libass zimg wayland
#                        wayland-protocols libplacebo rust (`dav1d` also
#                        covers the scripts/ci-pins.env pin)
#   --min-age-days N     soak period in days (0 disables it; default 14)
#   --allow-prerelease   also consider -rc/-beta/-alpha/-pre/dev tags
#   --json               machine-readable report instead of the table: exactly
#                        one JSON document on stdout, in report mode and with
#                        --check, and never the delta lines (diagnostics go to
#                        stderr; --check's verdict is the exit status)
#   --versions-file P    operate on P instead of packaging/appimage/versions.env
#                        (for testing this script, not for real bumps)
#   --ci-pins P          operate on P instead of scripts/ci-pins.env (for
#                        testing this script, not for real pins)
#   --help
#
# Exit status: 0 = nothing to do (or the apply succeeded), 1 = error (nothing
# was written), 2 = --check found updates.
#
# Deliberately NOT covered:
#
#   * the Flatpak manifest (flatpak/app.mediasort.MediaSort.yml), which pins the
#     same projects separately. It is a separate Flathub submission built
#     against a different runtime, with a different module set and tag+commit
#     pins instead of archives, so it has its own bumper:
#     scripts/bump-flatpak.py, run by .github/workflows/flatpak-bump.yml. The
#     two are separate PRs on purpose, so each is judged by its own build.
#   * the Rust dependencies, the Windows libmpv/ffmpeg bundle and the snap --
#     Renovate and their workflows own those.
#   * the container base image (`ubuntu:22.04`): that is the payload's glibc
#     floor, not a dependency.
#
# Manual escape hatch: this script is a convenience, not a gate. Nothing in the
# build trusts it -- packaging/appimage/build-in-container.sh only ever reads
# versions.env and verifies the sha256 it finds there. So when a bump needs a
# combination this script would not pick (FFmpeg must stay on the previous major
# because the new one breaks mpv, a component must be pinned to a release that
# never got a GitHub "latest" marker, ...), edit versions.env by hand exactly as
# before:
#
#   curl -fL <new-url> | sha256sum          # the value for *_SHA256
#
# and move on; the next scheduled run simply reports the newer tag as pending.
# =============================================================================
set -euo pipefail
export LC_ALL=C

SCRIPT_NAME=${0##*/}
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

VERSIONS_FILE="$REPO_ROOT/packaging/appimage/versions.env"
CI_PINS_FILE="$REPO_ROOT/scripts/ci-pins.env"
MODE=report
MIN_AGE_DAYS=14
ALLOW_PRERELEASE=0
JSON=0
SELECTED=()

API_TIMEOUT=${API_TIMEOUT:-60}
DOWNLOAD_TIMEOUT=${DOWNLOAD_TIMEOUT:-600}

COMPONENTS=(mpv ffmpeg dav1d libass zimg wayland wayland-protocols libplacebo rust)
# The ci.yml half of the dav1d pin. Not in COMPONENTS: it is not a versions.env
# entry and has no upstream query of its own -- it is selected by `dav1d`.
CI_PIN_COMPONENT=dav1d-ci
CI_PIN_REPO=videolan/dav1d

log() { printf '%s: %s\n' "$SCRIPT_NAME" "$*" >&2; }
warn() { printf '%s: WARNING: %s\n' "$SCRIPT_NAME" "$*" >&2; }
die() {
  printf '%s: ERROR: %s\n' "$SCRIPT_NAME" "$*" >&2
  exit 1
}

usage() {
  # The header block: everything from line 2 up to the closing `# ====` rule.
  sed -n '2,/^# ====/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

# --- arguments ---------------------------------------------------------------

add_component() {
  local name=$1 known=0 c
  for c in "${COMPONENTS[@]}"; do
    [ "$c" = "$name" ] && known=1
  done
  [ "$known" = 1 ] || die "unknown component '$name' (known: ${COMPONENTS[*]})"
  SELECTED+=("$name")
}

# is_selected <component> -- true when this run covers that component.
is_selected() {
  local want=$1 c
  for c in "${SELECTED[@]}"; do
    [ "$c" = "$want" ] && return 0
  done
  return 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --apply) MODE=apply ;;
    --check) MODE=check ;;
    --json) JSON=1 ;;
    --allow-prerelease) ALLOW_PRERELEASE=1 ;;
    --component)
      [ $# -ge 2 ] || die "--component needs a value"
      add_component "$2"
      shift
      ;;
    --component=*) add_component "${1#*=}" ;;
    --min-age-days)
      [ $# -ge 2 ] || die "--min-age-days needs a value"
      MIN_AGE_DAYS=$2
      shift
      ;;
    --min-age-days=*) MIN_AGE_DAYS=${1#*=} ;;
    --versions-file)
      [ $# -ge 2 ] || die "--versions-file needs a value"
      VERSIONS_FILE=$2
      shift
      ;;
    --versions-file=*) VERSIONS_FILE=${1#*=} ;;
    --ci-pins)
      [ $# -ge 2 ] || die "--ci-pins needs a value"
      CI_PINS_FILE=$2
      shift
      ;;
    --ci-pins=*) CI_PINS_FILE=${1#*=} ;;
    --help | -h) usage 0 ;;
    *) die "unknown argument '$1' (try --help)" ;;
  esac
  shift
done

case "$MIN_AGE_DAYS" in
  '' | *[!0-9]*) die "--min-age-days must be a non-negative integer, got '$MIN_AGE_DAYS'" ;;
esac

[ -f "$VERSIONS_FILE" ] || die "no such versions file: $VERSIONS_FILE"
if [ ${#SELECTED[@]} -eq 0 ]; then
  SELECTED=("${COMPONENTS[@]}")
fi

HAVE_GH=0
command -v gh >/dev/null 2>&1 && HAVE_GH=1

# Upstream timestamps arrive as ISO-8601, or as RFC-1123 for the FFmpeg
# tarball's Last-Modified, and only GNU date parses those with `date -d`.
# macOS/BSD date fails on every one of them -- and the old code swallowed that
# (`date -u -d ... || return 1` in age_ok), so every candidate looked "too
# young" and the run reported 0 updates with exit 0: a silent no-op on exactly
# the platform a human would run it on. Prove the capability once, up front, and
# fail loudly instead. (`gdate` is coreutils' name on macOS.)
DATE_CMD=$(command -v gdate || command -v date || true)
if [ -z "$DATE_CMD" ] ||
  ! "$DATE_CMD" -u -d '1970-01-01T00:00:00Z' +%s >/dev/null 2>&1; then
  die "GNU date required: upstream timestamps are parsed with 'date -d', which the date on this system does not support (on macOS install coreutils so 'gdate' is on PATH; on Linux this should never happen)"
fi

TAB=$(printf '\t')

# --- component metadata ------------------------------------------------------
#
# Each component is a <bash prefix> in versions.env plus a query kind. The URL
# shapes below mirror versions.env exactly, and check_url_templates proves that
# on every run before any network call: an upstream that changed its archive
# layout would otherwise be pinned to a URL that 404s during a release build.

prefix_of() {
  case "$1" in
    mpv) printf 'MPV\n' ;;
    ffmpeg) printf 'FFMPEG\n' ;;
    dav1d) printf 'DAV1D\n' ;;
    libass) printf 'LIBASS\n' ;;
    zimg) printf 'ZIMG\n' ;;
    wayland) printf 'WAYLAND\n' ;;
    wayland-protocols) printf 'WAYLAND_PROTOCOLS\n' ;;
    libplacebo) printf 'LIBPLACEBO\n' ;;
    rust) printf 'RUST\n' ;;
    *) return 1 ;;
  esac
}

kind_of() {
  case "$1" in
    mpv | libass | libplacebo) printf 'gh-release\n' ;;
    dav1d | zimg) printf 'gh-tag\n' ;;
    wayland | wayland-protocols) printf 'gitlab-tag\n' ;;
    ffmpeg) printf 'ffmpeg\n' ;;
    rust) printf 'rust\n' ;;
    *) return 1 ;;
  esac
}

repo_of() {
  case "$1" in
    mpv) printf 'mpv-player/mpv\n' ;;
    libass) printf 'libass/libass\n' ;;
    libplacebo) printf 'haasn/libplacebo\n' ;;
    dav1d) printf 'videolan/dav1d\n' ;;
    zimg) printf 'sekrit-twc/zimg\n' ;;
    *) return 1 ;;
  esac
}

gitlab_project_of() {
  case "$1" in
    wayland) printf 'wayland/wayland\n' ;;
    wayland-protocols) printf 'wayland/wayland-protocols\n' ;;
    *) return 1 ;;
  esac
}

# url_of <component> <version> -- the exact download URL versions.env carries
# for that version.
url_of() {
  local c=$1 v=$2
  case "$c" in
    mpv | dav1d | zimg | libplacebo)
      printf 'https://github.com/%s/archive/refs/tags/%s.tar.gz\n' "$(repo_of "$c")" "$v"
      ;;
    libass) printf 'https://github.com/libass/libass/releases/download/%s/libass-%s.tar.xz\n' "$v" "$v" ;;
    wayland)
      printf 'https://gitlab.freedesktop.org/wayland/wayland/-/archive/%s/wayland-%s.tar.gz\n' "$v" "$v"
      ;;
    wayland-protocols)
      printf 'https://gitlab.freedesktop.org/wayland/wayland-protocols/-/archive/%s/wayland-protocols-%s.tar.gz\n' "$v" "$v"
      ;;
    ffmpeg) printf 'https://ffmpeg.org/releases/ffmpeg-%s.tar.xz\n' "$v" ;;
    rust) printf '\n' ;;
    *) return 1 ;;
  esac
}

# --- versions.env access -----------------------------------------------------

# env_get <VAR> -- the value of VAR (the last definition wins, like sourcing
# would), quotes stripped. Fails when the variable is absent.
env_get() {
  local line
  line=$(sed -n "s/^$1=//p" "$VERSIONS_FILE" | tail -n 1)
  [ -n "$line" ] || return 1
  line=${line%\"}
  line=${line#\"}
  line=${line%\'}
  line=${line#\'}
  printf '%s\n' "$line"
}

env_has() {
  grep -q "^$1=" "$VERSIONS_FILE"
}

# --- ci-pins.env access -------------------------------------------------------
#
# The two pin lines are a plain env file, one `VAR=value` per line, sourced by
# .github/workflows/ci.yml's `Build dav1d (Windows)` step. `value` ends at the
# first blank, exactly as sourcing it does, so a human's trailing note
# (`DAV1D_TAG=1.5.5   # first tag that survives MSVC 14.52`) is text the rewrite
# carries through rather than part of the pin.

# ci_pin_get <file> <VAR> -- the value of one pin line. Exactly one line must
# carry it: a renamed variable would make this script silently stop bumping the
# pin, which is the failure this whole mechanism exists to prevent, so it is
# loud instead.
ci_pin_get() {
  local file=$1 var=$2 count value
  count=$(grep -cE "^[[:space:]]*$var=" "$file" || true)
  [ "$count" = 1 ] ||
    die "$file carries $count $var= line(s), expected exactly one; the dav1d pin has been renamed or moved"
  value=$(sed -n -E "s/^[[:space:]]*$var=//p" "$file")
  value=${value%%[[:space:]]*}
  value=${value%\"}
  value=${value#\"}
  value=${value%\'}
  value=${value#\'}
  [ -n "$value" ] || die "$file has an empty $var="
  printf '%s\n' "$value"
}

# is_full_sha <value> -- true for a lowercase 40-hex git object name.
is_full_sha() {
  case "$1" in
    *[!0-9a-f]*) return 1 ;;
  esac
  [ ${#1} -eq 40 ]
}

# --- version arithmetic ------------------------------------------------------

# The comparable core of a tag: no `v`/`release-` prefix, no `-rc1` suffix.
version_core() {
  local v=$1
  v=${v#v}
  v=${v#release-}
  printf '%s\n' "$v" | sed -E 's/^[^0-9]*//; s/[^0-9.].*$//'
}

# version_gt <a> <b> -- true when a is strictly newer than b. `sort -V` orders
# 1.5.4 before 1.5.10, which a plain string compare gets backwards.
version_gt() {
  local a b
  a=$(version_core "$1")
  b=$(version_core "$2")
  [ -n "$a" ] && [ -n "$b" ] || return 1
  [ "$a" != "$b" ] || return 1
  [ "$(printf '%s\n%s\n' "$a" "$b" | sort -V | tail -n 1)" = "$a" ]
}

sort_candidates() {
  sort -t "$TAB" -k1,1 -Vr
}

# is_version_tag <tag> -- false for tags that carry no version at all (`main`,
# `latest`, `nightly`). Those sort above every release under `sort -V`, so a
# resolver must skip them instead of stopping at them: stopping reported `keep`
# for a component that had a genuine newer release.
is_version_tag() {
  [ -n "$(version_core "$1")" ]
}

# is_prerelease <component> <tag>
is_prerelease() {
  local component=$1 version=$2 micro
  [ "$ALLOW_PRERELEASE" = 1 ] && return 1
  case "$version" in
    *-rc* | *-RC* | *-beta* | *-alpha* | *-pre* | *-dev* | *-snapshot* | *_rc* | *_beta* | *_alpha*)
      return 0
      ;;
  esac
  # wayland/wayland-protocols publish development snapshots as odd micro
  # releases (1.25.91, 1.26.90): a third component >= 90 is never a stable
  # release.
  case "$component" in
    wayland | wayland-protocols)
      micro=$(version_core "$version" | awk -F. '{ print $3 + 0 }')
      [ "${micro:-0}" -ge 90 ] && return 0
      ;;
  esac
  return 1
}

# age_ok <ISO date> -- true when the artifact is at least MIN_AGE_DAYS old.
age_ok() {
  local published=$1 epoch now
  [ "$MIN_AGE_DAYS" -eq 0 ] && return 0
  [ -n "$published" ] || return 1
  epoch=$("$DATE_CMD" -u -d "$published" +%s 2>/dev/null) || return 1
  now=$("$DATE_CMD" -u +%s)
  [ $((now - epoch)) -ge $((MIN_AGE_DAYS * 86400)) ]
}

# --- HTTP --------------------------------------------------------------------

api_auth=()
if [ -n "${GITHUB_TOKEN:-}" ]; then
  api_auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
fi

# api_get <path> <jq filter> -- GitHub REST through gh when possible, curl+jq
# otherwise (authenticated when GITHUB_TOKEN is set). Returns non-zero when the
# answer could not be had, which the caller reports as `unknown`; it only ever
# dies for a jq that the run genuinely cannot do without.
api_get() {
  local path=$1 filter=$2 out
  if [ "$HAVE_GH" = 1 ]; then
    if out=$(gh api "$path" --jq "$filter" 2>/dev/null); then
      printf '%s' "$out"
      return 0
    fi
    # gh is there but this one call failed. The curl fallback needs jq; without
    # it the honest answer is "unknown" for this component, so return instead of
    # dying -- one flaky upstream call must not turn the weekly run red with a
    # message about a jq this run does not actually need.
    command -v jq >/dev/null 2>&1 || return 1
  else
    command -v jq >/dev/null 2>&1 || die "jq is required to query the GitHub API without gh"
  fi
  if out=$(curl -fsSL --retry 3 --max-time "$API_TIMEOUT" \
    -H 'Accept: application/vnd.github+json' \
    ${api_auth[@]+"${api_auth[@]}"} \
    "https://api.github.com/$path" 2>/dev/null | jq -r "$filter" 2>/dev/null); then
    printf '%s' "$out"
    return 0
  fi
  return 1
}

url_encode_path() {
  printf '%s' "$1" | sed 's#/#%2F#g'
}

# gitlab_get <project> <suffix> <jq filter> -- GitLab REST, project path
# URL-encoded (`wayland/wayland` -> `wayland%2Fwayland`).
gitlab_get() {
  local project=$1 suffix=$2 filter=$3
  command -v jq >/dev/null 2>&1 || die "jq is required to query the GitLab API"
  curl -fsSL --retry 3 --max-time "$API_TIMEOUT" \
    "https://gitlab.freedesktop.org/api/v4/projects/$(url_encode_path "$project")/$suffix" |
    jq -r "$filter"
}

# http_last_modified <url> -- the URL's Last-Modified as an ISO timestamp.
http_last_modified() {
  local lm
  lm=$(curl -fsIL --retry 3 --max-time "$API_TIMEOUT" "$1" 2>/dev/null |
    sed -n 's/^[Ll]ast-[Mm]odified: *//p' | tail -n 1 | tr -d '\r')
  [ -n "$lm" ] || return 1
  "$DATE_CMD" -u -d "$lm" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null
}

# --- candidate resolution ----------------------------------------------------
#
# Every resolver prints "version<TAB>date[<TAB>commit]" for the newest
# *eligible* tag: not a prerelease, newer than the pin, and old enough for the
# soak. It walks down from the newest so a too-fresh release costs one extra
# query instead of a full scan, prints nothing when no candidate is newer than
# the pin (the pin is then already the latest eligible version), and returns
# non-zero when the upstream could not be queried at all -- which is reported as
# `unknown`, never as `keep`.

gh_release_eligible() { # <component> <pinned>
  local component=$1 pinned=$2 repo latest version date list
  repo=$(repo_of "$component") || return 1
  # releases/latest is the cheap question ("is anything newer at all?"); when
  # that release is still inside the soak, the full list is needed to find the
  # newest one that is not.
  if latest=$(api_get "repos/$repo/releases/latest" '[.tag_name, .published_at] | @tsv'); then
    version=$(printf '%s' "$latest" | cut -f1)
    date=$(printf '%s' "$latest" | cut -f2)
    if [ -n "$version" ] && ! is_prerelease "$component" "$version" &&
      version_gt "$version" "$pinned" && age_ok "$date"; then
      printf '%s\t%s\n' "$version" "$date"
      return 0
    fi
  fi
  list=$(api_get "repos/$repo/releases?per_page=30" \
    '.[] | select(.draft | not) | select(.prerelease | not) | [.tag_name, .published_at] | @tsv') || return 1
  # The list arrives in API order (newest *created*, not newest version), so a
  # backport published after a release would be proposed first. Sort by version,
  # exactly like the tag resolvers below.
  while IFS="$TAB" read -r version date; do
    [ -n "$version" ] || continue
    is_prerelease "$component" "$version" && continue
    is_version_tag "$version" || continue
    version_gt "$version" "$pinned" || continue
    age_ok "$date" || continue
    printf '%s\t%s\n' "$version" "$date"
    return 0
  done <<<"$(printf '%s\n' "$list" | sort_candidates)"
}

gh_tag_eligible() { # <component> <pinned>
  local component=$1 pinned=$2 repo tags name sha date found=""
  repo=$(repo_of "$component") || return 1
  tags=$(api_get "repos/$repo/tags?per_page=100" '.[] | [.name, .commit.sha] | @tsv') || return 1
  # Newest version first; stop at the pin (nothing below it can be an update).
  while IFS="$TAB" read -r name sha; do
    [ -n "$name" ] || continue
    is_prerelease "$component" "$name" && continue
    # A non-version tag (`main`, `nightly`) is skipped, not treated as the end
    # of the scan: only the pin itself or something older ends it.
    is_version_tag "$name" || continue
    version_gt "$name" "$pinned" || break
    date=$(api_get "repos/$repo/commits/$sha" '.commit.committer.date') || return 1
    age_ok "$date" || continue
    found="$name$TAB$date$TAB$sha"
    break
  done <<<"$(printf '%s\n' "$tags" | sort_candidates)"
  [ -z "$found" ] || printf '%s\n' "$found"
}

gitlab_tag_eligible() { # <component> <pinned>
  local component=$1 pinned=$2 project tags name date sha found=""
  project=$(gitlab_project_of "$component") || return 1
  tags=$(gitlab_get "$project" 'repository/tags?per_page=100' \
    '.[] | [.name, .commit.committed_date, .commit.id] | @tsv') || return 1
  while IFS="$TAB" read -r name date sha; do
    [ -n "$name" ] || continue
    is_prerelease "$component" "$name" && continue
    # Same rule as gh_tag_eligible: a non-version tag above the pin is skipped.
    is_version_tag "$name" || continue
    version_gt "$name" "$pinned" || break
    age_ok "$date" || continue
    found="$name$TAB$date$TAB$sha"
    break
  done <<<"$(printf '%s\n' "$tags" | sort_candidates)"
  [ -z "$found" ] || printf '%s\n' "$found"
}

ffmpeg_eligible() { # <component> <pinned>
  local pinned=$2 versions version date
  # `-rc*` and snapshot files never match this pattern: only bare X.Y[.Z]
  # release tarballs are considered.
  versions=$(curl -fsSL --retry 3 --max-time "$API_TIMEOUT" https://ffmpeg.org/releases/ |
    grep -o 'ffmpeg-[0-9][0-9.]*\.tar\.xz' |
    sed 's/^ffmpeg-//; s/\.tar\.xz$//' | sort -u | sort -Vr) || return 1
  [ -n "$versions" ] || return 1
  while IFS= read -r version; do
    [ -n "$version" ] || continue
    version_gt "$version" "$pinned" || break
    date=$(http_last_modified "https://ffmpeg.org/releases/ffmpeg-$version.tar.xz") || continue
    age_ok "$date" || continue
    printf '%s\t%s\n' "$version" "$date"
    return 0
  done <<<"$versions"
}

rust_eligible() { # <component> <pinned>
  local pinned=$2 toml version date
  toml=$(curl -fsSL --retry 3 --max-time "$API_TIMEOUT" \
    https://static.rust-lang.org/dist/channel-rust-stable.toml) || return 1
  # `head -n 1` closes the pipe early, and the resulting SIGPIPE surfaces as
  # "printf: write error: Broken pipe" in the caller's captured output (it
  # appeared in the first bump PR's body). sed prints every match cheaply, so
  # take the first line without breaking the upstream writer.
  date=$(printf '%s\n' "$toml" | sed -n 's/^date = "\(.*\)"$/\1/p' | sed -n '1p')
  # No `exit` inside awk: it closes the pipe while printf is still writing,
  # and the race surfaces as "printf: write error: Broken pipe" (the TOML is
  # small enough that it usually wins, which is why it only showed up in CI).
  # Read to EOF and print the first match instead.
  version=$(printf '%s\n' "$toml" |
    awk '/^\[pkg\.rust\]/ { in_rust = 1; next }
         /^\[/ { in_rust = 0 }
         in_rust && /^version = "/ && !seen { print; seen = 1 }' |
    sed -n 's/^version = "\([^ "]*\).*$/\1/p')
  [ -n "$version" ] && [ -n "$date" ] || return 1
  version_gt "$version" "$pinned" || return 0
  age_ok "$date" || return 0
  printf '%s\t%s\n' "$version" "$date"
}

resolve_eligible() { # <component> <pinned>
  case "$(kind_of "$1")" in
    gh-release) gh_release_eligible "$1" "$2" ;;
    gh-tag) gh_tag_eligible "$1" "$2" ;;
    gitlab-tag) gitlab_tag_eligible "$1" "$2" ;;
    ffmpeg) ffmpeg_eligible "$1" "$2" ;;
    rust) rust_eligible "$1" "$2" ;;
    *) return 1 ;;
  esac
}

# commit_for <component> <version> <commit-from-the-candidate-or-empty> -- the
# commit a component with a *_COMMIT variable records next to the tag. GitLab
# candidates carry their commit; a GitHub tag is dereferenced (which also
# resolves an annotated tag to the commit it points at).
commit_for() {
  local component=$1 version=$2 known=${3:-}
  if [ -n "$known" ]; then
    printf '%s\n' "$known"
    return 0
  fi
  case "$(kind_of "$component")" in
    gh-release | gh-tag) api_get "repos/$(repo_of "$component")/commits/$version" '.sha' ;;
    gitlab-tag)
      gitlab_get "$(gitlab_project_of "$component")" \
        "repository/tags/$(url_encode_path "$version")" '.commit.id'
      ;;
    *) return 1 ;;
  esac
}

# peeled_commit <repo> <tag> -- the commit a release tag points at, never the tag
# object. This is what a *commit* archive URL needs: ci.yml fetches
# `github.com/<repo>/archive/<sha>.tar.gz`, and an archive URL for an annotated
# tag object's own sha is a 404 (the tag archive is spelled differently). The
# REST tags endpoint is not a substitute -- it answers with the tag's commit
# only because GitHub dereferences server-side -- so ask git directly: the
# `^{}` line of `git ls-remote --tags` is the peeled commit, and its absence
# means the tag is lightweight and its own sha is the commit. Needs no clone.
peeled_commit() {
  local repo=$1 tag=$2 refs sha ref peeled="" plain=""
  refs=$(git ls-remote --tags "https://github.com/$repo" \
    "refs/tags/$tag" "refs/tags/$tag^{}" 2>/dev/null) || return 1
  while IFS="$TAB" read -r sha ref; do
    [ -n "$sha" ] || continue
    case "$ref" in
      *'^{}') peeled=$sha ;;
      "refs/tags/$tag") plain=$sha ;;
    esac
  done <<<"$refs"
  [ -n "$peeled" ] || peeled=$plain
  [ -n "$peeled" ] || return 1
  printf '%s\n' "$peeled"
}

# --- query-time guard --------------------------------------------------------

# The URL this script would write for the *pinned* version must already be the
# one versions.env carries; otherwise a bump would silently repoint the pin at a
# different archive layout. Pure -- no network, runs before anything is fetched.
check_url_templates() {
  local component prefix pinned pinned_url generated
  for component in "${SELECTED[@]}"; do
    prefix=$(prefix_of "$component") || continue
    env_has "${prefix}_URL" || continue
    pinned=$(env_get "${prefix}_VERSION") || die "$VERSIONS_FILE has no ${prefix}_VERSION"
    pinned_url=$(env_get "${prefix}_URL") || die "$VERSIONS_FILE has no ${prefix}_URL"
    generated=$(url_of "$component" "$pinned")
    [ "$generated" = "$pinned_url" ] ||
      die "the URL template for $component does not reproduce ${prefix}_URL:
  versions.env: $pinned_url
  script:       $generated
Fix url_of() in $SCRIPT_NAME before trusting a bump."
  done
}

# --- main --------------------------------------------------------------------
#
# Phase 1 resolves every selected component and touches nothing. Phase 2 (apply
# only) downloads and hashes the archives. versions.env is rewritten only after
# both phases succeeded, into a temp file in the same directory, from a single
# mv -- so any failure leaves the file byte-identical.

WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/bump-appimage.XXXXXX")
tmp_file=""
ci_tmp_file=""
trap 'rm -rf "$WORK_DIR"; [ -z "$tmp_file" ] || rm -f "$tmp_file"; [ -z "$ci_tmp_file" ] || rm -f "$ci_tmp_file"' EXIT

check_url_templates

declare -a order=() pinned_list=() latest_list=() action_list=() status_list=() commit_list=()
declare -a new_url_list=() new_sha_list=() new_commit_list=()
updates=0
unknowns=0
ci_pinned=""
ci_latest=""
ci_action=""

for component in "${SELECTED[@]}"; do
  prefix=$(prefix_of "$component") || die "no versions.env prefix for $component"
  pinned=$(env_get "${prefix}_VERSION") || die "$VERSIONS_FILE has no ${prefix}_VERSION"

  latest=""
  candidate_commit=""
  status=ok
  if ! eligible=$(resolve_eligible "$component" "$pinned"); then
    status=unknown
  elif [ -n "$eligible" ]; then
    latest=$(printf '%s' "$eligible" | cut -f1)
    candidate_commit=$(printf '%s' "$eligible" | cut -f3)
  fi

  if [ "$status" = unknown ]; then
    action=unknown
    latest_display=unknown
    unknowns=$((unknowns + 1))
  elif [ -z "$latest" ]; then
    action=keep
    latest_display=$pinned
  else
    action=update
    latest_display=$latest
    updates=$((updates + 1))
  fi

  order+=("$component")
  pinned_list+=("$pinned")
  latest_list+=("$latest_display")
  action_list+=("$action")
  status_list+=("$status")
  commit_list+=("$candidate_commit")
  new_url_list+=("")
  new_sha_list+=("")
  new_commit_list+=("")
done

# --- the ci-pins.env dav1d pin -----------------------------------------------
#
# .github/workflows/ci.yml's `Build dav1d (Windows)` step builds dav1d for the
# Windows test job itself, from a *commit* archive
# (`archive/$DAV1D_COMMIT.tar.gz`), because dav1d-sys' build script swallows its
# own clone/compile failures there; the job reads both values from
# scripts/ci-pins.env and asserts the unpacked meson.build says
# `version: '<DAV1D_TAG>'`. Two consequences make this its own row rather than a
# versions.env component:
#
#   * the commit must be the release tag's *peeled* commit -- an archive URL for
#     an annotated tag object's own sha is a 404 -- so it is resolved with
#     `git ls-remote` (peeled_commit) right before writing, never taken from the
#     tags API;
#   * the tag and the commit have to move together, or the job fails on its own
#     assertion: both values are rewritten in one pass over one file.
#
# The pin lives in a plain file outside `.github/workflows/` because a push that
# touches a workflow file needs the `workflows` permission, which the bump
# workflow's `GITHUB_TOKEN` can never hold -- inline in ci.yml it could not be
# moved at all.
#
# It is deliberately independent of versions.env's DAV1D_VERSION (the Windows
# job needs a release whose stdatomic.h survives the MSVC toolset -- the floor is
# recorded in the pin file, next to the value -- while the AppImage may sit
# elsewhere), so it is resolved against its own pin -- forward only, so a pin
# ahead of the newest soaked release is kept, not dragged back.
if is_selected dav1d; then
  [ -f "$CI_PINS_FILE" ] || die "no such pin file: $CI_PINS_FILE"
  ci_pinned=$(ci_pin_get "$CI_PINS_FILE" DAV1D_TAG)
  ci_pinned_commit=$(ci_pin_get "$CI_PINS_FILE" DAV1D_COMMIT)
  # A commit that is not a full sha cannot be fetched, which means every bump
  # built on it is broken: fail before querying anything else.
  is_full_sha "$ci_pinned_commit" ||
    die "$CI_PINS_FILE: DAV1D_COMMIT is not a full 40-hex commit sha ('$ci_pinned_commit')"

  ci_latest=""
  ci_status=ok
  if ! ci_eligible=$(resolve_eligible dav1d "$ci_pinned"); then
    ci_status=unknown
  elif [ -n "$ci_eligible" ]; then
    ci_latest=$(printf '%s' "$ci_eligible" | cut -f1)
  fi

  if [ "$ci_status" = unknown ]; then
    ci_action=unknown
    ci_display=unknown
    unknowns=$((unknowns + 1))
  elif [ -z "$ci_latest" ]; then
    ci_action=keep
    ci_display=$ci_pinned
  else
    ci_action=update
    ci_display=$ci_latest
    updates=$((updates + 1))
  fi

  order+=("$CI_PIN_COMPONENT")
  pinned_list+=("$ci_pinned")
  latest_list+=("$ci_display")
  action_list+=("$ci_action")
  status_list+=("$ci_status")
  commit_list+=("")
  new_url_list+=("")
  new_sha_list+=("")
  new_commit_list+=("")
fi

print_report() {
  local i
  printf '%-18s %-14s %-16s %-10s %s\n' component pinned 'latest eligible' status action
  printf '%-18s %-14s %-16s %-10s %s\n' ------------------ -------------- ---------------- ---------- ------
  for i in "${!order[@]}"; do
    printf '%-18s %-14s %-16s %-10s %s\n' \
      "${order[$i]}" "${pinned_list[$i]}" "${latest_list[$i]}" \
      "${status_list[$i]}" "${action_list[$i]}"
  done
}

print_json() {
  local i sep=""
  printf '['
  for i in "${!order[@]}"; do
    printf '%s{"component":"%s","pinned":"%s","latest":"%s","action":"%s","status":"%s"}' \
      "$sep" "${order[$i]}" "${pinned_list[$i]}" "${latest_list[$i]}" \
      "${action_list[$i]}" "${status_list[$i]}"
    sep=","
  done
  printf ']\n'
}

# The machine-friendly delta lines (what --apply would write), one per update.
print_deltas() {
  local i
  for i in "${!order[@]}"; do
    if [ "${action_list[$i]}" = update ]; then
      printf '%s %s -> %s\n' "${order[$i]}" "${pinned_list[$i]}" "${latest_list[$i]}"
    fi
  done
}

if [ "$MODE" = check ]; then
  # No table and no downloads: whether anything is pending is the whole answer,
  # and the exit status carries it. --json still emits exactly one JSON
  # document and nothing else on stdout.
  if [ "$JSON" = 1 ]; then
    print_json
  else
    print_deltas
    printf 'updates available: %d\n' "$updates"
  fi
  if [ "$unknowns" -gt 0 ]; then
    die "$unknowns component(s) could not be queried; the answer is incomplete"
  fi
  [ "$updates" -gt 0 ] && exit 2
  exit 0
fi

# Machine-readable means machine-readable: --json prints exactly one JSON
# document and never the delta/table/diagnostic lines around it, so the output
# can be piped straight into jq.
if [ "$JSON" = 1 ]; then
  print_json
else
  print_report
fi

if [ "$MODE" = report ]; then
  if [ "$JSON" != 1 ]; then
    print_deltas
  fi
  exit 0
fi

# --- apply -------------------------------------------------------------------

if [ "$unknowns" -gt 0 ]; then
  die "$unknowns component(s) could not be queried; refusing to write $VERSIONS_FILE (narrow the run with --component if that upstream is down)"
fi

for i in "${!order[@]}"; do
  [ "${action_list[$i]}" = update ] || continue
  # The ci.yml row is written by its own block below: it has no versions.env
  # prefix, and it needs a peeled commit instead of an archive hash.
  [ "${order[$i]}" = "$CI_PIN_COMPONENT" ] && continue
  component=${order[$i]}
  prefix=$(prefix_of "$component")
  latest=${latest_list[$i]}
  url=$(url_of "$component" "$latest")

  # Paired commit, only where the file already tracks one.
  if env_has "${prefix}_COMMIT"; then
    if ! new_commit=$(commit_for "$component" "$latest" "${commit_list[$i]}") || [ -z "$new_commit" ]; then
      die "could not resolve the commit for $component $latest; $VERSIONS_FILE was left untouched"
    fi
  else
    new_commit=""
  fi

  if [ -z "$url" ]; then
    # rust: a version, no URL and no sha256 to compute.
    new_sha=""
  else
    archive="$WORK_DIR/$(basename "$url")"
    if ! curl -fL --retry 3 --max-time "$DOWNLOAD_TIMEOUT" -o "$archive" "$url" 2>/dev/null; then
      die "could not download $url; $VERSIONS_FILE was left untouched"
    fi
    if [ ! -s "$archive" ]; then
      die "downloaded an empty archive from $url; $VERSIONS_FILE was left untouched"
    fi
    new_sha=$(sha256sum "$archive" | cut -d' ' -f1)
    rm -f "$archive"
    case "$new_sha" in
      [0-9a-f][0-9a-f]*) ;;
      *) die "sha256sum produced '$new_sha' for $url; $VERSIONS_FILE was left untouched" ;;
    esac
  fi

  new_url_list[i]=$url
  new_sha_list[i]=$new_sha
  new_commit_list[i]=$new_commit
done

if [ "$updates" -eq 0 ]; then
  log "nothing to apply; every pin in ${VERSIONS_FILE#"$REPO_ROOT"/} and ${CI_PINS_FILE#"$REPO_ROOT"/} is already current"
  exit 0
fi

ci_new_commit=""
if [ "$ci_action" = update ]; then
  # The peeled commit, as the *very* last network question of the run: if it
  # cannot be had, nothing below this point has rewritten a file yet.
  if ! ci_new_commit=$(peeled_commit "$CI_PIN_REPO" "$ci_latest") || [ -z "$ci_new_commit" ]; then
    die "could not resolve the peeled commit of dav1d $ci_latest ('git ls-remote --tags $CI_PIN_REPO'); nothing was written"
  fi
  is_full_sha "$ci_new_commit" ||
    die "git ls-remote returned '$ci_new_commit' for dav1d $ci_latest, which is not a commit sha; nothing was written"

  ci_tmp_file=$(mktemp "${CI_PINS_FILE}.XXXXXX")
  # Only the two values are replaced. Everything after a value on the same line
  # -- a trailing comment, its spacing -- is carried through untouched, so a
  # hand note on a pin line cannot vanish in a bump (the mask check below proves
  # it for the whole file).
  while IFS= read -r line || [ -n "$line" ]; do
    if [[ $line =~ ^([[:space:]]*DAV1D_TAG=)[^[:space:]]*(.*)$ ]]; then
      printf '%s%s%s\n' "${BASH_REMATCH[1]}" "$ci_latest" "${BASH_REMATCH[2]}"
    elif [[ $line =~ ^([[:space:]]*DAV1D_COMMIT=)[^[:space:]]*(.*)$ ]]; then
      printf '%s%s%s\n' "${BASH_REMATCH[1]}" "$ci_new_commit" "${BASH_REMATCH[2]}"
    else
      printf '%s\n' "$line"
    fi
  done <"$CI_PINS_FILE" >"$ci_tmp_file"

  # mask_pins <file> -- the file with both values blanked out. Comparing the
  # masked old and new file is the strongest statement available: same lines,
  # same order, same comments, same spacing, only the two values differ, so a
  # dropped line, a lost trailing comment or a re-indented pin cannot pass.
  mask_pins() { sed -E 's/^([[:space:]]*DAV1D_(TAG|COMMIT)=)[^[:space:]]*/\1<pin>/' "$1"; }
  [ "$(mask_pins "$CI_PINS_FILE")" = "$(mask_pins "$ci_tmp_file")" ] ||
    die "the ci-pins.env rewrite changed more than the two values; $CI_PINS_FILE was left untouched"
  [ "$(ci_pin_get "$ci_tmp_file" DAV1D_TAG)" = "$ci_latest" ] ||
    die "the ci-pins.env rewrite did not write DAV1D_TAG=$ci_latest; $CI_PINS_FILE was left untouched"
  [ "$(ci_pin_get "$ci_tmp_file" DAV1D_COMMIT)" = "$ci_new_commit" ] ||
    die "the ci-pins.env rewrite did not write DAV1D_COMMIT=$ci_new_commit; $CI_PINS_FILE was left untouched"
  chmod --reference="$CI_PINS_FILE" "$ci_tmp_file" 2>/dev/null || chmod 644 "$ci_tmp_file"
fi

# versions.env: the same row a --apply has always written, plus a count that
# ignores the dav1d-ci row (which has no variable in this file).
env_updates=0
declare -A NEW=()
for i in "${!order[@]}"; do
  [ "${action_list[$i]}" = update ] || continue
  [ "${order[$i]}" = "$CI_PIN_COMPONENT" ] && continue
  prefix=$(prefix_of "${order[$i]}")
  NEW["${prefix}_VERSION"]=${latest_list[$i]}
  [ -n "${new_url_list[$i]}" ] && NEW["${prefix}_URL"]=${new_url_list[$i]}
  [ -n "${new_sha_list[$i]}" ] && NEW["${prefix}_SHA256"]=${new_sha_list[$i]}
  if env_has "${prefix}_COMMIT"; then
    NEW["${prefix}_COMMIT"]=${new_commit_list[$i]}
  fi
  env_updates=$((env_updates + 1))
done

if [ "$env_updates" -gt 0 ]; then
  tmp_file=$(mktemp "${VERSIONS_FILE}.XXXXXX")
  while IFS= read -r line || [ -n "$line" ]; do
    var=${line%%=*}
    if [ "$line" != "$var" ] && [ -n "${NEW[$var]+set}" ]; then
      printf '%s=%s\n' "$var" "${NEW[$var]}"
    else
      printf '%s\n' "$line"
    fi
  done <"$VERSIONS_FILE" >"$tmp_file"

  # Nothing may be lost in the rewrite: every variable that was meant to change
  # must be in the output (this also catches a NEW key that matched no line), and
  # the file must still define exactly the same set of variables.
  for var in "${!NEW[@]}"; do
    grep -q "^$var=" "$tmp_file" || die "the rewrite dropped $var; $VERSIONS_FILE was left untouched"
  done
  assignments() { grep -cE '^[A-Za-z_][A-Za-z0-9_]*=' "$1" || true; }
  if [ "$(assignments "$VERSIONS_FILE")" != "$(assignments "$tmp_file")" ]; then
    die "the rewrite changed the number of variables in $VERSIONS_FILE; it was left untouched"
  fi

  # Keep the file's mode (mktemp creates 0600) so the rewrite is not a silent
  # permission change for a file other people's scripts source.
  chmod --reference="$VERSIONS_FILE" "$tmp_file" 2>/dev/null || chmod 644 "$tmp_file"
fi

# Both rewrites are complete and validated by now, so only a failure between
# these two renames can leave the tree half-updated; every query, download,
# hash, ls-remote and validation above leaves both files byte-identical.
if [ -n "$tmp_file" ]; then
  mv "$tmp_file" "$VERSIONS_FILE"
  tmp_file=""
fi
if [ -n "$ci_tmp_file" ]; then
  mv "$ci_tmp_file" "$CI_PINS_FILE"
  ci_tmp_file=""
fi

if [ "$env_updates" -gt 0 ]; then
  log "updated $env_updates component(s) in ${VERSIONS_FILE#"$REPO_ROOT"/}"
fi
if [ "$ci_action" = update ]; then
  log "updated the dav1d pin in ${CI_PINS_FILE#"$REPO_ROOT"/}: $ci_pinned -> $ci_latest ($ci_new_commit)"
fi
print_deltas
