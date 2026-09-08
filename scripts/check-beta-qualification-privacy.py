#!/usr/bin/env python3
"""Run the closed beta qualification root privacy and overclaim review."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path

ROOT_FILES = {
    "Cargo.lock",
    "Cargo.toml",
    "README.md",
    "SECURITY.md",
    "rust-toolchain.toml",
}
ROOT_PREFIXES = (
    "crates/kapsel-authority/",
    "docs/",
    "scripts/",
    "src/",
    "tests/",
    "vectors/",
)
PRIVATE_PATHS = (
    re.compile(rb"/Users/[^\s\x00]+"),
    re.compile(rb"/private/var/[^\s\x00]+"),
)
CREDENTIAL_PATTERNS = (
    re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"),
    re.compile(rb"AKIA[0-9A-Z]{16}"),
    re.compile(rb"gh[pousr]_[A-Za-z0-9]{30,}"),
)
AFFIRMATIVE_OVERCLAIMS = (
    re.compile(r"\bis production[- ]ready\b", re.IGNORECASE),
    re.compile(r"\bguarantees? exactly[- ]once\b", re.IGNORECASE),
    re.compile(r"\bprovides? (?:a )?production[- ]support SLA\b", re.IGNORECASE),
    re.compile(r"\bsupports? (?:all|every) Kubernetes(?: distribution)?\b", re.IGNORECASE),
    re.compile(r"\bestablishes? native[- ]host performance\b", re.IGNORECASE),
)
PRIVATE_ARTIFACT_SUFFIXES = (".key", ".kubeconfig", ".pem", ".receipt", ".seed", ".sqlite3")
PATTERN_FIXTURE_FILES = {
    "scripts/check-beta-qualification-privacy.py",
    "scripts/test-beta-qualification.py",
    "scripts/validate-beta-qualification-baseline.py",
}


def tracked_paths(root: Path) -> list[str]:
    paths = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
        cwd=root,
        text=True,
    ).splitlines()
    selected = [
        path
        for path in paths
        if (path in ROOT_FILES or path.startswith(ROOT_PREFIXES)) and (root / path).is_file()
    ]
    return sorted(selected)


def fail(category: str, path: str, line: int | None = None) -> None:
    location = path if line is None else f"{path}:{line}"
    raise RuntimeError(f"{category} finding at {location}")


def validate(root: Path, paths: list[str]) -> str:
    digest = hashlib.sha256()
    for relative in paths:
        path = root / relative
        if relative.endswith(PRIVATE_ARTIFACT_SUFFIXES):
            fail("private artifact", relative)
        data = path.read_bytes()
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update(data)
        digest.update(b"\0")
        if relative not in PATTERN_FIXTURE_FILES:
            for pattern in PRIVATE_PATHS:
                if pattern.search(data):
                    fail("absolute private path", relative)
            for pattern in CREDENTIAL_PATTERNS:
                if pattern.search(data):
                    fail("credential material", relative)
        if path.suffix == ".md":
            text = data.decode("utf-8")
            for line_number, line in enumerate(text.splitlines(), start=1):
                for pattern in AFFIRMATIVE_OVERCLAIMS:
                    if pattern.search(line):
                        fail("unsupported claim", relative, line_number)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    paths = tracked_paths(root)
    digest = validate(root, paths)
    result = {
        "schema_version": 1,
        "checked_file_count": len(paths),
        "checked_source_sha256": digest,
        "checks": [
            "no-absolute-private-path",
            "no-credential-material",
            "no-private-artifact",
            "no-production-sla-overclaim",
        ],
        "status": "passed",
    }
    arguments.output.write_text(json.dumps(result, sort_keys=True, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
