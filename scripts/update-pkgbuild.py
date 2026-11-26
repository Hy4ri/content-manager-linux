#!/usr/bin/env python3
"""Update pkgver, _tag, pkgrel, and sha256sums in packaging/arch/PKGBUILD."""

import os
import re
import sys


def update_pkgbuild(raw_version: str, appimage_sha256: str, license_sha256: str) -> None:
    version = raw_version.lstrip("v").strip()
    tag = f"v{version}"
    pkgver = version.replace("-", "_")

    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    path = os.path.join(repo_root, "packaging", "arch", "PKGBUILD")
    with open(path, "r", encoding="utf-8") as f:
        text = f.read()

    def replace_once(pattern: str, repl: str, src: str) -> str:
        out, count = re.subn(pattern, repl, src, count=1, flags=re.MULTILINE)
        if count != 1:
            raise RuntimeError(f"Expected 1 match for {pattern!r}, got {count}")
        return out

    text = replace_once(r"^pkgver=.*$", f"pkgver={pkgver}", text)
    text = replace_once(r"^_tag=.*$", f'_tag="{tag}"', text)
    text = replace_once(r"^pkgrel=.*$", "pkgrel=1", text)
    text = replace_once(
        r"^sha256sums=\(\n  '[0-9a-f]+'\n  '[0-9a-f]+'\n\)$",
        f"sha256sums=(\n  '{appimage_sha256}'\n  '{license_sha256}'\n)",
        text,
    )

    with open(path, "w", encoding="utf-8") as f:
        f.write(text)

    print(f"Updated {path} -> pkgver={pkgver} (_tag={tag})")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        print(
            "Usage: python3 scripts/update-pkgbuild.py <version> <appimage_sha256> <license_sha256>",
            file=sys.stderr,
        )
        sys.exit(1)
    update_pkgbuild(sys.argv[1], sys.argv[2], sys.argv[3])
