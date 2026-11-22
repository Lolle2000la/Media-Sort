#!/usr/bin/env bash
#
# Cut a release: bump the workspace version, refresh Cargo.lock, commit, and
# create the signed annotated tag that drives .github/workflows/release.yml.
#
# The tag is created HERE (not in CI) so it is signed with your own key
# (commit.gpgsign / tag.gpgsign), and its message is the prefacing release
# summary that the publish job prepends to the generated changelog. Pushing
# the tag is what finally triggers the release workflow.
#
# Usage:
#   scripts/release.sh <version> [-m <message> | -F <notes-file>] [options]
#
# Examples:
#   scripts/release.sh 3.0.0-alpha.18 -F /tmp/notes.md
#   scripts/release.sh 3.0.0 -m "First stable release of the Rust rewrite."
#   scripts/release.sh 3.0.0-alpha.19 --dry-run
#
# Options:
#   -m, --message <text>    Signed tag message (the release summary).
#   -F, --notes-file <file> Read the tag message from a file.
#       --require-ci        Fail unless every Actions run for HEAD succeeded.
#       --no-push           Stop after creating the tag; push it yourself.
#       --dry-run           Validate and print the plan; change nothing.
#       --no-verify         Skip the CI status check entirely.
#   -h, --help              Show this help.
#
# The version is the single source of truth: it is written to
# [workspace.package].version in Cargo.toml and mirrored into Cargo.lock, and
# release.yml refuses to build a tag whose name (minus the leading "v") does
# not match the manifest.
#
# Two more files carry a version the OS reads, and both are synced here so the
# tag alone determines every version in the shipped artifacts:
#   packaging/macos/Info.plist                 CFBundleShortVersionString + CFBundleVersion
#   crates/media-sort-gui/media-sort-gui.rc     Windows VERSIONINFO resource
#
# The AppStream metadata is synced too, so Flathub's release list cannot drift
# from the tags:
#   packaging/linux/app.mediasort.MediaSort.metainfo.xml   <releases> entry

set -euo pipefail

VERSION=""
TAG_MESSAGE=""
NOTES_FILE=""
REQUIRE_CI=0
NO_PUSH=0
DRY_RUN=0
NO_VERIFY=0

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '==> %s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }

usage() {
  cat <<'EOF'
Cut a release: bump the workspace version, refresh Cargo.lock, commit, and
create the signed annotated tag that drives .github/workflows/release.yml.

Usage:
  scripts/release.sh <version> [-m <message> | -F <notes-file>] [options]

Examples:
  scripts/release.sh 3.0.0-alpha.18 -F /tmp/notes.md
  scripts/release.sh 3.0.0 -m "First stable release of the Rust rewrite."
  scripts/release.sh 3.0.0-alpha.19 --dry-run

Options:
  -m, --message <text>    Signed tag message (the release summary).
  -F, --notes-file <file> Read the tag message from a file.
      --require-ci        Fail unless every Actions run for HEAD succeeded.
      --no-push           Stop after creating the tag; push it yourself.
      --dry-run           Validate and print the plan; change nothing.
      --no-verify         Skip the CI status check entirely.
  -h, --help              Show this help.
EOF
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    -m|--message)
      [ $# -ge 2 ] || die "$1 requires an argument"
      TAG_MESSAGE="$2"; shift 2 ;;
    -F|--notes-file)
      [ $# -ge 2 ] || die "$1 requires an argument"
      NOTES_FILE="$2"; shift 2 ;;
    --require-ci) REQUIRE_CI=1; shift ;;
    --no-push) NO_PUSH=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --no-verify) NO_VERIFY=1; shift ;;
    -h|--help) usage 0 ;;
    -*) die "unknown option: $1 (try --help)" ;;
    *)
      [ -z "$VERSION" ] || die "version given twice: '$VERSION' and '$1'"
      VERSION="$1"; shift ;;
  esac
done

[ -n "$VERSION" ] || { printf 'error: missing <version>\n\n' >&2; usage 1; }
[ -z "$TAG_MESSAGE" ] || [ -z "$NOTES_FILE" ] || die "-m and -F are mutually exclusive"

