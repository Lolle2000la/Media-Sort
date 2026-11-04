#!/usr/bin/env bash
#
# Check that the machine about to render the demo videos can shape Japanese text.
#
# `media-sort-gui --export` draws the UI through iced/cosmic-text, which resolves
# `Family::SansSerif` against the *installed* system fonts and falls back over
# them; the binary bundles no CJK font of its own. On a machine without a
# Japanese-capable font every `ja` glyph renders as a tofu box (an empty
# rectangle) and the export still exits 0 -- the failure is silent until someone
# watches the video. `fc-list :lang=ja` is the predicate: it lists every font
# fontconfig believes covers Japanese, so an empty list means the `ja` demos
# would be tofu.
#
# Usage: scripts/check-render-fonts.sh [--selftest]
#
# `--selftest` proves the predicate both ways using a throwaway fontconfig (an
# empty font directory must fail, the real system config must pass), so the gate
# can be exercised on a machine that does have the font. Needy of nothing beyond
# coreutils + fontconfig.
set -euo pipefail

# True when fontconfig reports at least one font claiming Japanese coverage.
# Honours FONTCONFIG_FILE, which is how --selftest points it at an empty config.
has_ja_font() {
  [ -n "$(fc-list :lang=ja 2>/dev/null)" ]
}

if ! command -v fc-list >/dev/null 2>&1; then
  if [ "$(uname -s)" != Linux ]; then
    echo "Skipping the Japanese font check: fc-list is not installed and this is a Linux CI gate."
    exit 0
  fi
  echo "::error::fc-list is missing, so the Japanese font check cannot run."
  echo "Install fontconfig (sudo apt-get install -y fontconfig) and re-run; without a"
  echo "CJK font the demo videos render Japanese as tofu boxes."
  exit 1
fi

selftest() {
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  mkdir -p "$tmp/fonts" "$tmp/cache"
  cat >"$tmp/fonts.conf" <<EOF
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir>$tmp/fonts</dir>
  <cachedir>$tmp/cache</cachedir>
</fontconfig>
EOF

  if FONTCONFIG_FILE="$tmp/fonts.conf" has_ja_font; then
    echo "::error::selftest: a font directory with no fonts reported a Japanese font"
    exit 1
  fi
  echo "selftest: an empty font set is rejected, as intended"

  if has_ja_font; then
    echo "selftest: the system config is accepted, as intended"
  else
    echo "selftest: no Japanese font on this host, so the positive half is skipped"
  fi
}

if [ "${1-}" = "--selftest" ]; then
  selftest
  exit 0
fi

if has_ja_font; then
  echo "Japanese-capable fonts found: $(fc-list :lang=ja | head -3 | cut -d: -f1 | paste -sd, -)"
  exit 0
fi

echo "::error::no font with Japanese coverage is installed, so the ja demo videos would render as tofu."
echo "Fix (Debian/Ubuntu): sudo apt-get install -y fonts-noto-cjk"
exit 1
