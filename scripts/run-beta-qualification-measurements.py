#!/usr/bin/env python3
"""Run and validate the pinned beta qualification x86-64 resource measurements."""

from __future__ import annotations

import argparse
import json
import math
import os
import subprocess
import tempfile
from pathlib import Path
from typing import Any

IMAGE = "rust@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922"
SAMPLES = 30
WARMUPS = 5


def nearest_rank_p95(values: list[int]) -> int:
    return sorted(values)[math.ceil(0.95 * len(values)) - 1]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def aggregate(raw: dict[str, Any]) -> dict[str, Any]:
    require(raw["schema_version"] == 1, "unexpected raw measurement schema")
    require(raw["samples"] == SAMPLES and raw["warmups"] == WARMUPS, "sample plan changed")
    process = raw["process_measurements"]
    expected_process = {
        "process_startup",
        "grant_provision",
        "offline_inspection",
        "journal_fresh_open",
        "journal_marked_open",
        "complete_success",
        "complete_recovery",
    }
    require(set(process) == expected_process, "process workload set changed")
    internal = raw["internal_wall_us"]
    expected_internal = {
        "submit_authorized",
        "target_read",
        "conditional_patch",
        "reconcile_apply_started",
        "receipt_finalize",
        "restart_recovery",
    }
    require(set(internal) == expected_internal, "internal workload set changed")

    measurements: dict[str, dict[str, int]] = {}
    for name, samples in process.items():
        require(len(samples) == SAMPLES, f"{name} sample count changed")
        for sample in samples:
            require(
                set(sample)
                == {
                    "wall_us",
                    "cpu_us",
                    "rss_bytes",
                    "returncode",
                    "stdout_bytes",
                    "stderr_bytes",
                },
                f"{name} sample shape changed",
            )
        measurements[name] = {
            "wall_p95_us": nearest_rank_p95([sample["wall_us"] for sample in samples]),
            "wall_max_us": max(sample["wall_us"] for sample in samples),
            "cpu_p95_us": nearest_rank_p95([sample["cpu_us"] for sample in samples]),
            "rss_max_bytes": max(sample["rss_bytes"] for sample in samples),
            "stdout_max_bytes": max(sample["stdout_bytes"] for sample in samples),
            "stderr_max_bytes": max(sample["stderr_bytes"] for sample in samples),
        }
    for name, samples in internal.items():
        require(
            len(samples) == SAMPLES and all(isinstance(value, int) for value in samples),
            f"{name} samples changed",
        )
        measurements[name] = {
            "wall_p95_us": nearest_rank_p95(samples),
            "wall_max_us": max(samples),
        }
    bounded_unknown = raw["bounded_unknown_observation"]
    require(
        set(bounded_unknown)
        == {
            "wall_us",
            "cpu_us",
            "rss_bytes",
            "returncode",
            "stdout_bytes",
            "stderr_bytes",
        },
        "bounded unknown sample shape changed",
    )
    measurements["bounded_unknown_observation"] = {
        "wall_max_us": bounded_unknown["wall_us"],
        "cpu_max_us": bounded_unknown["cpu_us"],
        "rss_max_bytes": bounded_unknown["rss_bytes"],
        "stdout_max_bytes": bounded_unknown["stdout_bytes"],
        "stderr_max_bytes": bounded_unknown["stderr_bytes"],
    }

    require(measurements["process_startup"]["wall_max_us"] <= 500_000, "startup budget failed")
    for name in ("grant_provision", "journal_marked_open", "offline_inspection"):
        require(measurements[name]["wall_p95_us"] <= 500_000, f"{name} wall budget failed")
    for name in (
        "journal_fresh_open",
        "submit_authorized",
        "reconcile_apply_started",
        "restart_recovery",
        "target_read",
        "conditional_patch",
    ):
        require(measurements[name]["wall_p95_us"] <= 1_000_000, f"{name} wall budget failed")
    require(measurements["receipt_finalize"]["wall_p95_us"] <= 500_000, "receipt budget failed")
    require(
        measurements["bounded_unknown_observation"]["wall_max_us"] <= 35_000_000,
        "bounded unknown wall budget failed",
    )
    for name in process:
        cpu_limit = 2_000_000 if name in {"complete_success", "complete_recovery"} else 1_000_000
        require(measurements[name]["cpu_p95_us"] <= cpu_limit, f"{name} CPU budget failed")
        require(
            measurements[name]["rss_max_bytes"] <= 128 * 1024 * 1024, f"{name} RSS budget failed"
        )
    require(
        measurements["bounded_unknown_observation"]["rss_max_bytes"] <= 128 * 1024 * 1024,
        "bounded unknown RSS budget failed",
    )
    require(
        raw["binary"]["build_target"] == "x86_64-unknown-linux-gnu",
        "explicit build target changed",
    )
    require(raw["binary"]["ordinary_bytes"] <= 32 * 1024 * 1024, "ordinary size budget failed")
    require(raw["binary"]["demo_bytes"] <= 32 * 1024 * 1024, "demo size budget failed")
    growth = raw["growth"]
    require(growth["operations"] == 10_000, "journal operation count changed")
    require(growth["final_bytes"] <= 64 * 1024 * 1024, "journal size budget failed")
    require(growth["average_growth_bytes"] <= 8 * 1024, "journal growth budget failed")
    require(growth["maximal_receipt_bytes"] <= 16 * 1024, "maximal receipt budget failed")
    require(growth["persisted_value_bytes_max"] == 16 * 1024, "persisted value limit changed")
    require(
        growth["sqlite_value_or_row_bytes_max"] == 64 * 1024,
        "SQLite value-or-row limit changed",
    )
    require(
        growth["rollback_journal_bytes_max"] == 65 * 1024 * 1024,
        "rollback journal limit changed",
    )

    return {
        "schema_version": 1,
        "environment_id": "debian12-linux-amd64-virtualized",
        "warmups": WARMUPS,
        "samples": SAMPLES,
        "measurements": measurements,
        "growth": growth,
        "wire_sizes": raw["wire_sizes"],
        "binary": raw["binary"],
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    repo = Path.cwd().resolve()
    registry = Path.home() / ".cargo/registry"
    require(registry.is_dir(), "host Cargo registry is unavailable")
    require(
        subprocess.run(["git", "diff", "--quiet", "--exit-code"], cwd=repo).returncode == 0
        and subprocess.run(
            ["git", "diff", "--cached", "--quiet", "--exit-code"], cwd=repo
        ).returncode
        == 0,
        "measurement requires a clean source tree",
    )
    with tempfile.TemporaryDirectory(prefix="beta-qualification-output-") as output_directory:
        raw_path = Path(output_directory) / "raw.json"
        uid = os.getuid()
        gid = os.getgid()
        command = [
            "docker",
            "run",
            "--rm",
            "--platform",
            "linux/amd64",
            "--cpus",
            "8",
            "--memory",
            "8g",
            "--network",
            "none",
            "--user",
            f"{uid}:{gid}",
            "-v",
            f"{repo}:/workspace:ro",
            "-v",
            f"{registry}:/cargo-registry:ro",
            "-v",
            f"{output_directory}:/output",
            "-w",
            "/workspace",
            IMAGE,
            "bash",
            "-lc",
            "mkdir -p /tmp/cargo && ln -s /cargo-registry /tmp/cargo/registry && "
            "PATH=/usr/local/cargo/bin:$PATH CARGO_HOME=/tmp/cargo CARGO_NET_OFFLINE=true "
            "RUSTUP_TOOLCHAIN=1.98.0-x86_64-unknown-linux-gnu "
            "python3 scripts/measure-beta-qualification.py --output /output/raw.json",
        ]
        subprocess.run(command, check=True)
        raw = json.loads(raw_path.read_text())
    result = aggregate(raw)
    arguments.output.write_text(json.dumps(result, sort_keys=True, indent=2) + "\n")


if __name__ == "__main__":
    main()
