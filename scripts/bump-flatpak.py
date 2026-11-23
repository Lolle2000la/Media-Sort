#!/usr/bin/env python3
"""Find, report and apply upstream dependency bumps for the Flatpak manifest.

`flatpak/app.mediasort.MediaSort.yml` pins the same projects as the AppImage,
but separately: it is a Flathub build against a different runtime, and each
module is a `type: git` source whose integrity pin is a tag *and* the commit it
points at (Flathub's linter rejects a tag-only source). Renovate cannot maintain
those: it can find a new tag, but not prove which commit it resolves to.

This script keeps the six dependency modules current:

    component          upstream                                   manifest module
    ------------------ ------------------------------------------ -----------------
    mpv                GitHub releases                            mpv
    libass             GitHub releases                            libass
    libplacebo         GitHub releases                            libplacebo
    zimg               GitHub tags?per_page=100                   zimg
    wayland-protocols  GitLab repository/tags                     wayland-protocols
    libXpresent        GitLab repository/tags                     libXpresent

It deliberately does not touch the two *release-owned* sources:

  * the `media-sort` git source (tag + commit of this repository), and
  * the `cargo-vendor.tar.gz` archive (url + sha256 of that release's asset),

because those move when a release is cut, not on a weekly schedule; Flathub's
external data checker also keeps them in step. The Freedesktop runtime version
is likewise a deliberate, human-bumped choice.

Policy is the same as `scripts/bump-appimage.sh`: a candidate must be the
newest non-prerelease tag above the pin, and at least `--min-age-days` old (the
14-day soak from AGENTS.md "Dependency updates (Renovate)"). Ordering is numeric,
so `v7.387.0` beats `v7.360.1` (a lexicographic sort would get that wrong).
wayland-protocols development snapshots (`1.26.90`) count as prereleases.

Usage:
  scripts/bump-flatpak.py [options]

  (default)            dry run: print the report table, change nothing
  --apply              rewrite the manifest in place (temp file + atomic rename)
  --check              print only whether updates are available; exit 2 when
                       there are, 0 when there are none (no writes)
  --component NAME     limit the run to one module (repeatable): mpv libass
                       libplacebo zimg wayland-protocols libXpresent
  --min-age-days N     soak period in days (0 disables it; default 14)
  --json               one JSON document on stdout instead of the table
  --manifest P         operate on P instead of
                       flatpak/app.mediasort.MediaSort.yml (for testing)
  --help

Exit status: 0 = nothing to do (or the apply succeeded), 1 = error (nothing was
written), 2 = --check found updates.

Nothing is written until every update is resolved *and* its commit is fetched;
the manifest is then rewritten by a single os.replace, and the result is checked
against the original with every tag/commit value masked out, so a dropped line,
a lost comment or a re-indented block cannot pass silently.

`GITHUB_TOKEN`/`GH_TOKEN` are used for the GitHub API when present (the bump
workflow sets one); GitLab is public.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Iterable, Sequence

SCRIPT_NAME = Path(sys.argv[0]).name
REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = REPO_ROOT / "flatpak" / "app.mediasort.MediaSort.yml"

API_TIMEOUT = float(os.environ.get("API_TIMEOUT", "60"))
API_RETRIES = 3

GITLAB_PREFIX = "https://gitlab.freedesktop.org/"
GITHUB_API = "https://api.github.com/"


class BumpError(Exception):
    """A problem that must stop the run (nothing has been written yet)."""


class UnknownError(BumpError):
    """An upstream could not be queried, so the answer is incomplete."""


def log(message: str) -> None:
    print(f"{SCRIPT_NAME}: {message}", file=sys.stderr)


def die(message: str) -> None:
    print(f"{SCRIPT_NAME}: ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


# ---------------------------------------------------------------------------
# upstream model
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Component:
    """One manifest module and where its newest release can be found."""

    name: str
    kind: str  # gh-release | gh-tag | gitlab-tag
    project: str  # owner/repo on GitHub, path on GitLab
    tag_re: re.Pattern[str]
    git_url: str


COMPONENTS: tuple[Component, ...] = (
    Component(
        "mpv",
        "gh-release",
        "mpv-player/mpv",
        re.compile(r"^v(\d+(?:\.\d+)*)$"),
        "https://github.com/mpv-player/mpv.git",
    ),
    Component(
        "libass",
        "gh-release",
        "libass/libass",
        re.compile(r"^(\d+(?:\.\d+)*)$"),
        "https://github.com/libass/libass.git",
    ),
    Component(
        "libplacebo",
        "gh-release",
        "haasn/libplacebo",
        re.compile(r"^v(\d+(?:\.\d+)*)$"),
        "https://github.com/haasn/libplacebo.git",
    ),
    Component(
        "zimg",
        "gh-tag",
        "sekrit-twc/zimg",
        re.compile(r"^release-(\d+(?:\.\d+)*)$"),
        "https://github.com/sekrit-twc/zimg.git",
    ),
    Component(
        "wayland-protocols",
        "gitlab-tag",
        "wayland/wayland-protocols",
        re.compile(r"^(\d+(?:\.\d+)*)$"),
        "https://gitlab.freedesktop.org/wayland/wayland-protocols.git",
    ),
    Component(
        "libXpresent",
        "gitlab-tag",
        "xorg/lib/libxpresent",
        re.compile(r"^libXpresent-(\d+(?:\.\d+)*)$"),
        "https://gitlab.freedesktop.org/xorg/lib/libxpresent.git",
    ),
)

BY_NAME = {component.name: component for component in COMPONENTS}


@dataclass(frozen=True)
class Candidate:
    tag: str
    date: datetime | None


# ---------------------------------------------------------------------------
# small helpers: versions, dates, HTTP
# ---------------------------------------------------------------------------


def version_core(component: Component, tag: str) -> str | None:
    match = component.tag_re.match(tag)
    return match.group(1) if match else None


def version_parts(core: str) -> tuple[int, ...]:
    return tuple(int(part) for part in core.split("."))


def version_gt(left: str, right: str) -> bool:
    a, b = version_parts(left), version_parts(right)
    width = max(len(a), len(b))
    return a + (0,) * (width - len(a)) > b + (0,) * (width - len(b))


def is_prerelease(component: Component, core: str) -> bool:
    # The tag patterns only match stable-looking numeric tags, so the marker
    # families ("-rc", "-beta", ...) cannot reach this point. wayland-protocols
    # publishes development snapshots as odd micro releases (1.26.90), which do
    # match: a third component >= 90 is never a stable release.
    if component.name == "wayland-protocols":
        parts = version_parts(core)
        return len(parts) >= 3 and parts[2] >= 90
    return False


def parse_date(value: object) -> datetime | None:
    if not isinstance(value, str) or not value.strip():
        return None
    text = value.strip()
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        parsed = datetime.fromisoformat(text)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def soaked(date: datetime | None, min_age_days: int) -> bool:
    if min_age_days <= 0:
        return True
    if date is None:
        return False
    return datetime.now(timezone.utc) - date >= timedelta(days=min_age_days)


def http_get(url: str) -> str:
    headers = {
        "Accept": "application/json",
        "User-Agent": f"media-sort-{SCRIPT_NAME}",
    }
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token and url.startswith(GITHUB_API):
        headers["Authorization"] = f"Bearer {token}"
        headers["Accept"] = "application/vnd.github+json"

    last_error: Exception | None = None
    for attempt in range(API_RETRIES):
        request = urllib.request.Request(url, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=API_TIMEOUT) as response:
                return response.read().decode("utf-8", "replace")
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            last_error = exc
            if attempt + 1 < API_RETRIES:
                time.sleep(2 * (attempt + 1))
    raise UnknownError(f"could not query {url}: {last_error}")


def gh_json(path: str) -> object:
    return json.loads(http_get(GITHUB_API + path.lstrip("/")))


def gitlab_json(project: str, suffix: str) -> object:
    quoted = urllib.parse.quote(project, safe="")
    return json.loads(http_get(f"{GITLAB_PREFIX}api/v4/projects/{quoted}/{suffix}"))


def sorted_by_version(
    component: Component, items: Iterable[tuple[str, Candidate]]
) -> list[tuple[str, Candidate]]:
    """Newest version first, dropping tags that are not releases of this project."""
    kept = [
        (core, candidate)
        for core, candidate in items
        if component.tag_re.match(candidate.tag)
    ]
    kept.sort(key=lambda pair: version_parts(pair[0]), reverse=True)
    return kept


# ---------------------------------------------------------------------------
# candidate resolution
# ---------------------------------------------------------------------------


def gh_release_eligible(
    component: Component, pinned: str, min_age_days: int
) -> Candidate | None:
    # The list is sorted by version below, not by API order: a backport
    # published after the newest release must not win, and releases/latest
    # answers "newest *published*", which is exactly that trap.
    releases = gh_json(f"repos/{component.project}/releases?per_page=30")
    if not isinstance(releases, list):
        raise UnknownError(f"unexpected releases list for {component.project}")

    candidates = [
        (
            version_core(component, release.get("tag_name", "")) or "",
            Candidate(
                release.get("tag_name", ""),
                parse_date(release.get("published_at")),
            ),
        )
        for release in releases
        if isinstance(release, dict)
        and not release.get("draft")
        and not release.get("prerelease")
    ]
    for core, candidate in sorted_by_version(component, candidates):
        if not version_gt(core, pinned) or is_prerelease(component, core):
            continue
        if soaked(candidate.date, min_age_days):
            return candidate
    return None


def gh_tag_eligible(
    component: Component, pinned: str, min_age_days: int
) -> Candidate | None:
    tags = gh_json(f"repos/{component.project}/tags?per_page=100")
    if not isinstance(tags, list):
        raise UnknownError(f"unexpected tags list for {component.project}")

    commits = {
        tag.get("name"): tag.get("commit", {}).get("sha")
        for tag in tags
        if isinstance(tag, dict)
    }
    candidates = [
        (
            version_core(component, tag.get("name", "")) or "",
            Candidate(tag.get("name", ""), None),
        )
        for tag in tags
        if isinstance(tag, dict)
    ]
    for core, candidate in sorted_by_version(component, candidates):
        if not version_gt(core, pinned):
            break
        if is_prerelease(component, core):
            continue
        commit = commits.get(candidate.tag)
        if not isinstance(commit, str) or not commit:
            raise UnknownError(
                f"github tag {candidate.tag} of {component.project} has no commit"
            )
        detail = gh_json(f"repos/{component.project}/commits/{commit}")
        if not isinstance(detail, dict):
            raise UnknownError(f"unexpected commit detail for {component.project}")
        date = parse_date(detail.get("commit", {}).get("committer", {}).get("date"))
        if soaked(date, min_age_days):
            return Candidate(candidate.tag, date)
    return None


def gitlab_tag_eligible(
    component: Component, pinned: str, min_age_days: int
) -> Candidate | None:
    tags = gitlab_json(component.project, "repository/tags?per_page=100")
    if not isinstance(tags, list):
        raise UnknownError(f"unexpected tag list for {component.project}")

    candidates = [
        (
            version_core(component, tag.get("name", "")) or "",
            Candidate(
                tag.get("name", ""),
                parse_date(tag.get("commit", {}).get("committed_date")),
            ),
        )
        for tag in tags
        if isinstance(tag, dict)
    ]
    for core, candidate in sorted_by_version(component, candidates):
        if not version_gt(core, pinned):
            break
        if is_prerelease(component, core):
            continue
        if soaked(candidate.date, min_age_days):
            return candidate
    return None


def eligible(
    component: Component, pinned: str, min_age_days: int
) -> Candidate | None:
    if component.kind == "gh-release":
        return gh_release_eligible(component, pinned, min_age_days)
    if component.kind == "gh-tag":
        return gh_tag_eligible(component, pinned, min_age_days)
    if component.kind == "gitlab-tag":
        return gitlab_tag_eligible(component, pinned, min_age_days)
    raise BumpError(f"{component.name}: unknown upstream kind {component.kind!r}")


SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def peeled_commit(component: Component, tag: str) -> str:
    """The commit a tag points at, never an annotated tag's own object id.

    This is what Flathub's manifest needs and what `git rev-parse <tag>^{commit}`
    would print. `git ls-remote` needs no clone; the `^{}` line is the peeled
    commit, and its absence means the tag is lightweight.
    """
    proc = subprocess.run(
        [
            "git",
            "ls-remote",
            "--tags",
            component.git_url,
            f"refs/tags/{tag}",
            f"refs/tags/{tag}^{{}}",
        ],
        capture_output=True,
        text=True,
        timeout=120,
    )
    if proc.returncode != 0:
        raise UnknownError(
            f"git ls-remote failed for {component.name} {tag}: {proc.stderr.strip()}"
        )
    peeled = plain = None
    for line in proc.stdout.splitlines():
        sha, _, ref = line.partition("\t")
        if ref.endswith("^{}"):
            peeled = sha.strip()
        elif ref == f"refs/tags/{tag}":
            plain = sha.strip()
    commit = peeled or plain
    if not commit or not SHA_RE.match(commit):
        raise UnknownError(
            f"git ls-remote returned no commit for {component.name} {tag}"
        )
    return commit


# ---------------------------------------------------------------------------
# manifest reading and rewriting
# ---------------------------------------------------------------------------

MODULE_RE = re.compile(r"^  - name: (\S+)\s*$")
PIN_RE = re.compile(r"^(        )(tag|commit):\s*(.*?)\s*$")


@dataclass
class Module:
    name: str
    start: int
    end: int
    tag_index: int | None = None
    commit_index: int | None = None
    tag_raw: str = ""
    commit_raw: str = ""


def unquote(raw: str) -> str:
    raw = raw.strip()
    if len(raw) >= 2 and raw[0] == raw[-1] and raw[0] in ("'", '"'):
        return raw[1:-1]
    return raw


def parse_modules(lines: Sequence[str]) -> list[Module]:
    modules: list[Module] = []
    current: Module | None = None
    for index, line in enumerate(lines):
        match = MODULE_RE.match(line)
        if match:
            if current is not None:
                current.end = index
                modules.append(current)
            current = Module(match.group(1), index, len(lines))
            continue
        if current is None:
            continue
        pin = PIN_RE.match(line)
        if pin and pin.group(2) == "tag" and current.tag_index is None:
            current.tag_index = index
            current.tag_raw = pin.group(3)
        elif pin and pin.group(2) == "commit" and current.commit_index is None:
            current.commit_index = index
            current.commit_raw = pin.group(3)
    if current is not None:
        current.end = len(lines)
        modules.append(current)
    return modules


def format_scalar(old_raw: str, value: str) -> str:
    """Write `value` in the quoting style the pin already used.

    A bare YAML scalar like `1.49` is a float, so a version that would parse as
    one must stay quoted; the manifest already quotes exactly those.
    """
    if old_raw[:1] in ("'", '"'):
        quote = old_raw[0]
        return f"{quote}{value}{quote}"
    if re.fullmatch(r"[0-9][0-9.]*", value):
        return f"'{value}'"
    return value


def replace_value(line: str, value: str) -> str:
    match = PIN_RE.match(line)
    if match is None:
        raise BumpError(f"cannot rewrite pin line: {line!r}")
    start, end = match.span(3)
    return line[:start] + value + line[end:]


def masked(lines: Sequence[str]) -> str:
    """The manifest with every tag/commit value blanked out.

    Comparing the masked form before and after the rewrite proves that nothing
    but those values moved: same lines, same order, same comments, same
    indentation.
    """
    return "\n".join(
        PIN_RE.sub(lambda m: f"{m.group(1)}{m.group(2)}: <pin>", line) for line in lines
    )


def module_pin(lines: Sequence[str], module: Module, key: str) -> str | None:
    index = module.tag_index if key == "tag" else module.commit_index
    if index is None:
        return None
    match = PIN_RE.match(lines[index])
    return unquote(match.group(3)) if match else None


# ---------------------------------------------------------------------------
# report
# ---------------------------------------------------------------------------


@dataclass
class Result:
    component: Component
    pinned: str
    latest: str
    action: str  # keep | update | unknown
    status: str  # ok | unknown
    candidate: Candidate | None = None


def print_report(results: Sequence[Result]) -> None:
    print(
        f"{'component':<18} {'pinned':<14} {'latest eligible':<16} "
        f"{'status':<10} action"
    )
    print(
        f"{'-' * 18} {'-' * 14} {'-' * 16} {'-' * 10} {'-' * 6}"
    )
    for result in results:
        print(
            f"{result.component.name:<18} {result.pinned:<14} "
            f"{result.latest:<16} {result.status:<10} {result.action}"
        )


def print_deltas(results: Sequence[Result]) -> None:
    for result in results:
        if result.action == "update":
            print(f"{result.component.name} {result.pinned} -> {result.latest}")


def print_json(results: Sequence[Result]) -> None:
    print(
        json.dumps(
            [
                {
                    "component": result.component.name,
                    "pinned": result.pinned,
                    "latest": result.latest,
                    "action": result.action,
                    "status": result.status,
                }
                for result in results
            ]
        )
    )


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog=SCRIPT_NAME, description="Bump the Flatpak manifest's dependency pins."
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--apply", action="store_true", help="rewrite the manifest")
    mode.add_argument("--check", action="store_true", help="only report pending updates")
    parser.add_argument(
        "--component",
        action="append",
        metavar="NAME",
        help="limit the run (repeatable): " + " ".join(BY_NAME),
    )
    parser.add_argument(
        "--min-age-days",
        type=int,
        default=14,
        metavar="N",
        help="soak period in days (0 disables it; default 14)",
    )
    parser.add_argument(
        "--json", action="store_true", help="one JSON document on stdout"
    )
    parser.add_argument(
        "--manifest",
        default=str(DEFAULT_MANIFEST),
        metavar="P",
        help="manifest to operate on (for testing)",
    )
    args = parser.parse_args(argv)
    if args.min_age_days < 0:
        parser.error("--min-age-days must be non-negative")
    selected = args.component or list(BY_NAME)
    unknown = [name for name in selected if name not in BY_NAME]
    if unknown:
        parser.error(
            "unknown component(s): "
            + ", ".join(unknown)
            + " (known: "
            + " ".join(BY_NAME)
            + ")"
        )
    args.selected = selected
    return args


def resolve_all(
    args: argparse.Namespace,
    lines: Sequence[str],
    modules: dict[str, Module],
) -> list[Result]:
    results: list[Result] = []
    for name in args.selected:
        component = BY_NAME[name]
        module = modules.get(name)
        if module is None:
            raise BumpError(
                f"the manifest has no module named {name!r}; it was renamed or removed"
            )
        pinned = module_pin(lines, module, "tag")
        if not pinned:
            raise BumpError(f"module {name!r} has no tag: pin")
        pinned_core = version_core(component, pinned)
        if pinned_core is None:
            raise BumpError(
                f"module {name!r} has tag {pinned!r}, which {SCRIPT_NAME} does not"
                " understand; fix the pin or the tag pattern"
            )
        if module.commit_index is None:
            raise BumpError(f"module {name!r} has no commit: pin (Flathub requires one)")

        try:
            candidate = eligible(component, pinned_core, args.min_age_days)
        except UnknownError as exc:
            log(f"WARNING: {exc}")
            results.append(Result(component, pinned, "unknown", "unknown", "unknown"))
            continue

        if candidate is None:
            results.append(Result(component, pinned, pinned, "keep", "ok"))
        else:
            results.append(Result(component, pinned, candidate.tag, "update", "ok", candidate))
    return results


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    manifest_path = Path(args.manifest)
    if not manifest_path.is_file():
        die(f"no such manifest: {manifest_path}")

    text = manifest_path.read_text(encoding="utf-8")
    if not text.endswith("\n"):
        die(f"{manifest_path} does not end with a newline; refusing to rewrite it")
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines = lines[:-1]

    modules = {module.name: module for module in parse_modules(lines)}

    results = resolve_all(args, lines, modules)
    updates = [result for result in results if result.action == "update"]
    unknowns = [result for result in results if result.status == "unknown"]

    if args.check:
        if args.json:
            print_json(results)
        else:
            print_deltas(results)
            print(f"updates available: {len(updates)}")
        if unknowns:
            die(
                f"{len(unknowns)} component(s) could not be queried; "
                "the answer is incomplete"
            )
        return 2 if updates else 0

    if args.json:
        print_json(results)
    else:
        print_report(results)

    if not args.apply:
        if not args.json:
            print_deltas(results)
        return 0

    if unknowns:
        die(
            f"{len(unknowns)} component(s) could not be queried; refusing to write "
            f"{manifest_path} (narrow the run with --component if that upstream is down)"
        )

    if not updates:
        log(
            f"nothing to apply; every pin in {manifest_path} is already current"
        )
        return 0

    # Resolve every commit before touching the file: a commit that cannot be
    # fetched would otherwise leave a manifest that cannot be built.
    resolved: dict[str, str] = {}
    for result in updates:
        resolved[result.component.name] = peeled_commit(
            result.component, result.latest
        )

    rewritten = list(lines)
    for result in updates:
        module = modules[result.component.name]
        new_tag = format_scalar(module.tag_raw, result.latest)
        new_commit = format_scalar(module.commit_raw, resolved[result.component.name])
        assert module.tag_index is not None and module.commit_index is not None
        rewritten[module.tag_index] = replace_value(
            rewritten[module.tag_index], new_tag
        )
        rewritten[module.commit_index] = replace_value(
            rewritten[module.commit_index], new_commit
        )

    if masked(rewritten) != masked(lines):
        die(
            "the rewrite changed more than the tag/commit values; "
            f"{manifest_path} was left untouched"
        )

    # Re-parse the result: the values must be exactly what was resolved.
    reparsed = {module.name: module for module in parse_modules(rewritten)}
    for result in updates:
        module = reparsed[result.component.name]
        if module_pin(rewritten, module, "tag") != result.latest:
            die(
                f"the rewrite did not set {result.component.name} to {result.latest}; "
                f"{manifest_path} was left untouched"
            )
        if module_pin(rewritten, module, "commit") != resolved[result.component.name]:
            die(
                f"the rewrite did not set {result.component.name}'s commit; "
                f"{manifest_path} was left untouched"
            )

    fd, tmp_name = tempfile.mkstemp(
        prefix=manifest_path.name + ".", dir=str(manifest_path.parent)
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as handle:
            handle.write("\n".join(rewritten) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(tmp_name, manifest_path.stat().st_mode & 0o777)
        os.replace(tmp_name, manifest_path)
    except BaseException:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass
        raise

    for result in updates:
        log(f"{result.component.name} {result.pinned} -> {result.latest}")
    log(f"updated {len(updates)} module(s) in {manifest_path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BumpError as error:
        die(str(error))
