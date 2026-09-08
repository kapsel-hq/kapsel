#!/usr/bin/env python3
"""Fail when a repository Markdown file has a broken local path or heading anchor."""

from __future__ import annotations

import html
import re
import subprocess
import sys
import unicodedata
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent.parent
ATX_HEADING = re.compile(r"^ {0,3}(#{1,6})(?:[ \t]+|$)(.*?)[ \t]*#*[ \t]*$")
SETEXT_HEADING = re.compile(r"^ {0,3}(=+|-+)[ \t]*$")
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})(.*)$")
FENCE_CLOSE = re.compile(r"^ {0,3}(`+|~+)[ \t]*$")
EXPLICIT_ANCHOR = re.compile(r'<(?:a|span)\b[^>]*\b(?:name|id)=["\']([^"\']+)["\']')
REFERENCE_DEFINITION = re.compile(r"^ {0,3}\[([^\]]+)\]:[ \t]*(.*)$")
REFERENCE_LINK = re.compile(r"!?\[([^\]]+)\]\[([^\]]*)\]")
SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")


@dataclass(frozen=True)
class LinkTarget:
    """One parsed Markdown destination and its source line."""

    target: str
    line_number: int


def repository_markdown() -> list[Path]:
    """Return tracked and untracked Markdown paths under the repository root."""

    try:
        output = subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "*.md"],
            cwd=ROOT,
            text=True,
            stderr=subprocess.DEVNULL,
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        return sorted(
            path for path in ROOT.rglob("*.md") if "target" not in path.relative_to(ROOT).parts
        )
    paths = [ROOT / line for line in output.splitlines() if line and (ROOT / line).is_file()]
    if paths:
        return paths
    return sorted(
        path for path in ROOT.rglob("*.md") if "target" not in path.relative_to(ROOT).parts
    )


def markdown_without_fences(path: Path) -> list[str]:
    """Blank fenced content while preserving source line numbers."""

    lines = path.read_text(encoding="utf-8").splitlines()
    visible: list[str] = []
    fence_character: str | None = None
    fence_length = 0
    for line in lines:
        if fence_character is None:
            opening = FENCE.match(line)
            if opening is not None:
                marker, info = opening.groups()
                if marker[0] != "`" or "`" not in info:
                    fence_character = marker[0]
                    fence_length = len(marker)
                    visible.append("")
                    continue
            visible.append(line)
            continue

        closing = FENCE_CLOSE.match(line)
        if (
            closing is not None
            and closing.group(1)[0] == fence_character
            and len(closing.group(1)) >= fence_length
        ):
            fence_character = None
            fence_length = 0
        visible.append("")
    return visible


def github_slug(heading: str) -> str:
    """Approximate GitHub's Unicode-aware heading base slug."""

    heading = html.unescape(heading)
    heading = re.sub(r"<[^>]+>", "", heading)
    heading = re.sub(r"!?\[([^\]]+)\]\([^)]+\)", r"\1", heading)
    heading = heading.replace("`", "").strip().lower()
    characters: list[str] = []
    for character in heading:
        category = unicodedata.category(character)
        if character in {"-", "_", " "} or not category.startswith(("P", "S")):
            characters.append(character)
    return "".join(characters).replace(" ", "-")


def anchors(path: Path) -> set[str]:
    """Return explicit and GitHub-style generated anchors for one file."""

    lines = markdown_without_fences(path)
    found: set[str] = set()
    used_slugs: set[str] = set()

    def add_heading(heading: str) -> None:
        base = github_slug(heading)
        candidate = base
        suffix = 0
        while candidate in used_slugs:
            suffix += 1
            candidate = f"{base}-{suffix}"
        used_slugs.add(candidate)
        found.add(candidate)

    for index, line in enumerate(lines):
        for explicit in EXPLICIT_ANCHOR.findall(line):
            found.add(explicit)
        atx = ATX_HEADING.match(line)
        if atx is not None:
            add_heading(atx.group(2))
            continue
        if (
            line.strip()
            and index + 1 < len(lines)
            and SETEXT_HEADING.match(lines[index + 1]) is not None
        ):
            add_heading(line.strip())
    return found


def normalize_reference(label: str) -> str:
    """Normalize a CommonMark reference label."""

    return " ".join(label.split()).casefold()


