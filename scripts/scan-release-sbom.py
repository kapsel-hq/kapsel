#!/usr/bin/env python3
"""Scan one exact release SBOM with the frozen fresh Trivy policy."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tempfile

TRIVY_VERSION = "0.72.0"
DATABASE_MAX_AGE = datetime.timedelta(hours=24)
OUTPUT_BYTES_MAX = 1024 * 1024


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_bounded_regular(path: pathlib.Path, maximum: int) -> bytes:
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise RuntimeError("release SBOM is not a bounded regular file")
        value = source.read(maximum + 1)
    if len(value) > maximum:
        raise RuntimeError("release SBOM exceeded its byte bound")
    return value


def write_exclusive(path: pathlib.Path, value: bytes) -> None:
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o644,
    )
    with os.fdopen(descriptor, "wb") as output:
        output.write(value)


def parse_utc(value: str) -> datetime.datetime:
    parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise RuntimeError("Trivy database time has no timezone")
    return parsed.astimezone(datetime.timezone.utc)


def scan(sbom: pathlib.Path, output: pathlib.Path) -> None:
    if shutil.which("trivy") is None:
        raise RuntimeError("Trivy is required for release SBOM scanning")
    tool = json.loads(
        subprocess.run(
            ["trivy", "--version", "--format", "json"],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        ).stdout
    )
    if tool.get("Version") != TRIVY_VERSION:
        raise RuntimeError(f"release SBOM scan requires exact Trivy {TRIVY_VERSION}")
    sbom_bytes = read_bounded_regular(sbom, 2 * 1024 * 1024)

    with tempfile.TemporaryDirectory(prefix="kapsel-release-sbom-scan-") as temporary:
        private = pathlib.Path(temporary)
        private.chmod(0o700)
        cache = private / "cache"
        snapshot = private / "candidate.spdx.json"
        snapshot.write_bytes(sbom_bytes)
        snapshot.chmod(0o600)
        subprocess.run(
            [
                "trivy",
                "filesystem",
                "--cache-dir",
                str(cache),
                "--download-db-only",
                str(private),
            ],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        version = json.loads(
            subprocess.run(
                ["trivy", "--cache-dir", str(cache), "--version", "--format", "json"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout
        )
        database = version.get("VulnerabilityDB")
        if version.get("Version") != TRIVY_VERSION:
            raise RuntimeError("Trivy identity changed after database refresh")
        if not isinstance(database, dict) or database.get("Version") != 2:
            raise RuntimeError("Trivy vulnerability database identity is unavailable")
        updated = parse_utc(str(database.get("UpdatedAt")))
        now = datetime.datetime.now(datetime.timezone.utc)
        if updated > now or now - updated > DATABASE_MAX_AGE:
            raise RuntimeError("Trivy vulnerability database is unavailable or older than 24 hours")
        database_path = cache / "db" / "trivy.db"
        if not database_path.is_file():
            raise RuntimeError("private Trivy vulnerability database file is unavailable")
        database_sha256 = sha256(database_path)
        raw = private / "trivy.json"
        subprocess.run(
            [
                "trivy",
                "sbom",
                "--cache-dir",
                str(cache),
                "--scanners",
                "vuln",
                "--skip-db-update",
                "--format",
                "json",
                "--output",
                str(raw),
                str(snapshot),
            ],
            check=True,
        )
        report = json.loads(raw.read_text())
        if sha256(database_path) != database_sha256:
            raise RuntimeError("Trivy vulnerability database identity changed during the scan")

        findings = []
        for result in report.get("Results") or []:
            for finding in result.get("Vulnerabilities") or []:
                findings.append(
                    {
                        "id": finding.get("VulnerabilityID"),
                        "package": finding.get("PkgName"),
                        "installed_version": finding.get("InstalledVersion"),
                        "fixed_version": finding.get("FixedVersion") or None,
                        "severity": finding.get("Severity"),
                    }
                )
        findings.sort(
            key=lambda finding: (
                str(finding["severity"]),
                str(finding["id"]),
                str(finding["package"]),
            )
        )
        blocked = [finding for finding in findings if finding["severity"] in {"HIGH", "CRITICAL"}]
        summary = {
            "schema": "kapsel.release-sbom-scan.v1",
            "sbom_sha256": hashlib.sha256(sbom_bytes).hexdigest(),
            "trivy_version": version["Version"],
            "database_version": database["Version"],
            "database_updated_utc": updated.isoformat().replace("+00:00", "Z"),
            "database_sha256": database_sha256,
            "scanned_utc": now.isoformat().replace("+00:00", "Z"),
            "finding_count": len(findings),
            "findings": findings,
            "status": "failed" if blocked else "passed",
        }
    encoded = (json.dumps(summary, indent=2, separators=(",", ": ")) + "\n").encode()
    if len(encoded) > OUTPUT_BYTES_MAX:
        raise RuntimeError("release SBOM vulnerability summary exceeded its byte bound")
    write_exclusive(output, encoded)
    if blocked:
        raise RuntimeError("release SBOM has a detected HIGH or CRITICAL vulnerability")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sbom", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    arguments = parser.parse_args()
    try:
        sbom = pathlib.Path(os.path.abspath(arguments.sbom))
        output = pathlib.Path(os.path.abspath(arguments.output))
        scan(sbom, output)
    except (OSError, RuntimeError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"Kapsel release SBOM scan failed: {error}", file=sys.stderr)
        return 1
    print(arguments.output.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
