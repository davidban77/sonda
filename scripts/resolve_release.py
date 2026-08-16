#!/usr/bin/env python3
"""Resolve a sonda-action version reference to a concrete, downloadable release.

The action must not hand a moving reference to the installer. ``@v1`` is a
tag that moves with every release, ``latest`` is a query, and
``github.action_ref`` can be any of a full tag, a major tag, a branch, or a
commit SHA. All of them have to become one concrete ``vX.Y.Z`` that has
assets attached, or a named failure — never a silent wrong version.

Two failure modes matter enough to name separately:

* **The release-please window.** ``release-please`` merges the version bump
  and creates the tag *before* the binary build finishes uploading. A tag
  that exists with no assets is therefore normal and transient, not
  corruption, and the message has to say so — otherwise the next person
  reads a 404 from the installer and goes looking for a bug.
* **A reference that resolves to nothing.** A typo'd tag and a real tag
  whose release was deleted look identical from the installer's side. Both
  fail here, before anything is downloaded.

Stdlib-only, mirroring ``live_infra_uat.py``. ``--self-test`` runs the
inline unit tests with no network.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import unittest
import urllib.error
import urllib.request
from typing import Any, Iterable, Sequence

GITHUB_API = "https://api.github.com"
DEFAULT_REPO = "davidban77/sonda"
HTTP_TIMEOUT_S = 15.0

# A concrete, immutable release tag: vMAJOR.MINOR.PATCH (with optional
# pre-release/build metadata, which release-please does not currently emit
# but which must not be silently mistaken for a major tag).
FULL_TAG = re.compile(r"^v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")

# A moving major tag: v1, v2 — the shape Actions users expect to pin.
MAJOR_TAG = re.compile(r"^v(\d+)$")

# `sonda test --alertmanager-url` landed in #552, after v1.20.0 shipped. The
# action installs *released* binaries, so an older pin cannot serve that
# input — and the failure it would otherwise produce is clap's "unexpected
# argument", which reads like a bug in the action rather than a version
# floor. Bump this when the flag's release changes.
MIN_ALERTMANAGER_VERSION = (1, 21, 0)


class ResolveError(Exception):
    """A reference that cannot be turned into a downloadable release."""


def is_full_tag(ref: str) -> bool:
    """Whether ``ref`` already names one immutable release."""
    return bool(FULL_TAG.match(ref))


def major_of(ref: str) -> str | None:
    """The major number of a moving major tag (``v1`` -> ``1``), else None."""
    match = MAJOR_TAG.match(ref)
    return match.group(1) if match else None


def newest_in_major(releases: Iterable[dict[str, Any]], major: str) -> str | None:
    """Newest published release tag within one major version.

    ``releases`` is the GitHub API payload order (newest first). Drafts and
    pre-releases are skipped: pinning ``@v1`` must never select something
    the maintainer has not published.
    """
    prefix = f"v{major}."
    for release in releases:
        tag = release.get("tag_name", "")
        if not isinstance(tag, str) or not tag.startswith(prefix):
            continue
        if release.get("draft") or release.get("prerelease"):
            continue
        if is_full_tag(tag):
            return tag
    return None


def newest_published(releases: Iterable[dict[str, Any]]) -> str | None:
    """Newest published, non-pre-release tag of any major version."""
    for release in releases:
        tag = release.get("tag_name", "")
        if not isinstance(tag, str) or not is_full_tag(tag):
            continue
        if release.get("draft") or release.get("prerelease"):
            continue
        return tag
    return None


def parse_version(tag: str) -> tuple[int, int, int] | None:
    """``v1.20.0`` -> ``(1, 20, 0)``. None when the tag is not a full tag.

    Pre-release metadata is dropped for comparison: a ``v2.0.0-rc.1`` is
    treated as ``2.0.0``, which is deliberately generous — the alternative
    is refusing to run against a release candidate someone chose on purpose.
    """
    if not is_full_tag(tag):
        return None
    core = re.split(r"[-+]", tag[1:], maxsplit=1)[0]
    major, minor, patch = core.split(".")
    return (int(major), int(minor), int(patch))


def at_least(tag: str, minimum: tuple[int, int, int]) -> bool:
    """Whether ``tag`` is at or above ``minimum``.

    An unparseable tag returns True: the caller has already resolved it to
    something installable, and blocking on a shape this function does not
    recognize would fail closed against the user rather than the risk.
    """
    parsed = parse_version(tag)
    return True if parsed is None else parsed >= minimum


def asset_names(release: dict[str, Any]) -> list[str]:
    """Names of the assets attached to a release payload."""
    assets = release.get("assets")
    if not isinstance(assets, list):
        return []
    return [a.get("name", "") for a in assets if isinstance(a, dict)]


def check_downloadable(tag: str, release: dict[str, Any] | None) -> None:
    """Raise unless ``release`` is a real release with assets to download.

    # Raises

    [`ResolveError`] naming which of the two failure modes happened.
    """
    if release is None:
        raise ResolveError(
            f"no release found for {tag!r}. Check the version input, or pin a "
            f"released version: https://github.com/{DEFAULT_REPO}/releases"
        )
    names = asset_names(release)
    if not names:
        raise ResolveError(
            f"release {tag} exists but has no assets attached yet. This is "
            "normal for a few minutes after a release: the tag is created "
            "when the version bump merges, and the binaries upload once the "
            "build finishes. Re-run this job, or pin the previous version."
        )
    if "SHA256SUMS" not in names:
        raise ResolveError(
            f"release {tag} has assets but no SHA256SUMS, so the download "
            "cannot be integrity-checked. Refusing to install an unverified "
            f"binary. Assets present: {', '.join(sorted(names))}"
        )


def _api_get(url: str, token: str | None) -> Any:
    request = urllib.request.Request(url)
    request.add_header("Accept", "application/vnd.github+json")
    request.add_header("User-Agent", "sonda-action")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request, timeout=HTTP_TIMEOUT_S) as response:
        return json.loads(response.read().decode("utf-8"))


def resolve(
    ref: str,
    repo: str,
    token: str | None,
    fetch=None,
) -> str:
    """Turn ``ref`` into a concrete release tag with downloadable assets.

    ``fetch(path)`` is injected in tests; it defaults to the GitHub API.

    # Raises

    [`ResolveError`] when the reference names nothing installable.
    """
    get = fetch or (lambda path: _api_get(f"{GITHUB_API}{path}", token))
    ref = ref.strip()
    if not ref:
        raise ResolveError("empty version reference")

    if is_full_tag(ref):
        tag = ref
    elif ref == "latest":
        tag = newest_published(get(f"/repos/{repo}/releases"))
        if tag is None:
            raise ResolveError(f"{repo} has no published releases to resolve 'latest' to")
    elif (major := major_of(ref)) is not None:
        tag = newest_in_major(get(f"/repos/{repo}/releases"), major)
        if tag is None:
            raise ResolveError(
                f"no published release found for major version {ref}. "
                f"Pin a concrete version instead: "
                f"https://github.com/{repo}/releases"
            )
    else:
        # A branch or a commit SHA: the action is being used from a ref that
        # is not a release at all (`uses: owner/repo@main`, or `uses: ./`).
        # There is no honest mapping from that to a release, so fall back to
        # the newest published one and say which was chosen.
        tag = newest_published(get(f"/repos/{repo}/releases"))
        if tag is None:
            raise ResolveError(
                f"{ref!r} is not a release tag and {repo} has no published "
                "releases to fall back to"
            )
        print(
            f"note: {ref!r} is not a release tag, so the newest published "
            f"release ({tag}) will be installed",
            file=sys.stderr,
        )

    release: dict[str, Any] | None
    try:
        release = get(f"/repos/{repo}/releases/tags/{tag}")
    except urllib.error.HTTPError as e:
        if e.code == 404:
            release = None
        else:
            raise ResolveError(f"GitHub API error resolving {tag}: {e}") from e
    check_downloadable(tag, release)
    return tag


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Resolve a sonda-action version reference to a concrete release tag.",
    )
    parser.add_argument("--ref", default="", help="Version input, or github.action_ref.")
    parser.add_argument("--repo", default=DEFAULT_REPO, help="owner/repo to resolve against.")
    parser.add_argument(
        "--needs-alertmanager",
        action="store_true",
        help=(
            "Fail if the resolved release predates `sonda test "
            "--alertmanager-url`, instead of installing a binary that will "
            "reject the flag."
        ),
    )
    parser.add_argument(
        "--self-test", action="store_true", help="Run inline unit tests and exit. No network."
    )
    args = parser.parse_args(argv)

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    try:
        tag = resolve(args.ref, args.repo, os.environ.get("GITHUB_TOKEN"))
        if args.needs_alertmanager and not at_least(tag, MIN_ALERTMANAGER_VERSION):
            floor = "v" + ".".join(str(p) for p in MIN_ALERTMANAGER_VERSION)
            raise ResolveError(
                f"the alertmanager-url input needs sonda {floor} or newer, but "
                f"this run resolved to {tag}. `sonda test --alertmanager-url` "
                f"does not exist in {tag}, so the run would fail with an "
                "'unexpected argument' error that looks like an action bug. "
                f"Pin version: {floor} once it is released, or use "
                "prometheus-url."
            )
        print(tag)
    except ResolveError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as e:
        print(f"error: could not reach the GitHub release API: {e}", file=sys.stderr)
        return 1
    return 0


# --- Inline tests ------------------------------------------------------------


def _release(tag: str, assets: Sequence[str] = ("SHA256SUMS",), **kw: Any) -> dict[str, Any]:
    return {
        "tag_name": tag,
        "assets": [{"name": n} for n in assets],
        "draft": kw.get("draft", False),
        "prerelease": kw.get("prerelease", False),
    }


class TagShapeTests(unittest.TestCase):
    def test_full_tags_are_recognized(self):
        for tag in ("v1.20.0", "v0.1.0", "v10.2.30", "v2.0.0-rc.1"):
            self.assertTrue(is_full_tag(tag), tag)

    def test_non_full_tags_are_rejected(self):
        # `v1` is the case that matters: treating it as concrete would pin
        # every user of @v1 to a tag that moves under them.
        for tag in ("v1", "v", "1.2.3", "main", "latest", "vX.Y.Z", ""):
            self.assertFalse(is_full_tag(tag), tag)

    def test_major_tags(self):
        self.assertEqual(major_of("v1"), "1")
        self.assertEqual(major_of("v22"), "22")
        for tag in ("v1.2.3", "main", "v", ""):
            self.assertIsNone(major_of(tag), tag)


class VersionFloorTests(unittest.TestCase):
    def test_parses_full_tags(self):
        self.assertEqual(parse_version("v1.20.0"), (1, 20, 0))
        self.assertEqual(parse_version("v10.2.30"), (10, 2, 30))
        self.assertEqual(parse_version("v2.0.0-rc.1"), (2, 0, 0))

    def test_non_full_tags_have_no_version(self):
        for tag in ("v1", "main", "", "1.2.3"):
            self.assertIsNone(parse_version(tag), tag)

    def test_ordering_is_numeric_not_lexical(self):
        # "v1.9.0" > "v1.20.0" as strings; the floor must not agree.
        self.assertTrue(at_least("v1.20.0", (1, 9, 0)))
        self.assertFalse(at_least("v1.9.0", (1, 20, 0)))

    def test_the_alertmanager_floor(self):
        self.assertFalse(at_least("v1.20.0", MIN_ALERTMANAGER_VERSION))
        self.assertTrue(at_least("v1.21.0", MIN_ALERTMANAGER_VERSION))
        self.assertTrue(at_least("v2.0.0", MIN_ALERTMANAGER_VERSION))

    def test_equality_satisfies_the_floor(self):
        self.assertTrue(at_least("v1.21.0", (1, 21, 0)))

    def test_an_unrecognized_shape_does_not_block(self):
        # Fail open against the user, not closed: the tag already resolved
        # to something installable.
        self.assertTrue(at_least("weird", (1, 21, 0)))


class SelectionTests(unittest.TestCase):
    def test_newest_in_major_respects_the_major(self):
        releases = [_release("v2.0.0"), _release("v1.20.0"), _release("v1.19.0")]
        self.assertEqual(newest_in_major(releases, "1"), "v1.20.0")
        self.assertEqual(newest_in_major(releases, "2"), "v2.0.0")

    def test_major_prefix_is_not_a_substring_match(self):
        # v11.x must never satisfy @v1.
        releases = [_release("v11.0.0"), _release("v1.5.0")]
        self.assertEqual(newest_in_major(releases, "1"), "v1.5.0")

    def test_drafts_and_prereleases_are_skipped(self):
        releases = [
            _release("v1.21.0", draft=True),
            _release("v1.20.1", prerelease=True),
            _release("v1.20.0"),
        ]
        self.assertEqual(newest_in_major(releases, "1"), "v1.20.0")
        self.assertEqual(newest_published(releases), "v1.20.0")

    def test_no_match_returns_none(self):
        self.assertIsNone(newest_in_major([_release("v2.0.0")], "3"))
        self.assertIsNone(newest_published([]))


class DownloadableTests(unittest.TestCase):
    def test_a_release_with_checksums_is_downloadable(self):
        check_downloadable("v1.20.0", _release("v1.20.0", ["SHA256SUMS", "sonda-x.tar.gz"]))

    def test_missing_release_is_named(self):
        with self.assertRaises(ResolveError) as ctx:
            check_downloadable("v9.9.9", None)
        self.assertIn("no release found", str(ctx.exception))

    def test_the_release_please_window_is_named_as_transient(self):
        # The tag exists before the binaries upload. A bare 404 from the
        # installer sends the reader hunting for a bug that is not there.
        with self.assertRaises(ResolveError) as ctx:
            check_downloadable("v1.21.0", _release("v1.21.0", []))
        message = str(ctx.exception)
        self.assertIn("no assets attached yet", message)
        self.assertIn("Re-run this job", message)

    def test_assets_without_checksums_are_refused(self):
        with self.assertRaises(ResolveError) as ctx:
            check_downloadable("v1.20.0", _release("v1.20.0", ["sonda-x.tar.gz"]))
        self.assertIn("no SHA256SUMS", str(ctx.exception))


class ResolveTests(unittest.TestCase):
    def _fetch(self, releases: Sequence[dict[str, Any]]):
        by_tag = {r["tag_name"]: r for r in releases}

        def fetch(path: str):
            if path.endswith("/releases"):
                return list(releases)
            tag = path.rsplit("/", 1)[-1]
            if tag not in by_tag:
                raise urllib.error.HTTPError(path, 404, "Not Found", {}, None)  # type: ignore[arg-type]
            return by_tag[tag]

        return fetch

    def test_a_full_tag_passes_through(self):
        releases = [_release("v1.20.0")]
        self.assertEqual(resolve("v1.20.0", "o/r", None, self._fetch(releases)), "v1.20.0")

    def test_a_major_tag_resolves_within_its_major(self):
        releases = [_release("v2.0.0"), _release("v1.20.0")]
        self.assertEqual(resolve("v1", "o/r", None, self._fetch(releases)), "v1.20.0")

    def test_latest_resolves_to_the_newest_published(self):
        releases = [_release("v2.0.0"), _release("v1.20.0")]
        self.assertEqual(resolve("latest", "o/r", None, self._fetch(releases)), "v2.0.0")

    def test_a_branch_or_sha_falls_back_to_newest(self):
        releases = [_release("v1.20.0")]
        for ref in ("main", "0c68373", "claude/some-branch"):
            self.assertEqual(resolve(ref, "o/r", None, self._fetch(releases)), "v1.20.0")

    def test_a_tag_with_no_assets_fails_rather_than_installing(self):
        releases = [_release("v1.21.0", [])]
        with self.assertRaises(ResolveError) as ctx:
            resolve("v1.21.0", "o/r", None, self._fetch(releases))
        self.assertIn("no assets attached yet", str(ctx.exception))

    def test_an_unknown_full_tag_fails(self):
        with self.assertRaises(ResolveError) as ctx:
            resolve("v9.9.9", "o/r", None, self._fetch([_release("v1.20.0")]))
        self.assertIn("no release found", str(ctx.exception))

    def test_an_empty_ref_fails(self):
        with self.assertRaises(ResolveError):
            resolve("   ", "o/r", None, self._fetch([]))


if __name__ == "__main__":
    raise SystemExit(main())
