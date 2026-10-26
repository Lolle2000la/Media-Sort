#!/usr/bin/env bash
# =============================================================================
# bump-appimage.sh -- find, report and apply upstream version bumps for the
# self-contained Linux AppImage payload.
#
# `packaging/appimage/versions.env` pins nine upstreams, and Renovate cannot
# maintain them: it can compute a new version, but not a new sha256 for an
# archive it never downloads. This script does that part. It only ever touches
# packaging/appimage/versions.env -- comments, ordering and formatting are
# preserved -- and never writes a version whose archive it did not download and
# hash itself.
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
# GitHub goes through `gh api` when it is on PATH (the runners have it, already
# authenticated), otherwise `curl` + `jq` with `GITHUB_TOKEN` when that is set.
# GitLab and the FFmpeg index are plain `curl`; GitLab needs `jq`.
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
#                        wayland-protocols libplacebo rust
#   --min-age-days N     soak period in days (0 disables it; default 14)
#   --allow-prerelease   also consider -rc/-beta/-alpha/-pre/dev tags
#   --json               machine-readable report instead of the table
#   --versions-file P    operate on P instead of packaging/appimage/versions.env
#                        (for testing this script, not for real bumps)
#   --help
#
# Exit status: 0 = nothing to do (or the apply succeeded), 1 = error (nothing
# was written), 2 = --check found updates.
#
# Deliberately NOT covered:
#
#   * the Flatpak manifest (flatpak/app.mediasort.MediaSort.yml), which pins the
#     same projects separately. It is a separate Flathub submission built
#     against a different runtime, so bumping both from one query would ship a
#     combination neither was tested with; bump it by hand and let
#     .github/workflows/flatpak-build.yml judge it.
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
MODE=report
MIN_AGE_DAYS=14
ALLOW_PRERELEASE=0
JSON=0
SELECTED=()

API_TIMEOUT=${API_TIMEOUT:-60}
DOWNLOAD_TIMEOUT=${DOWNLOAD_TIMEOUT:-600}

COMPONENTS=(mpv ffmpeg dav1d libass zimg wayland wayland-protocols libplacebo rust)

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
  epoch=$(date -u -d "$published" +%s 2>/dev/null) || return 1
  now=$(date -u +%s)
  [ $((now - epoch)) -ge $((MIN_AGE_DAYS * 86400)) ]
}

# --- HTTP --------------------------------------------------------------------

api_auth=()
if [ -n "${GITHUB_TOKEN:-}" ]; then
  api_auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
fi

# api_get <path> <jq filter> -- GitHub REST through gh when possible, curl+jq
# otherwise (authenticated when GITHUB_TOKEN is set).
api_get() {
  local path=$1 filter=$2 out
  if [ "$HAVE_GH" = 1 ]; then
    if out=$(gh api "$path" --jq "$filter" 2>/dev/null); then
      printf '%s' "$out"
      return 0
    fi
  fi
  command -v jq >/dev/null 2>&1 || die "jq is required to query the GitHub API without gh"
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
  date -u -d "$lm" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null
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
  while IFS="$TAB" read -r version date; do
    [ -n "$version" ] || continue
    is_prerelease "$component" "$version" && continue
    version_gt "$version" "$pinned" || continue
    age_ok "$date" || continue
    printf '%s\t%s\n' "$version" "$date"
    return 0
  done <<<"$list"
}

gh_tag_eligible() { # <component> <pinned>
  local component=$1 pinned=$2 repo tags name sha date found=""
  repo=$(repo_of "$component") || return 1
  tags=$(api_get "repos/$repo/tags?per_page=100" '.[] | [.name, .commit.sha] | @tsv') || return 1
  # Newest version first; stop at the pin (nothing below it can be an update).
  while IFS="$TAB" read -r name sha; do
    [ -n "$name" ] || continue
    is_prerelease "$component" "$name" && continue
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
  date=$(printf '%s\n' "$toml" | sed -n 's/^date = "\(.*\)"$/\1/p' | head -n 1)
  version=$(printf '%s\n' "$toml" |
    awk '/^\[pkg\.rust\]/ { in_rust = 1; next }
         /^\[/ { in_rust = 0 }
         in_rust && /^version = "/ { print; exit }' |
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
trap 'rm -rf "$WORK_DIR"; [ -z "$tmp_file" ] || rm -f "$tmp_file"' EXIT

check_url_templates

declare -a order=() pinned_list=() latest_list=() action_list=() status_list=() commit_list=()
declare -a new_url_list=() new_sha_list=() new_commit_list=()
updates=0
unknowns=0

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
  # No table and no downloads: whether anything is pending is the whole answer.
  print_deltas
  printf 'updates available: %d\n' "$updates"
  if [ "$unknowns" -gt 0 ]; then
    die "$unknowns component(s) could not be queried; the answer is incomplete"
  fi
  [ "$updates" -gt 0 ] && exit 2
  exit 0
fi

if [ "$JSON" = 1 ]; then print_json; else print_report; fi

if [ "$MODE" = report ]; then
  print_deltas
  exit 0
fi

# --- apply -------------------------------------------------------------------

if [ "$unknowns" -gt 0 ]; then
  die "$unknowns component(s) could not be queried; refusing to write $VERSIONS_FILE (narrow the run with --component if that upstream is down)"
fi

for i in "${!order[@]}"; do
  [ "${action_list[$i]}" = update ] || continue
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
  log "nothing to apply; ${VERSIONS_FILE#"$REPO_ROOT"/} is already current"
  exit 0
fi

declare -A NEW=()
for i in "${!order[@]}"; do
  [ "${action_list[$i]}" = update ] || continue
  prefix=$(prefix_of "${order[$i]}")
  NEW["${prefix}_VERSION"]=${latest_list[$i]}
  [ -n "${new_url_list[$i]}" ] && NEW["${prefix}_URL"]=${new_url_list[$i]}
  [ -n "${new_sha_list[$i]}" ] && NEW["${prefix}_SHA256"]=${new_sha_list[$i]}
  if env_has "${prefix}_COMMIT"; then
    NEW["${prefix}_COMMIT"]=${new_commit_list[$i]}
  fi
done

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

mv "$tmp_file" "$VERSIONS_FILE"
tmp_file=""
log "updated $updates component(s) in ${VERSIONS_FILE#"$REPO_ROOT"/}"
print_deltas
