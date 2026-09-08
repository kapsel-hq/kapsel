#!/usr/bin/env python3
"""Regression tests for the release SBOM vulnerability scanner."""

from __future__ import annotations

import importlib.util
import json
import os
import pathlib
import tempfile
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "scan_release_sbom",
    ROOT / "scripts" / "scan-release-sbom.py",
)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load the release SBOM scanner")
SCANNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SCANNER)


FAKE_TRIVY = r"""#!/usr/bin/env python3
import datetime
import json
import os
import pathlib
import sys

home = pathlib.Path(os.environ["HOME"])
arguments = sys.argv[1:]
cache = pathlib.Path(arguments[arguments.index("--cache-dir") + 1]) if "--cache-dir" in arguments else home / ".cache" / "trivy"
database = cache / "db" / "trivy.db"
if arguments and arguments[0] == "filesystem" and "--download-db-only" in arguments:
    database.parent.mkdir(parents=True, exist_ok=True)
    database.write_bytes(b"fresh-database")
elif "--version" in arguments and "--format" in arguments:
    updated = datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z")
    print(json.dumps({
        "Version": "0.72.0",
        "VulnerabilityDB": {"Version": 2, "UpdatedAt": updated},
    }))
elif arguments and arguments[0] == "sbom":
    if os.environ.get("FAKE_TRIVY_MUTATE") == "1":
        database.write_bytes(b"changed-database")
    severity = os.environ.get("FAKE_TRIVY_SEVERITY")
    vulnerabilities = [] if not severity else [{
        "VulnerabilityID": "CVE-TEST-1",
        "PkgName": "example",
        "InstalledVersion": "1.0.0",
        "FixedVersion": "1.0.1",
        "Severity": severity,
    }]
    output = pathlib.Path(arguments[arguments.index("--output") + 1])
    output.write_text(json.dumps({"Results": [{"Vulnerabilities": vulnerabilities}]}))
else:
    raise SystemExit(f"unexpected fake Trivy arguments: {arguments}")
"""


class ReleaseSbomScannerTests(unittest.TestCase):
    def fixture(
        self,
    ) -> tuple[tempfile.TemporaryDirectory[str], pathlib.Path, pathlib.Path, dict[str, str]]:
        temporary = tempfile.TemporaryDirectory(prefix="kapsel-sbom-scan-test-")
        root = pathlib.Path(temporary.name)
        binary_directory = root / "bin"
        binary_directory.mkdir()
        trivy = binary_directory / "trivy"
        trivy.write_text(FAKE_TRIVY)
        trivy.chmod(0o755)
        sbom = root / "candidate.spdx.json"
        sbom.write_text("{}\n")
        output = root / "summary.json"
        environment = {
            "HOME": str(root / "home"),
            "PATH": f"{binary_directory}:{os.environ.get('PATH', '')}",
        }
        return temporary, sbom, output, environment

    def test_fresh_atomic_scan_records_database_and_sbom_identity(self) -> None:
        temporary, sbom, output, environment = self.fixture()
        with temporary, mock.patch.dict(os.environ, environment, clear=True):
            SCANNER.scan(sbom, output)
            summary = json.loads(output.read_text())
        self.assertEqual(summary["status"], "passed")
        self.assertEqual(summary["finding_count"], 0)
        self.assertEqual(len(summary["database_sha256"]), 64)
        self.assertEqual(len(summary["sbom_sha256"]), 64)

    def test_high_finding_is_retained_and_rejected(self) -> None:
        temporary, sbom, output, environment = self.fixture()
        environment["FAKE_TRIVY_SEVERITY"] = "HIGH"
        with temporary, mock.patch.dict(os.environ, environment, clear=True):
            with self.assertRaises(RuntimeError):
                SCANNER.scan(sbom, output)
            summary = json.loads(output.read_text())
        self.assertEqual(summary["status"], "failed")
        self.assertEqual(summary["findings"][0]["id"], "CVE-TEST-1")

    def test_database_change_during_no_update_scan_is_rejected(self) -> None:
        temporary, sbom, output, environment = self.fixture()
        environment["FAKE_TRIVY_MUTATE"] = "1"
        with temporary, mock.patch.dict(os.environ, environment, clear=True):
            with self.assertRaisesRegex(RuntimeError, "database identity changed"):
                SCANNER.scan(sbom, output)
        self.assertFalse(output.exists())

    def test_oversized_sbom_is_rejected_before_trivy_refresh(self) -> None:
        temporary, sbom, output, environment = self.fixture()
        with temporary, mock.patch.dict(os.environ, environment, clear=True):
            with sbom.open("wb") as document:
                document.truncate(2 * 1024 * 1024 + 1)
            with self.assertRaisesRegex(RuntimeError, "bounded regular file"):
                SCANNER.scan(sbom, output)
            self.assertFalse(pathlib.Path(environment["HOME"]).exists())

    def test_sbom_symlink_is_rejected_without_resolving_final_component(self) -> None:
        temporary, sbom, output, environment = self.fixture()
        with temporary, mock.patch.dict(os.environ, environment, clear=True):
            target = sbom.with_name("target.spdx.json")
            sbom.replace(target)
            sbom.symlink_to(target)
            with self.assertRaises(OSError):
                SCANNER.scan(sbom, output)


if __name__ == "__main__":
    unittest.main()
