#!/usr/bin/env python3

"""Docs hygiene: relative Markdown links resolve, en/ja SUMMARies match.

Relative `.md` links must resolve to files in the checkout so they render
on GitHub and in editors; cross-area references should use absolute URLs.
`book/en` and `book/ja` SUMMARies must list the same pages in the same
order (titles differ by design, targets must not).
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
LINK_RE = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
SKIP_DIRS = {"target", "dist", ".git", "node_modules"}


def md_files() -> list[pathlib.Path]:
    out = []
    for md in sorted(ROOT.rglob("*.md")):
        if any(p in md.parts for p in SKIP_DIRS):
            continue
        out.append(md)
    return out


def is_external(url: str) -> bool:
    return url.startswith(("http://", "https://", "mailto:", "#")) or not url


def check_relative_links() -> list[str]:
    bad = []
    for md in md_files():
        for match in LINK_RE.finditer(md.read_text()):
            url = match.group(1).strip()
            if is_external(url):
                continue
            path = url.split("#")[0]
            if not path.endswith(".md"):
                continue
            if not (md.parent / path).is_file():
                bad.append(f"{md.relative_to(ROOT)}: {url}")
    return bad


def summary_targets(summary: pathlib.Path) -> list[str]:
    targets = []
    for match in LINK_RE.finditer(summary.read_text()):
        url = match.group(1).strip()
        if url.endswith(".md"):
            targets.append(url)
    return targets


def check_summary_parity() -> list[str]:
    en = ROOT / "book/en/src/SUMMARY.md"
    ja = ROOT / "book/ja/src/SUMMARY.md"
    if en.read_text() == ja.read_text():
        return []
    if summary_targets(en) == summary_targets(ja):
        return []
    return [f"{en.relative_to(ROOT)} and {ja.relative_to(ROOT)} list different pages"]


def main() -> int:
    errors = check_relative_links() + check_summary_parity()
    if errors:
        print("\n".join(errors))
        return 1
    print(f"docs ok ({len(md_files())} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