VERSION="${VERSION#v}"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  die "invalid version '$VERSION' (expected e.g. 3.0.0 or 3.0.0-alpha.18)"
fi
TAG="v$VERSION"

command -v cargo >/dev/null || die "cargo not found on PATH"

cd "$(git rev-parse --show-toplevel)" || die "not inside a git repository"

# --- preflight -------------------------------------------------------------

info "preflight checks"

git rev-parse --verify -q HEAD >/dev/null || die "repository has no commits"

BRANCH="$(git rev-parse --abbrev-ref HEAD)"
[ "$BRANCH" != "HEAD" ] || die "refusing to release from a detached HEAD"

if [ -n "$(git status --porcelain)" ]; then
  die "working tree is not clean; commit or stash first:
$(git status --short)"
fi

CURRENT_VERSION="$(awk '
  /^\[workspace\.package\]/ { in_section = 1; next }
  /^\[/ { in_section = 0 }
  in_section && /^version[[:space:]]*=/ { gsub(/^[^"]*"|".*$/, ""); print; exit }
' Cargo.toml)"
[ -n "$CURRENT_VERSION" ] || die "could not read [workspace.package].version from Cargo.toml"
[ "$VERSION" != "$CURRENT_VERSION" ] || die "Cargo.toml is already at version $VERSION"

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  die "tag $TAG already exists locally"
fi
git fetch --quiet --tags origin || warn "could not fetch from origin; continuing"
if git ls-remote --tags --exit-code origin "refs/tags/$TAG" >/dev/null 2>&1; then
  die "tag $TAG already exists on origin"
fi

if git rev-parse --abbrev-ref --symbolic-full-name '@{u}' >/dev/null 2>&1; then
  read -r BEHIND AHEAD < <(git rev-list --left-right --count '@{u}...HEAD')
  [ "$BEHIND" -eq 0 ] || die "local $BRANCH is $BEHIND commit(s) behind its upstream; pull first"
  [ "$AHEAD" -eq 0 ] || warn "local $BRANCH is $AHEAD commit(s) ahead of upstream (they will be pushed too)"
else
  warn "branch $BRANCH has no upstream configured"
fi

check_ci() {
  local head_sha runs_json bad
  head_sha="$(git rev-parse HEAD)"
  if ! command -v gh >/dev/null 2>&1; then
    warn "gh not found; skipping CI status check"
    return 0
  fi
  if ! command -v jq >/dev/null 2>&1; then
    warn "jq not found; skipping CI status check"
    return 0
  fi
  runs_json="$(gh run list --commit "$head_sha" --limit 100 \
    --json name,status,conclusion 2>/dev/null || echo '[]')"
  if [ "$runs_json" = "[]" ] || [ -z "$runs_json" ]; then
    if [ "$REQUIRE_CI" -eq 1 ]; then
      die "no Actions runs found for HEAD ($head_sha)"
    fi
    warn "no Actions runs found for HEAD ($head_sha); skipping CI check"
    return 0
  fi
  bad="$(printf '%s' "$runs_json" | jq -r '
    [ .[] | select(
        .status != "completed"
        or (.conclusion | IN("success", "skipped", "neutral") | not)
      ) | "  \(.name): \(.status)/\(.conclusion)" ] | join("\n")')"
  if [ -n "$bad" ]; then
    if [ "$REQUIRE_CI" -eq 1 ]; then
      die "CI is not green for HEAD ($head_sha):
$bad"
    fi
    warn "CI is not green for HEAD ($head_sha):
$bad"
  else
    info "all Actions runs for HEAD succeeded"
  fi
}

if [ "$NO_VERIFY" -eq 0 ]; then
  check_ci
fi

# --- derived version fields ------------------------------------------------

INFO_PLIST="packaging/macos/Info.plist"
RC_FILE="crates/media-sort-gui/media-sort-gui.rc"
METAINFO_XML="packaging/linux/app.mediasort.MediaSort.metainfo.xml"

# Apple's CFBundleShortVersionString is 1-3 period-separated integers, so the
# prerelease/build suffix is dropped (3.0.0-rc.1 -> 3.0.0).
MARKETING_VERSION="${VERSION%%-*}"
MARKETING_VERSION="${MARKETING_VERSION%%+*}"
IFS=. read -r VER_MAJOR VER_MINOR VER_PATCH _ <<< "$MARKETING_VERSION"

# The `<release date="...">` an AppStream entry needs (UTC, as Flathub shows it).
RELEASE_DATE="$(date -u +%Y-%m-%d)"

# Monotonically increasing build number for CFBundleVersion / FILEVERSION: the
# Nth release is numbered N (1-based over every existing v* tag). Swap this for
# `git rev-list --count HEAD` if you would rather track commits than releases.
BUILD_NUMBER="$(( $(git tag -l 'v*' | wc -l | tr -d '[:space:]') + 1 ))"

# --- plan ------------------------------------------------------------------

if [ -n "$NOTES_FILE" ]; then
  [ -f "$NOTES_FILE" ] || die "notes file not found: $NOTES_FILE"
  TAG_ARGS=(-F "$NOTES_FILE")
elif [ -n "$TAG_MESSAGE" ]; then
  TAG_ARGS=(-m "$TAG_MESSAGE")
else
  warn "no -m/-F given; using a default tag message"
  TAG_ARGS=(-m "Release $TAG")
fi

info "version:   $CURRENT_VERSION -> $VERSION"
info "marketing: $MARKETING_VERSION (CFBundleShortVersionString)"
info "build:     $BUILD_NUMBER (CFBundleVersion / FILEVERSION)"
info "appstream: <release version=\"$VERSION\" date=\"$RELEASE_DATE\"/>"
info "branch:    $BRANCH"
info "tag:       $TAG (signed)"

if [ "$DRY_RUN" -eq 1 ]; then
  info "dry run: no changes made"
  exit 0
fi

# --- bump ------------------------------------------------------------------

BUMPED=0
COMMITTED=0

restore() {
  local code=$?
  if [ "$code" -ne 0 ] && [ "$COMMITTED" -eq 0 ] && [ "$BUMPED" -eq 1 ]; then
    warn "rolling back the version files"
    git checkout -- Cargo.toml Cargo.lock "$INFO_PLIST" "$RC_FILE" "$METAINFO_XML" 2>/dev/null || true
  fi
  return "$code"
}
trap restore EXIT

info "bumping Cargo.toml"
CARGO_TMP="$(mktemp)"
awk -v new="$VERSION" '
  /^\[workspace\.package\]/ { in_section = 1; print; next }
  /^\[/ { in_section = 0 }
  in_section && !done && /^version[[:space:]]*=/ { sub(/=.*/, "= \"" new "\""); done = 1 }
  { print }
' Cargo.toml > "$CARGO_TMP"
grep -q "^version = \"$VERSION\"\$" "$CARGO_TMP" || { rm -f "$CARGO_TMP"; die "failed to rewrite the workspace version"; }
mv "$CARGO_TMP" Cargo.toml
BUMPED=1

info "refreshing Cargo.lock"
cargo update --workspace >/dev/null 2>&1 || die "cargo update --workspace failed"
cargo metadata --locked --format-version 1 >/dev/null || die "Cargo.lock is inconsistent after the bump"
LOCK_VERSION="$(grep -A1 '^name = "media-sort-gui"$' Cargo.lock | grep '^version = ' | head -1 | cut -d'"' -f2)"
[ "$LOCK_VERSION" = "$VERSION" ] || die "Cargo.lock still has media-sort-gui $LOCK_VERSION"

info "syncing macOS bundle version ($INFO_PLIST)"
PLIST_TMP="$(mktemp)"
awk -v sv="$MARKETING_VERSION" -v bv="$BUILD_NUMBER" '
  { lines[NR] = $0; if ($0 ~ /<key>CFBundleVersion<\/key>/) have_bv = 1 }
  END {
    for (i = 1; i <= NR; i++) {
      l = lines[i]
      if (l ~ /<key>CFBundleShortVersionString<\/key>/) {
        print l
        v = lines[++i]
        sub(/<string>.*<\/string>/, "<string>" sv "</string>", v)
        print v
        if (!have_bv) {
          print "    <key>CFBundleVersion</key>"
          print "    <string>" bv "</string>"
        }
        continue
      }
      if (l ~ /<key>CFBundleVersion<\/key>/) {
        print l
        v = lines[++i]
        sub(/<string>.*<\/string>/, "<string>" bv "</string>", v)
        print v
        continue
      }
      print l
    }
  }
' "$INFO_PLIST" > "$PLIST_TMP"
[ "$(grep -c '<key>CFBundleVersion</key>' "$PLIST_TMP")" = "1" ] || { rm -f "$PLIST_TMP"; die "failed to write CFBundleVersion in $INFO_PLIST"; }
grep -A1 '<key>CFBundleShortVersionString</key>' "$PLIST_TMP" | grep -q "<string>$MARKETING_VERSION</string>" || { rm -f "$PLIST_TMP"; die "failed to write CFBundleShortVersionString in $INFO_PLIST"; }
grep -A1 '<key>CFBundleVersion</key>' "$PLIST_TMP" | grep -q "<string>$BUILD_NUMBER</string>" || { rm -f "$PLIST_TMP"; die "failed to write CFBundleVersion in $INFO_PLIST"; }
mv "$PLIST_TMP" "$INFO_PLIST"

info "syncing Windows version resource ($RC_FILE)"
RC_TMP="$(mktemp)"
RC_BLOCK="$(mktemp)"
cat > "$RC_BLOCK" <<EOF
// MS_VERSION_BEGIN (generated by scripts/release.sh -- do not edit by hand)
1 VERSIONINFO
FILEVERSION ${VER_MAJOR},${VER_MINOR},${VER_PATCH},${BUILD_NUMBER}
PRODUCTVERSION ${VER_MAJOR},${VER_MINOR},${VER_PATCH},${BUILD_NUMBER}
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Media Sort"
      VALUE "FileDescription", "Media Sort"
      VALUE "FileVersion", "${VERSION}"
      VALUE "InternalName", "media-sort-gui"
      VALUE "OriginalFilename", "media-sort-gui.exe"
      VALUE "LegalCopyright", "AGPL-3.0-or-later"
      VALUE "ProductName", "Media Sort"
      VALUE "ProductVersion", "${VERSION}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
// MS_VERSION_END
EOF
awk -v blockfile="$RC_BLOCK" '
  function emit(   line) {
    while ((getline line < blockfile) > 0) print line
    close(blockfile)
  }
  /^\/\/ MS_VERSION_BEGIN/ { emit(); found = 1; skip = 1; next }
  /^\/\/ MS_VERSION_END/ { skip = 0; next }
  !skip { print }
  END { if (!found) { print ""; emit() } }
' "$RC_FILE" > "$RC_TMP"
[ "$(grep -c 'MS_VERSION_BEGIN' "$RC_TMP")" = "1" ] || { rm -f "$RC_TMP" "$RC_BLOCK"; die "failed to write the version block in $RC_FILE"; }
grep -q "VALUE \"ProductVersion\", \"$VERSION\"" "$RC_TMP" || { rm -f "$RC_TMP" "$RC_BLOCK"; die "failed to write ProductVersion in $RC_FILE"; }
mv "$RC_TMP" "$RC_FILE"
rm -f "$RC_BLOCK"

# Add — or re-date — this version's entry in the AppStream metadata, so the
# <releases> list Flathub reads from the tagged source tree always ends at the
# tag being cut (release.yml's verify-version fails the build otherwise).
# Idempotent: an entry that already carries $VERSION only gets its date
# rewritten, never duplicated or reordered. A prerelease (a version with a
# `-` suffix) is marked `type="development"`, which is how AppStream/Flathub
# label a release that is not the stable one.
sync_metainfo_xml() {
  local tmp first_release release_type=""
  case "$VERSION" in
    *-*) release_type='type="development" ' ;;
  esac
  tmp="$(mktemp)"
  if ! awk -v ver="$VERSION" -v date="$RELEASE_DATE" -v type_attr="$release_type" '
    # Anchored tag patterns: the header comment in this file mentions
    # <releases>/<release> in prose, which must not be mistaken for markup.
    /^[[:space:]]*<releases>[[:space:]]*$/ { in_releases = 1; releases_line = NR }
    /^[[:space:]]*<\/releases>[[:space:]]*$/ { in_releases = 0 }
    in_releases && first_line == 0 && /^[[:space:]]*<release[ >]/ { first_line = NR }
    in_releases && /^[[:space:]]*<release[ >]/ && index($0, "version=\"" ver "\"") > 0 { existing = NR }
    { lines[NR] = $0 }
    END {
      if (releases_line == 0) { exit 1 }
      if (existing > 0) {
        insert_after = 0
      } else if (first_line > 0) {
        # Above the current top entry (the file keeps a "add new releases at
        # the top" comment there, which stays where it is).
        insert_after = first_line - 1
      } else {
        insert_after = releases_line
      }
      if (first_line > 0) {
        match(lines[first_line], /^[[:space:]]*/)
        indent = substr(lines[first_line], 1, RLENGTH)
      } else {
        match(lines[releases_line], /^[[:space:]]*/)
        indent = substr(lines[releases_line], 1, RLENGTH) "  "
      }
      for (i = 1; i <= NR; i++) {
        line = lines[i]
        if (i == existing) {
          # Rewrite only the date attribute: indentation, version and any other
          # attribute (including type="development") stay exactly as they were.
          if (line ~ /date="[^"]*"/) {
            sub(/date="[^"]*"/, "date=\"" date "\"", line)
          } else if (line ~ /\/>[[:space:]]*$/) {
            # Self-closing tag: `<release .../>`.
            sub(/\/>[[:space:]]*$/, "date=\"" date "\"/>", line)
          } else {
            # Tag with children: `<release ...>`.
            sub(/>[[:space:]]*$/, " date=\"" date "\">", line)
          }
        }
        print line
        if (i == insert_after) {
          print indent "<release " type_attr "version=\"" ver "\" date=\"" date "\"/>"
        }
      }
    }
  ' "$METAINFO_XML" > "$tmp"; then
    rm -f "$tmp"
    die "no <releases> block in $METAINFO_XML"
  fi

  first_release="$(grep -m1 -E '^[[:space:]]*<release[ >]' "$tmp" || true)"
  case "$first_release" in
    *"version=\"$VERSION\""*) ;;
    *)
      rm -f "$tmp"
      die "failed to write <release version=\"$VERSION\"> as the first entry in $METAINFO_XML"
      ;;
  esac
  case "$first_release" in
    *"date=\"$RELEASE_DATE\""*) ;;
    *)
      rm -f "$tmp"
      die "failed to write date=\"$RELEASE_DATE\" for version $VERSION in $METAINFO_XML"
      ;;
  esac
  case "$VERSION" in
    *-*)
      case "$first_release" in
        *'type="development"'*) ;;
        *)
          rm -f "$tmp"
          die "failed to mark the prerelease $VERSION as type=\"development\" in $METAINFO_XML"
          ;;
      esac
      ;;
  esac

  mv "$tmp" "$METAINFO_XML"
}

info "syncing AppStream metadata ($METAINFO_XML)"
sync_metainfo_xml

info "committing"
git add Cargo.toml Cargo.lock "$INFO_PLIST" "$RC_FILE" "$METAINFO_XML"
git commit -m "chore: bump version to $VERSION"
COMMITTED=1

info "creating signed tag $TAG"
git tag -s "$TAG" "${TAG_ARGS[@]}" || {
  die "failed to create the signed tag; is tag signing configured (tag.gpgsign / user.signingkey)?"
}

if [ "$NO_PUSH" -eq 1 ]; then
  info "skipping push (--no-push); finish with:"
  printf '    git push origin %s\n    git push origin refs/tags/%s\n' "$BRANCH" "$TAG" >&2
  exit 0
fi

info "pushing $BRANCH"
git push origin "HEAD:refs/heads/$BRANCH"
info "pushing tag $TAG (this triggers the release workflow)"
git push origin "refs/tags/$TAG"

info "done. $TAG pushed; the release workflow is building it now."
cat >&2 <<EOF

    gh run watch \$(gh run list --workflow=release.yml --limit=1 --json databaseId --jq '.[0].databaseId')
EOF