def parse_destination(text: str) -> str | None:
    """Parse the destination prefix from an inline body or reference definition."""

    text = text.strip()
    if not text:
        return ""
    if text.startswith("<"):
        escaped = False
        for index, character in enumerate(text[1:], start=1):
            if character == ">" and not escaped:
                return text[1:index]
            escaped = character == "\\" and not escaped
            if character != "\\":
                escaped = False
        return None

    depth = 0
    escaped = False
    destination: list[str] = []
    for character in text:
        if escaped:
            destination.append(character)
            escaped = False
            continue
        if character == "\\":
            escaped = True
            continue
        if character in " \t" and depth == 0:
            break
        if character == "(":
            depth += 1
        elif character == ")":
            if depth == 0:
                break
            depth -= 1
        destination.append(character)
    if escaped or depth != 0:
        return None
    return "".join(destination)


def inline_destinations(line: str) -> list[str]:
    """Return balanced inline-link destinations from one visible line."""

    destinations: list[str] = []
    cursor = 0
    while cursor < len(line):
        close_label = line.find("](", cursor)
        if close_label < 0:
            break
        open_label = line.rfind("[", 0, close_label)
        if open_label < 0 or (open_label > 0 and line[open_label - 1] == "\\"):
            cursor = close_label + 2
            continue
        body_start = close_label + 2
        depth = 1
        escaped = False
        index = body_start
        while index < len(line):
            character = line[index]
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == "(":
                depth += 1
            elif character == ")":
                depth -= 1
                if depth == 0:
                    target = parse_destination(line[body_start:index])
                    if target is not None:
                        destinations.append(target)
                    cursor = index + 1
                    break
            index += 1
        else:
            break
    return destinations


def link_targets(lines: list[str]) -> tuple[list[LinkTarget], list[tuple[int, str]]]:
    """Parse inline and reference destinations plus missing reference labels."""

    definitions: dict[str, LinkTarget] = {}
    definition_lines: set[int] = set()
    for line_number, line in enumerate(lines, start=1):
        match = REFERENCE_DEFINITION.match(line)
        if match is None:
            continue
        destination = parse_destination(match.group(2))
        if destination is not None:
            definitions.setdefault(
                normalize_reference(match.group(1)),
                LinkTarget(destination, line_number),
            )
        definition_lines.add(line_number)

    targets = list(definitions.values())
    missing_references: list[tuple[int, str]] = []
    for line_number, line in enumerate(lines, start=1):
        if line_number in definition_lines:
            continue
        targets.extend(
            LinkTarget(destination, line_number) for destination in inline_destinations(line)
        )
        for match in REFERENCE_LINK.finditer(line):
            label = match.group(2) or match.group(1)
            normalized = normalize_reference(label)
            if normalized not in definitions:
                missing_references.append((line_number, label))
    return targets, missing_references


def check_target(
    source: Path,
    link: LinkTarget,
    anchor_cache: dict[Path, set[str]],
) -> str | None:
    """Validate one local target and return an error description when invalid."""

    target = link.target.strip()
    if not target or SCHEME.match(target) or target.startswith("//"):
        return None
    split = urlsplit(target)
    path_part = unquote(split.path)
    fragment = unquote(split.fragment)
    if path_part:
        if path_part.startswith("/"):
            return f"absolute local path {target}"
        resolved = (source.parent / path_part).resolve()
    else:
        resolved = source.resolve()
    try:
        resolved.relative_to(ROOT)
    except ValueError:
        return f"path escapes repository {target}"
    if not resolved.exists():
        return f"missing path {target}"
    if fragment and resolved.suffix.lower() == ".md":
        target_anchors = anchor_cache.setdefault(resolved, anchors(resolved))
        if fragment not in target_anchors:
            return f"missing anchor {target}"
    return None


def main() -> int:
    """Check all tracked Markdown and return a process status."""

    failures: list[str] = []
    markdown = repository_markdown()
    anchor_cache: dict[Path, set[str]] = {}
    for source in markdown:
        lines = markdown_without_fences(source)
        targets, missing_references = link_targets(lines)
        for line_number, label in missing_references:
            failures.append(
                f"{source.relative_to(ROOT)}:{line_number}: missing reference definition [{label}]"
            )
        for link in targets:
            failure = check_target(source, link, anchor_cache)
            if failure is not None:
                failures.append(f"{source.relative_to(ROOT)}:{link.line_number}: {failure}")
    if failures:
        print("broken local Markdown links:", file=sys.stderr)
        for failure in sorted(failures):
            print(f"  {failure}", file=sys.stderr)
        return 1
    print(f"local Markdown links: ok ({len(markdown)} tracked files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
