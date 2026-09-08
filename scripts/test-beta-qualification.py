#!/usr/bin/env python3
"""Regression tests for beta qualification and its privacy review."""

import hashlib
import runpy
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORCHESTRATOR = runpy.run_path(str(ROOT / "scripts/run-beta-qualification.py"))
CHECKER_PATH = ROOT / "scripts/check-beta-qualification-privacy.py"
CHECKER = runpy.run_path(str(CHECKER_PATH))
VALIDATOR = runpy.run_path(str(ROOT / "scripts/validate-beta-qualification-baseline.py"))


class BetaQualificationTests(unittest.TestCase):
    def test_source_scopes_match(self) -> None:
        paths = [
            "Cargo.lock",
            "crates/kapsel-authority/src/lib.rs",
            "scripts/format.sh",
            "scripts/test-demo-harness.sh",
            "scripts/test-fuzz.sh",
            "scripts/test-simulation.sh",
            "unrelated",
        ]
        selected = ORCHESTRATOR["selected_source_paths"](paths)
        self.assertEqual(selected, VALIDATOR["selected_baseline_source_paths"](paths))
        self.assertEqual(len(selected), len(paths) - 1)

    def test_bounded_output_placeholder_does_not_change_recorded_command(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "bounded.json"
            command = [
                "python3",
                "-c",
                "from pathlib import Path; import sys; Path(sys.argv[1]).write_text('bounded')",
                "BOUNDED_OUTPUT",
            ]
            result = ORCHESTRATOR["run_lane"]("placeholder regression", command, ROOT, 30, output)
            self.assertEqual(result["command"], command)
            self.assertEqual(output.read_text(), "bounded")
            self.assertEqual(
                result["bounded_output_sha256"], hashlib.sha256(b"bounded").hexdigest()
            )

    def test_kind_version_selects_semver_not_platform(self) -> None:
        parse = ORCHESTRATOR["parse_kind_version"]
        self.assertEqual(parse("kind v0.32.0 go1.26.3 darwin/arm64"), "0.32.0")
        with self.assertRaises(RuntimeError):
            parse("darwin/arm64")

    def test_security_finding_severity_policy(self) -> None:
        retain = ORCHESTRATOR["retained_security_findings"]
        self.assertEqual(retain({"trivy": {"findings": []}}), [])
        low = {
            "vulnerability_id": "CVE-TEST",
            "package": "example",
            "installed_version": "1.0",
            "fixed_version": "unavailable",
            "severity": "LOW",
        }
        self.assertEqual(retain({"trivy": {"findings": [low]}})[0]["severity"], "LOW")
        with self.assertRaises(RuntimeError):
            retain({"trivy": {"findings": [{**low, "severity": "HIGH"}]}})

    def test_live_timings_are_closed_and_complete(self) -> None:
        output = b"\n".join(
            f"[kind timing] {name}_ms={value}".encode()
            for name, value in (("healthy", 1), ("failed", 2), ("unknown", 3), ("cleanup", 4))
        )
        self.assertEqual(
            ORCHESTRATOR["parse_live"](output),
            {"healthy": 1, "failed": 2, "unknown": 3, "cleanup": 4},
        )
        with self.assertRaises(RuntimeError):
            ORCHESTRATOR["parse_live"](b"[kind timing] healthy_ms=1")

    def test_privacy_scopes_match_and_include_extracted_source(self) -> None:
        self.assertEqual(CHECKER["ROOT_FILES"], VALIDATOR["PRIVACY_ROOT_FILES"])
        self.assertEqual(CHECKER["ROOT_PREFIXES"], VALIDATOR["PRIVACY_ROOT_PREFIXES"])
        self.assertIn("crates/kapsel-authority/src/lib.rs", CHECKER["tracked_paths"](ROOT))

    def test_current_repository_passes_privacy_review(self) -> None:
        with tempfile.NamedTemporaryFile() as output:
            result = subprocess.run(
                ["python3", str(CHECKER_PATH), "--output", output.name],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_privacy_review_rejects_private_inputs_and_overclaims(self) -> None:
        cases = {
            "private-path.md": b"path: /Users/operator/private\n",
            "credential.md": b"-----BEGIN PRIVATE KEY-----\n",
            "claim.md": b"Kapsel is production-ready.\n",
            "journal.sqlite3": b"SQLite format 3\0",
        }
        for name, contents in cases.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / name).write_bytes(contents)
                with self.assertRaises(RuntimeError):
                    CHECKER["validate"](root, [name])


if __name__ == "__main__":
    unittest.main()
