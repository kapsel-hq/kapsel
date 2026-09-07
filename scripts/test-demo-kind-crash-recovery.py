#!/usr/bin/env python3
"""Deterministic prerequisite refusal tests for the live release demonstration harness."""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
HARNESS = ROOT / "scripts" / "demo-kind-crash-recovery.sh"

FAKE_DEMO_EXECUTABLE = r'''
command=$1
shift
case "$command" in
  provision-grant)
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --output ]; then output=$2; shift 2; else shift; fi
    done
    printf grant >"$output"
    printf '%s\n' '{"command":"provision-grant","status":"PROVISIONED"}'
    ;;
  operate)
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --operator-config ]; then operator=$2; shift 2; else shift; fi
    done
    workspace=$(dirname "$operator")
    case "${KAPSEL_DEMO_PAUSE:-}" in
      after_apply)
        printf '1\n' >"$KAPSEL_DEMO_CONTROL_DIRECTORY/provider-apply-count"
        : >"$KAPSEL_DEMO_CONTROL_DIRECTORY/after-apply.ready"
        while :; do sleep 1; done
        ;;
      after_receipt_commit)
        python3 - "$workspace/failed-journal.sqlite3" <<'PYSQL'
import hashlib
import sqlite3
import sys
with sqlite3.connect(sys.argv[1]) as connection:
    connection.execute("CREATE TABLE kubernetes_image_operations(state, receipt_digest)")
    connection.execute("INSERT INTO kubernetes_image_operations VALUES (?, ?)",
                       ("finalized", hashlib.sha256(b"receipt").hexdigest()))
PYSQL
        : >"$KAPSEL_DEMO_CONTROL_DIRECTORY/after-receipt-commit.ready"
        while :; do sleep 1; done
        ;;
      *)
        if echo "$operator" | grep -q healthy; then
          printf '%s\n' '{"state":"FINALIZED","result":"SUCCEEDED"}'
        else
          printf receipt >"$workspace/rotated-receipts/fake.receipt"
          printf '%s\n' '{"state":"FINALIZED","result":"FAILED"}'
        fi
        ;;
    esac
    ;;
  inspect)
    printf '%s\n' '{"status":"INSPECTED","result":"FAILED","rollout_condition_reason":"ProgressDeadlineExceeded","non_claims":"no-exactly-once;no-causation;no-kubernetes-truth;no-complete-capture;no-witnessing;not-production"}'
    ;;
esac
'''


