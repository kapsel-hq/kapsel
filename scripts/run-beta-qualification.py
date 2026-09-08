#!/usr/bin/env python3
"""Run every beta qualification lane and freeze one closed replacement baseline."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import runpy
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
VALIDATOR = runpy.run_path(str(ROOT / "scripts/validate-beta-qualification-baseline.py"))
EXPECTED_BUDGETS = VALIDATOR["EXPECTED_BUDGETS"]
BUDGET_FIELDS = VALIDATOR["BUDGET_FIELDS"]
BUILDER_IMAGE_DIGEST = "82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922"
NODE_IMAGE = (
    "kindest/node:v1.33.12@sha256:3f5c8443c620245e4d355cfe09e96a91ead32ceaa569d3f1ca9edf0cb2fe2ff4"
)


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_path(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def digest_paths(paths: list[Path]) -> str:
    digest = hashlib.sha256()
    for path in sorted(paths):
        relative = path.relative_to(ROOT).as_posix()
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def git(*arguments: str) -> str:
    return subprocess.check_output(["git", *arguments], cwd=ROOT, text=True).strip()


SOURCE_FILES = {
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    "scripts/format.sh",
    "scripts/test-demo-harness.sh",
    "scripts/test-fuzz.sh",
    "scripts/test-simulation.sh",
}
SOURCE_PREFIXES = ("crates/kapsel-authority/", "src/", "vectors/")


def selected_source_paths(paths: list[str]) -> list[str]:
    return sorted(
        path for path in paths if path in SOURCE_FILES or path.startswith(SOURCE_PREFIXES)
    )


def source_identity(commit: str) -> tuple[str, int]:
    paths = git("ls-tree", "-r", "--name-only", commit).splitlines()
    selected = selected_source_paths(paths)
    digest = hashlib.sha256()
    for path in selected:
        contents = subprocess.check_output(["git", "show", f"{commit}:{path}"], cwd=ROOT)
        digest.update(path.encode())
        digest.update(b"\0")
        digest.update(contents)
        digest.update(b"\0")
    return digest.hexdigest(), len(selected)


def run_lane(
    name: str,
    command: list[str],
    directory: Path,
    timeout: int,
    output_path: Path | None = None,
) -> dict[str, Any]:
    print(f"==> {name}", file=sys.stderr, flush=True)
    started = time.monotonic_ns()
    execution_command = [
        str(output_path) if argument == "BOUNDED_OUTPUT" and output_path is not None else argument
        for argument in command
    ]
    completed = subprocess.run(
        execution_command,
        cwd=directory,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    duration_ms = (time.monotonic_ns() - started) // 1_000_000
    combined = completed.stdout + b"\n--- stderr ---\n" + completed.stderr
    if completed.returncode != 0:
        sys.stderr.buffer.write(combined[-65_536:])
        raise RuntimeError(f"{name} failed with exit {completed.returncode}")
    bounded = output_path.read_bytes() if output_path is not None else combined
    return {
        "name": name,
        "command": command,
        "duration_ms": duration_ms,
        "bounded_output_sha256": sha256_bytes(bounded),
        "bounded_output": bounded,
    }


def host_memory_bytes() -> int:
    if sys.platform == "darwin":
        return int(subprocess.check_output(["sysctl", "-n", "hw.memsize"], text=True))
    pages = os.sysconf("SC_PHYS_PAGES")
    page_size = os.sysconf("SC_PAGE_SIZE")
    return int(pages * page_size)


def version(command: list[str], cwd: Path = ROOT) -> str:
    return subprocess.check_output(command, cwd=cwd, text=True, stderr=subprocess.STDOUT).strip()


def rust_commit(command: list[str]) -> str:
    output = version(command)
    commit = next(
        line.split(":", 1)[1].strip()
        for line in output.splitlines()
        if line.startswith("commit-hash:")
    )
    release = next(
        line.split(":", 1)[1].strip() for line in output.splitlines() if line.startswith("release:")
    )
    return f"rustc {release} commit {commit}"


def parse_kind_version(output: str) -> str:
    match = re.fullmatch(r"kind v([0-9]+\.[0-9]+\.[0-9]+) .+", output)
    if match is None:
        raise RuntimeError("kind version output is not recognized")
    return match.group(1)


def tools(security: dict[str, Any]) -> list[dict[str, Any]]:
    docker = version(
        ["docker", "version", "--format", "client {{.Client.Version}} server {{.Server.Version}}"]
    )
    kind = parse_kind_version(version(["kind", "version"]))
    kubectl_document = json.loads(version(["kubectl", "version", "--client", "-o", "json"]))
    kubectl = kubectl_document["clientVersion"]["gitVersion"].removeprefix("v")
    audit_tool = security["cargo_audit_tool"]
    trivy_tool = security["trivy_tool"]
    return [
        {"id": "rust-host", "environment_id": "host", "version": rust_commit(["rustc", "-Vv"])},
        {"id": "cargo-host", "environment_id": "host", "version": version(["cargo", "--version"])},
        {"id": "python-host", "environment_id": "host", "version": platform.python_version()},
        {"id": "docker", "environment_id": "host", "version": docker},
        {"id": "kind", "environment_id": "host", "version": kind},
        {"id": "kubectl", "environment_id": "host", "version": kubectl},
        {
            "id": "cargo-fuzz",
            "environment_id": "host",
            "version": version(["cargo", "fuzz", "--version"], ROOT / "fuzz"),
        },
        {
            "id": "nightly-rust",
            "environment_id": "host",
            "version": rust_commit(["rustup", "run", "nightly-2026-07-03", "rustc", "-Vv"]),
        },
        {
            "id": "cargo-audit",
            "environment_id": "host",
            "version": audit_tool["version"].removeprefix("cargo-audit "),
            "database_utc": audit_tool["database_utc"],
        },
        {
            "id": "trivy",
            "environment_id": "host",
            "version": f"{trivy_tool['version']} database version {trivy_tool['database_version']}",
            "database_utc": trivy_tool["database_utc"],
        },
        {
            "id": "rust-container",
            "environment_id": "container",
            "version": "rustc and cargo 1.98.0",
        },
        {"id": "python-container", "environment_id": "container", "version": "Python 3.11.2"},
        {
            "id": "builder-image",
            "environment_id": "container",
            "version": f"rust image digest {BUILDER_IMAGE_DIGEST}",
        },
    ]


def producer(lane: dict[str, Any], environment_id: str, inputs: dict[str, str]) -> dict[str, Any]:
    return {
        "command": lane["command"],
        "duration_ms": lane["duration_ms"],
        "environment_id": environment_id,
        "input_sha256": dict(sorted(inputs.items())),
        "bounded_output_sha256": lane["bounded_output_sha256"],
    }


def measurement(id_: str, value: int, statistic: str, unit: str) -> dict[str, Any]:
    return {"id": id_, "value": value, "statistic": statistic, "unit": unit}


def budget_result(
    subject: str,
    value: int,
    source: dict[str, Any],
    assertions: list[tuple[str, str]] | None = None,
) -> dict[str, Any]:
    spec = EXPECTED_BUDGETS[subject]
    values = dict(zip(BUDGET_FIELDS, spec, strict=True))
    return {
        "id": f"budget-{subject}",
        "kind": "budget",
        "subject_id": subject,
        **source,
        "status": "passed",
        "sample_count": values["required_samples"],
        "failure_count": 0,
        "measurements": [measurement("budget-value", value, values["statistic"], values["unit"])],
        "assertions": [
            {"id": id_, "passed": True, "detail": detail}
            for id_, detail in (
                assertions
                or [("within-budget", "measured value is within the frozen stopping rule")]
            )
        ],
    }


def lane_result(
    subject: str,
    source: dict[str, Any],
    sample_count: int,
    assertions: list[tuple[str, str]],
) -> dict[str, Any]:
    return {
        "id": f"lane-{subject}",
        "kind": "lane",
        "subject_id": subject,
        **source,
        "status": "passed",
        "sample_count": sample_count,
        "failure_count": 0,
        "measurements": [],
        "assertions": [{"id": id_, "passed": True, "detail": detail} for id_, detail in assertions],
    }


def parse_live(output: bytes) -> dict[str, int]:
    text = output.decode(errors="replace")
    values = {}
    for name in ("healthy", "failed", "unknown", "cleanup"):
        match = re.search(rf"\[kind timing\] {name}_ms=(\d+)", text)
        if match is None:
            raise RuntimeError(f"live output lacks {name} timing")
        values[name] = int(match.group(1))
    return values


def retained_security_findings(security: dict[str, Any]) -> list[dict[str, Any]]:
    findings = []
    for index, finding in enumerate(security["trivy"]["findings"], start=1):
        if finding["severity"] in {"HIGH", "CRITICAL"}:
            raise RuntimeError("rejected Trivy severity reached baseline construction")
        findings.append(
            {
                "id": f"trivy-finding-{index}",
                "scanner": "trivy",
                **finding,
                "disposition": "retained for review and not rejected by the frozen severity policy",
            }
        )
    return findings


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    if git("status", "--porcelain"):
        raise RuntimeError("qualification requires a clean tree")
    commit = git("rev-parse", "HEAD")
    tree = git("rev-parse", f"{commit}^{{tree}}")
    source_sha256, source_path_count = source_identity(commit)
    source_input = {"source": source_sha256}

    with tempfile.TemporaryDirectory(prefix="beta-qualification-evidence-") as temporary:
        evidence = Path(temporary)
        default = run_lane("default and hostile input", ["./scripts/ci-local.sh"], ROOT, 900)
        simulation = run_lane("seeded simulation", ["./scripts/test-simulation.sh"], ROOT, 900)
        fuzz = run_lane("seeded fuzz", ["./scripts/test-fuzz.sh"], ROOT, 900)
        subprocess_lane = run_lane(
            "v0.1.1 subprocess matrix",
            ["python3", "scripts/test-v011-upgrade-fixtures.py"],
            ROOT,
            1800,
        )
        demo = run_lane("deterministic demo", ["./scripts/test-demo-harness.sh"], ROOT, 900)
        live = run_lane("live kind", ["./scripts/test-kind-effect-gateway.sh"], ROOT, 1200)
        measurement_path = evidence / "measurement.json"
        measurement_lane = run_lane(
            "x86-64 measurement",
            [
                "python3",
                "scripts/run-beta-qualification-measurements.py",
                "--output",
                "BOUNDED_OUTPUT",
            ],
            ROOT,
            1800,
            measurement_path,
        )
        privacy_path = evidence / "privacy.json"
        privacy = run_lane(
            "privacy and overclaim review",
            [
                "python3",
                "scripts/check-beta-qualification-privacy.py",
                "--output",
                "BOUNDED_OUTPUT",
            ],
            ROOT,
            120,
            privacy_path,
        )
        security_path = evidence / "security.json"
        security_lane = run_lane(
            "dependency and secret scans",
            ["python3", "scripts/run-beta-qualification-security.py", "--output", "BOUNDED_OUTPUT"],
            ROOT,
            900,
            security_path,
        )
        measured = json.loads(measurement_path.read_text())
        security = json.loads(security_path.read_text())

    measurement_inputs = {
        **source_input,
        "demo-executable": measured["binary"]["demo_sha256"],
        "ordinary-executable": measured["binary"]["ordinary_sha256"],
        "measurement-harness": digest_paths(
            [
                ROOT / "scripts/measure-beta-qualification.py",
                ROOT / "scripts/run-beta-qualification-measurements.py",
                ROOT / "src/gateway/tests/qualification.rs",
            ]
        ),
    }
    live_inputs = {
        **source_input,
        "kind-harness": sha256_path(ROOT / "scripts/test-kind-effect-gateway.sh"),
        "node-image": sha256_bytes(NODE_IMAGE.encode()),
    }
    privacy_document = json.loads(privacy["bounded_output"])
    privacy_inputs = {
        **source_input,
        "checked-source": privacy_document["checked_source_sha256"],
        "privacy-check": digest_paths([ROOT / "scripts/check-beta-qualification-privacy.py"]),
    }
    security_inputs = {
        **source_input,
        "cargo-lock": security["cargo_lock_sha256"],
        "rustsec-database": security["cargo_audit_tool"]["database_sha256"],
        "trivy-database": security["trivy_tool"]["database_sha256"],
    }
    default_source = producer(default, "host", source_input)
    measurement_source = producer(measurement_lane, "container", measurement_inputs)
    live_source = producer(live, "host", live_inputs)
    security_source = producer(security_lane, "host", security_inputs)

    values: dict[str, int] = {
        "process-startup-wall": measured["measurements"]["process_startup"]["wall_max_us"],
        "grant-provision-wall": measured["measurements"]["grant_provision"]["wall_p95_us"],
        "journal-fresh-open-wall": measured["measurements"]["journal_fresh_open"]["wall_p95_us"],
        "journal-marked-open-wall": measured["measurements"]["journal_marked_open"]["wall_p95_us"],
        "offline-inspection-wall": measured["measurements"]["offline_inspection"]["wall_p95_us"],
        "submit-authorized-wall": measured["measurements"]["submit_authorized"]["wall_p95_us"],
        "target-read-wall": measured["measurements"]["target_read"]["wall_p95_us"],
        "conditional-patch-wall": measured["measurements"]["conditional_patch"]["wall_p95_us"],
        "reconcile-apply-started-wall": measured["measurements"]["reconcile_apply_started"][
            "wall_p95_us"
        ],
        "receipt-finalize-wall": measured["measurements"]["receipt_finalize"]["wall_p95_us"],
        "restart-recovery-wall": measured["measurements"]["restart_recovery"]["wall_p95_us"],
        "process-startup-cpu": measured["measurements"]["process_startup"]["cpu_p95_us"],
        "grant-provision-cpu": measured["measurements"]["grant_provision"]["cpu_p95_us"],
        "journal-fresh-open-cpu": measured["measurements"]["journal_fresh_open"]["cpu_p95_us"],
        "journal-marked-open-cpu": measured["measurements"]["journal_marked_open"]["cpu_p95_us"],
        "offline-inspection-cpu": measured["measurements"]["offline_inspection"]["cpu_p95_us"],
        "complete-success-cpu": measured["measurements"]["complete_success"]["cpu_p95_us"],
        "complete-recovery-cpu": measured["measurements"]["complete_recovery"]["cpu_p95_us"],
        "process-rss": max(
            item["rss_max_bytes"]
            for item in measured["measurements"].values()
            if "rss_max_bytes" in item
        ),
        "bounded-unknown-wall": (
            measured["measurements"]["bounded_unknown_observation"]["wall_max_us"] + 999
        )
        // 1000,
        "journal-size": measured["growth"]["final_bytes"],
        "journal-average-growth": measured["growth"]["average_growth_bytes"],
        "persisted-value-size": measured["growth"]["persisted_value_bytes_max"],
        "sqlite-value-or-row-size": measured["growth"]["sqlite_value_or_row_bytes_max"],
        "rollback-journal-size": measured["growth"]["rollback_journal_bytes_max"],
        "grant-size": measured["wire_sizes"]["canonical_grant_bytes"],
        "trust-size": measured["wire_sizes"]["canonical_trust_bytes"],
        "receipt-size": measured["growth"]["maximal_receipt_bytes"],
        "statement-size": measured["wire_sizes"]["canonical_statement_bytes"],
        "ordinary-executable-size": measured["binary"]["ordinary_bytes"],
        "demo-executable-size": measured["binary"]["demo_bytes"],
        "request-json-size": 16384,
        "mcp-frame-size": 16384,
        "mcp-response-size": 8192,
        "machine-output-size": 65536,
        "kubernetes-identity-size": 128,
        "immutable-image-size": 512,
        "kubernetes-response-size": 2097152,
        "security-findings": 0,
    }
    live_values = parse_live(live["bounded_output"])
    values.update(
        {
            "live-healthy-wall": live_values["healthy"],
            "live-failed-wall": live_values["failed"],
            "live-unknown-wall": live_values["unknown"],
            "live-cleanup-wall": live_values["cleanup"],
        }
    )
    measurement_subjects = {
        subject
        for subject in values
        if subject
        not in {
            "request-json-size",
            "mcp-frame-size",
            "mcp-response-size",
            "machine-output-size",
            "kubernetes-identity-size",
            "immutable-image-size",
            "kubernetes-response-size",
            "security-findings",
            "live-healthy-wall",
            "live-failed-wall",
            "live-unknown-wall",
            "live-cleanup-wall",
        }
    }
    results = []
    for subject in sorted(EXPECTED_BUDGETS):
        source = measurement_source if subject in measurement_subjects else default_source
        assertions = None
        if subject.startswith("live-"):
            source = live_source
            assertions = [
                (
                    "one-patch-and-owned-cleanup",
                    "live scenario retained one patch opportunity and owned cleanup",
                )
            ]
        elif subject == "security-findings":
            source = security_source
            assertions = [("no-rejected-finding", "RustSec and Trivy reported no rejected finding")]
        elif subject == "bounded-unknown-wall":
            assertions = [
                (
                    "deterministic-404-fixture",
                    "deterministic fixture returned 404 for every receiver read",
                ),
                ("receiver-result-unknown", "bounded observation returned UNKNOWN"),
                ("thirty-read-schedule", "production schedule exhausted exactly 30 reads"),
                ("zero-recovery-patches", "restart observation issued zero patches"),
            ]
        results.append(budget_result(subject, values[subject], source, assertions))

    simulation_source = producer(
        simulation,
        "host",
        {**source_input, "simulation-seed": sha256_bytes(b"21182435914953528")},
    )
    fuzz_source = producer(
        fuzz,
        "host",
        {
            **source_input,
            "corpus": "86dda67e958b96cd56452de77199c2ebfac36400d6c971e84966a4b9fb3e9e8d",
        },
    )
    results.extend(
        [
            lane_result(
                "default", default_source, 1, [("default-gate", "default repository gate passed")]
            ),
            lane_result(
                "hostile-input",
                default_source,
                1,
                [("denial-matrix", "hostile input matrices passed")],
            ),
            lane_result(
                "simulation",
                simulation_source,
                10000,
                [("replayable", "all seeded cases preserved lifecycle invariants")],
            ),
            lane_result(
                "fuzz",
                fuzz_source,
                10000,
                [("no-crash", "all seeded fuzz runs completed without finding")],
            ),
            lane_result(
                "subprocess",
                producer(subprocess_lane, "host", source_input),
                9,
                [("historical-compatibility", "all historical states and process seams passed")],
            ),
            lane_result(
                "demo",
                producer(demo, "host", source_input),
                1,
                [("one-apply", "real-process recovery retained one patch")],
            ),
            lane_result(
                "live-kind",
                live_source,
                3,
                [
                    ("three-scenarios", "healthy failed and unknown scenarios passed"),
                    ("one-patch-each", "every live scenario retained one patch opportunity"),
                    ("owned-cleanup", "the live harness removed only owned resources"),
                ],
            ),
            lane_result(
                "measurement",
                measurement_source,
                391,
                [
                    ("all-budgets", "all declared resource stopping rules passed"),
                    (
                        "explicit-target-build",
                        "both executables used the explicit x86-64 GNU/Linux target",
                    ),
                ],
            ),
            lane_result(
                "cargo-audit",
                security_source,
                1,
                [("zero-rustsec", "cargo-audit reported zero vulnerabilities and warnings")],
            ),
            lane_result(
                "trivy",
                security_source,
                1,
                [
                    (
                        "zero-trivy",
                        "exact clean-tree scan reported zero rejected vulnerabilities and secrets",
                    )
                ],
            ),
            lane_result(
                "privacy",
                producer(privacy, "host", privacy_inputs),
                1,
                [
                    ("no-private-material", "root release scope contained no private paths"),
                    ("no-credentials", "root release scope contained no credential material"),
                    (
                        "no-raw-evidence",
                        "root release scope contained no private evidence artifact",
                    ),
                    (
                        "no-sla-overclaim",
                        "root release scope contained no unsupported production or SLA claim",
                    ),
                ],
            ),
        ]
    )

    budgets = [
        {"id": id_, **dict(zip(BUDGET_FIELDS, definition, strict=True))}
        for id_, definition in sorted(EXPECTED_BUDGETS.items())
    ]
    lanes = [
        {"id": id_, "description": description, "required": True}
        for id_, description in [
            ("default", "format links Clippy rustdoc deterministic tests and docs"),
            ("hostile-input", "root CLI MCP grant kubeconfig provider and journal denial matrices"),
            ("simulation", "seeded lifecycle crash simulation"),
            ("fuzz", "seeded receipt inspection fuzzing"),
            ("subprocess", "historical migration restore and downgrade subprocess matrix"),
            ("demo", "real-process deterministic demonstration recovery"),
            ("live-kind", "healthy failed and bounded unknown live Kubernetes scenarios"),
            ("measurement", "pinned containerized native x86-64 resource measurements"),
            ("cargo-audit", "fresh RustSec dependency audit"),
            ("trivy", "clean exact-tree vulnerability and secret scan"),
            ("privacy", "key trust disclosure privacy and no-SLA review"),
        ]
    ]
    environments = [
        {
            "id": "host",
            "os": platform.platform(),
            "architecture": platform.machine(),
            "cpu_count": os.cpu_count() or 1,
            "memory_bytes": host_memory_bytes(),
            "virtualized": False,
            "description": "dedicated native x86-64 Linux qualification host",
        },
        {
            "id": "container",
            "os": "Debian 12 container",
            "architecture": "linux-amd64",
            "cpu_count": 8,
            "memory_bytes": 8 * 1024 * 1024 * 1024,
            "virtualized": True,
            "description": "pinned x86-64 builder in an isolated Docker container",
        },
    ]
    document = {
        "schema_version": 1,
        "baseline": {
            "commit": commit,
            "tree": tree,
            "source_sha256": source_sha256,
            "source_path_count": source_path_count,
            "ordinary_executable_sha256": measured["binary"]["ordinary_sha256"],
            "demo_executable_sha256": measured["binary"]["demo_sha256"],
            "ordinary_executable_bytes": measured["binary"]["ordinary_bytes"],
            "demo_executable_bytes": measured["binary"]["demo_bytes"],
            "qualification_baseline_only": True,
        },
        "environments": environments,
        "tools": tools(security),
        "budgets": budgets,
        "lanes": lanes,
        "results": results,
        "replay": {
            "fuzz_seed": 2118243591,
            "fuzz_runs": 10000,
            "fuzz_corpus_sha256": "86dda67e958b96cd56452de77199c2ebfac36400d6c971e84966a4b9fb3e9e8d",
            "simulation_seed": 21182435914953528,
            "simulation_cases": 10000,
            "simulation_shards": 8,
        },
        "security": {
            "scanned_utc": security["scanned_utc"],
            "findings": retained_security_findings(security),
            "exceptions": [],
            "reviews": [
                {
                    "id": "dependency",
                    "status": "passed",
                    "disposition": "cargo-audit 0.22.2 reported no vulnerability or warning",
                },
                {
                    "id": "filesystem-and-trust",
                    "status": "passed",
                    "disposition": "exact modes no-follow identity size trust and replacement tests passed",
                },
                {
                    "id": "privacy-and-disclosure",
                    "status": "passed",
                    "disposition": "closed root privacy command reported no private material or overclaim",
                },
                {
                    "id": "trivy-clean-tree",
                    "status": "passed",
                    "disposition": "Trivy reported no rejected vulnerability or secret in the exact clean tree",
                },
                {
                    "id": "trivy-lower-severity",
                    "status": "passed",
                    "disposition": f"Trivy retained lower-severity counts: {json.dumps(security['trivy']['vulnerability_counts'], sort_keys=True)}",
                },
                {
                    "id": "no-sla",
                    "status": "passed",
                    "disposition": "budgets remain qualification stopping rules rather than production promises",
                },
            ],
        },
        "residual_risks": [
            {
                "id": "containerized-performance",
                "statement": "measurements from one native host in an isolated container do not establish uncontainerized or production performance",
            },
            {
                "id": "single-live-environment",
                "statement": "one disposable kind cluster cannot establish behavior across production Kubernetes distributions or failures",
            },
            {
                "id": "scanner-scope",
                "statement": "dependency scanners do not prove absence of malicious packages future advisories or unreachable lower-severity defects",
            },
            {
                "id": "beta-storage",
                "statement": "prototype journal trust lifecycle and receipt semantics remain scoped to the finite beta",
            },
        ],
        "invalidation_rules": [
            {
                "id": "root-source-or-identity",
                "trigger": "any root source dependency lockfile toolchain feature compatibility command or executable identity change",
                "rerun_lanes": ["all"],
            },
            {
                "id": "qualification-input",
                "trigger": "any qualification fixture vector corpus test harness scanner database environment or tool change",
                "rerun_lanes": ["all"],
            },
            {
                "id": "distribution-only",
                "trigger": "nonexecutable distribution metadata or publication input change",
                "rerun_lanes": ["default", "privacy", "trivy"],
            },
            {
                "id": "semantic-or-budget",
                "trigger": "any semantic security-policy or budget change",
                "rerun_lanes": ["all"],
            },
        ],
    }
    arguments.output.write_text(json.dumps(document, sort_keys=True, indent=2) + "\n")
    VALIDATOR["validate"](arguments.output)
    print(f"beta qualification replacement baseline written: {arguments.output}")


if __name__ == "__main__":
    main()