class HarnessPrerequisiteTests(unittest.TestCase):
    def run_case(
        self,
        commands: dict[str, str],
        artifact_state: str | None = None,
        *,
        packaged: bool = False,
        executable_body: str = "exit 0",
    ) -> tuple[subprocess.CompletedProcess[str], str]:
        with tempfile.TemporaryDirectory(prefix="kapsel-demo-prerequisites-") as temporary:
            directory = pathlib.Path(temporary)
            log = directory / "calls.log"
            for name, body in commands.items():
                if name == "docker" and body == "exit 0":
                    body = "[ \"$1\" = version ] && echo '29.4.0'; exit 0"
                path = directory / name
                path.write_text(f"#!/bin/sh\nprintf '%s\\n' \"{name} $*\" >>\"$FAKE_LOG\"\n{body}\n")
                path.chmod(0o755)
            environment = os.environ.copy()
            environment["PATH"] = f"{directory}:{environment['PATH']}"
            environment["FAKE_LOG"] = str(log)
            harness = HARNESS
            if packaged:
                root = directory / "release"
                assets = root / "share" / "kapsel"
                executable = root / "libexec" / "kapsel-demo-harness"
                assets.mkdir(parents=True)
                executable.parent.mkdir()
                harness = assets / "demo-kind-crash-recovery.sh"
                shutil.copyfile(HARNESS, harness)
                harness.chmod(0o755)
                assets.joinpath("kap0038-trust.hex").write_text("00")
                if artifact_state != "missing-executable":
                    executable.write_text(f"#!/bin/sh\n{executable_body}\n")
                    executable.chmod(0o755)
            elif artifact_state is not None:
                executable = directory / "kapsel-demo-harness"
                assets = directory / "assets"
                assets.mkdir()
                if artifact_state != "missing-executable":
                    executable.write_text(f"#!/bin/sh\n{executable_body}\n")
                    executable.chmod(0o755)
                if artifact_state != "missing-vector":
                    assets.joinpath("kap0038-trust.hex").write_text("00")
                environment["KAPSEL_DEMO_EXECUTABLE"] = str(executable)
                environment["KAPSEL_DEMO_ASSET_DIRECTORY"] = str(assets)
            result = subprocess.run(
                [str(harness)],
                cwd=ROOT,
                env=environment,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
                timeout=10,
            )
            calls = log.read_text() if log.exists() else ""
            return result, calls

    def test_missing_artifact_executable_stops_before_docker(self) -> None:
        result, calls = self.run_case(
            {"docker": "exit 99"},
            artifact_state="missing-executable",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, "")
        self.assertIn("artifact demo executable is unsafe or unavailable", result.stderr)

    def test_missing_artifact_vector_stops_before_docker(self) -> None:
        result, calls = self.run_case(
            {"docker": "exit 99"},
            artifact_state="missing-vector",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, "")
        self.assertIn("artifact demo trust vector is unsafe or unavailable", result.stderr)

    def test_packaged_layout_is_discovered_without_environment(self) -> None:
        result, calls = self.run_case(
            {"docker": "exit 99"},
            artifact_state="missing-executable",
            packaged=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, "")
        self.assertIn("artifact demo executable is unsafe or unavailable", result.stderr)

    def test_unavailable_docker_stops_before_cluster_inspection(self) -> None:
        result, calls = self.run_case({"docker": "exit 1", "kind": "exit 99"})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, "docker info\n")
        self.assertIn("start the Docker daemon", result.stderr)

    def test_unparseable_docker_version_stops_before_kind(self) -> None:
        result, calls = self.run_case(
            {
                "docker": "[ \"$1\" = version ] && echo unknown; exit 0",
                "kind": "exit 99",
            }
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("docker version", calls)
        self.assertNotIn("kind", calls)
        self.assertIn("cannot parse Docker server version", result.stderr)

    def test_old_kind_stops_before_cluster_creation(self) -> None:
        result, calls = self.run_case(
            {
                "docker": "exit 0",
                "kind": "[ \"$1\" = version ] && echo 'kind v0.31.0'; exit 0",
            }
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("kind version", calls)
        self.assertNotIn("kind create", calls)
        self.assertIn("install kind 0.32 or newer", result.stderr)

    def test_old_kubectl_reports_corrective_action(self) -> None:
        result, calls = self.run_case(
            {
                "docker": "exit 0",
                "kind": "echo 'kind v0.32.0'",
                "kubectl": "echo '{\"clientVersion\":{\"major\":\"1\",\"minor\":\"29\"}}'",
            }
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("kubectl version", calls)
        self.assertIn("install kubectl 1.30 or newer", result.stderr)

    def test_old_python_reports_corrective_action(self) -> None:
        result, calls = self.run_case(
            {
                "docker": "exit 0",
                "kind": "echo 'kind v0.32.0'",
                "kubectl": "echo '{\"clientVersion\":{\"major\":\"1\",\"minor\":\"34\"}}'",
                "python3": "exit 1",
            }
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("python3 -c", calls)
        self.assertIn("install Python 3.11 or newer", result.stderr)

    def test_failed_cluster_creation_explains_compatibility_and_cleanup(self) -> None:
        result, calls = self.run_case(
            {
                "docker": "exit 0",
                "kind": (
                    "if [ \"$1\" = version ]; then echo 'kind v0.32.0'; "
                    "elif [ \"$1 $2\" = 'get clusters' ]; then echo 'No kind clusters found.'; "
                    "elif [ \"$1\" = create ]; then exit 1; fi; exit 0"
                ),
                "kubectl": "echo '{\"clientVersion\":{\"major\":\"1\",\"minor\":\"34\"}}'",
            }
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("kind create cluster", calls)
        self.assertIn("kind delete cluster", calls)
        self.assertIn("verify that kind supports this Docker host", result.stderr)
        self.assertIn("owned kind cluster removed", result.stdout)

    def test_cleanup_failure_prints_exact_owned_retry(self) -> None:
        result, calls = self.run_case(
            {
                "docker": "exit 0",
                "kind": (
                    "if [ \"$1\" = version ]; then echo 'kind v0.32.0'; "
                    "elif [ \"$1 $2\" = 'get clusters' ]; then echo 'No kind clusters found.'; "
                    "elif [ \"$1\" = create ]; then exit 1; "
                    "elif [ \"$1\" = delete ]; then exit 1; fi; exit 0"
                ),
                "kubectl": "echo '{\"clientVersion\":{\"major\":\"1\",\"minor\":\"34\"}}'",
            }
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("kind delete cluster", calls)
        self.assertIn("kind delete cluster --name kapsel-demo-", result.stderr)

    def test_workspace_cleanup_failure_names_only_the_owned_retry(self) -> None:
        result, _ = self.run_case(
            {
                "docker": "exit 0",
                "kind": (
                    "if [ \"$1\" = version ]; then echo 'kind v0.32.0'; "
                    "elif [ \"$1 $2\" = 'get clusters' ]; then echo 'No kind clusters found.'; "
                    "elif [ \"$1\" = create ]; then exit 1; fi; exit 0"
                ),
                "kubectl": "echo '{\"clientVersion\":{\"major\":\"1\",\"minor\":\"34\"}}'",
                "rm": "echo hidden-descendant >&2; exit 1",
            }
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("hidden-descendant", result.stderr)
        self.assertIn("retry only this owned path: rm -rf --", result.stderr)

    def test_timeline_and_final_summary_explain_the_recovery_boundary(self) -> None:
        result, _ = self.run_case(
            {
                "docker": "exit 0",
                "kind": (
                    "if [ \"$1\" = version ]; then echo 'kind v0.32.0'; "
                    "elif [ \"$1 $2\" = 'get clusters' ]; then echo 'No kind clusters found.'; "
                    "elif [ \"$1 $2\" = 'get kubeconfig' ]; then echo kubeconfig; fi; exit 0"
                ),
                "kubectl": (
                    "if [ \"$1\" = version ]; then "
                    "echo '{\"clientVersion\":{\"major\":\"1\",\"minor\":\"34\"}}'; "
                    "elif echo \"$*\" | grep -q 'apply -f -'; then cat >/dev/null; "
                    "elif echo \"$*\" | grep -q 'get deployment'; then "
                    "printf registry.k8s.io/pause:3.10.1; fi; exit 0"
                ),
                "shasum": "exit 99",
            },
            artifact_state="ready",
            packaged=True,
            executable_body=FAKE_DEMO_EXECUTABLE,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout, r"\[demo \+[0-9]+s 1/9\]")
        for evidence in [
            "durable attempt: apply_started recorded before provider mutation",
            "process termination: after the returned mutation",
            "restart behavior: reconciled without a blind second mutation",
            "provider apply count: 1",
            "receiver outcome: FAILED from ProgressDeadlineExceeded",
            "frozen receipt",
            "offline inspection path:",
            "offline inspection: INSPECTED, never VERIFIED",
            "UNKNOWN boundary:",
            "owned kind cluster removed",
            "owned workspace removed",
        ]:
            self.assertIn(evidence, result.stdout)

    def test_preexisting_cluster_stops_before_creation(self) -> None:
        result, calls = self.run_case(
            {
                "docker": "exit 0",
                "kind": "if [ \"$1\" = version ]; then echo 'kind v0.32.0'; else echo occupied; fi",
                "kubectl": (
                    "echo '{\"clientVersion\":{\"major\":\"1\",\"minor\":\"34\"}}'"
                ),
            }
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to run while kind clusters already exist", result.stderr)
        self.assertNotIn("kind create", calls)


if __name__ == "__main__":
    unittest.main()
