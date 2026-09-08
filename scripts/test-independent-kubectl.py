#!/usr/bin/env python3
"""Pinned kubectl client-contract experiment, not a Kubernetes API emulator."""

import argparse
import contextlib
import copy
import json
import os
import shutil
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

VERSION = "v1.33.9"
COMMIT = "69220b617523ac1ba5d070e74c12b5daf5e6c572"
IMAGE = "registry.example/api@sha256:" + "a" * 64
OTHER_IMAGE = "registry.example/api@sha256:" + "b" * 64
INITIAL_IMAGE = "registry.example/api@sha256:" + "c" * 64
RESOURCE = "/apis/apps/v1/namespaces/demo/deployments"
CASES = (
    "healthy",
    "lost-response",
    "kill-after-persistence",
    "stale-between-get-patch",
    "intervening-writer",
    "recreated-same-revision",
    "pending-no-watch",
)


def deployment():
    return {
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": "api",
            "namespace": "demo",
            "uid": "original-uid",
            "resourceVersion": "10",
            "generation": 1,
            "annotations": {"deployment.kubernetes.io/revision": "1"},
        },
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": "api"}},
            "template": {
                "metadata": {"labels": {"app": "api"}},
                "spec": {"containers": [{"name": "api", "image": INITIAL_IMAGE}]},
            },
        },
        "status": {
            "observedGeneration": 1,
            "replicas": 1,
            "updatedReplicas": 1,
            "availableReplicas": 1,
        },
    }


def facts(obj):
    return {
        "uid": obj["metadata"]["uid"],
        "resourceVersion": obj["metadata"]["resourceVersion"],
        "generation": obj["metadata"]["generation"],
        "revision": obj["metadata"]["annotations"]["deployment.kubernetes.io/revision"],
        "image": obj["spec"]["template"]["spec"]["containers"][0]["image"],
        "availableReplicas": obj["status"]["availableReplicas"],
    }


class Fixture:
    def __init__(self, fault=None):
        self.obj = deployment()
        self.fault = fault
        self.requests = []
        self.effects = []
        self.errors = []
        self.calls = []
        self.persisted = threading.Event()
        self.release = threading.Event()
        self.lock = threading.Lock()

    def writer(self, recreate=False, same_revision=False):
        with self.lock:
            before = facts(self.obj)
            self.obj["metadata"]["resourceVersion"] = "20"
            self.obj["metadata"]["generation"] = 3
            self.obj["metadata"]["annotations"]["deployment.kubernetes.io/revision"] = (
                "2" if same_revision else "3"
            )
            if recreate:
                self.obj["metadata"]["uid"] = "replacement-uid"
            self.obj["spec"]["template"]["spec"]["containers"][0]["image"] = OTHER_IMAGE
            self.obj["status"].update(observedGeneration=3, availableReplicas=1)
            self.effects.append(
                {"actor": "fixture-writer", "before": before, "after": facts(self.obj)}
            )

    def handler(self):
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def reply(self, obj, status=200):
                data = json.dumps(obj).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                    self.wfile.write(data)

            def do_GET(self):
                path = urlsplit(self.path).path
                with fixture.lock:
                    fixture.requests.append({"method": "GET", "path": self.path})
                    current = copy.deepcopy(fixture.obj)
                group = {
                    "name": "apps",
                    "versions": [{"groupVersion": "apps/v1", "version": "v1"}],
                    "preferredVersion": {"groupVersion": "apps/v1", "version": "v1"},
                }
                if path == "/api":
                    self.reply({"kind": "APIVersions", "apiVersion": "v1", "versions": ["v1"]})
                elif path == "/apis":
                    self.reply({"kind": "APIGroupList", "apiVersion": "v1", "groups": [group]})
                elif path in ("/api/v1", "/apis/apps/v1"):
                    self.reply(
                        {
                            "kind": "APIResourceList",
                            "apiVersion": "v1",
                            "groupVersion": "apps/v1" if "apps" in path else "v1",
                            "resources": [
                                {
                                    "name": "deployments",
                                    "singularName": "deployment",
                                    "namespaced": True,
                                    "kind": "Deployment",
                                    "verbs": ["get", "list", "watch", "patch"],
                                }
                            ]
                            if "apps" in path
                            else [],
                        }
                    )
                elif path == RESOURCE + "/api":
                    # Freeze the GET response before the writer, then deliver that stale snapshot.
                    if fixture.fault == "stale-between-get-patch":
                        fixture.writer()
                        fixture.fault = None
                    self.reply(current)
                elif path == RESOURCE:
                    if parse_qs(urlsplit(self.path).query).get("watch") == ["true"]:
                        self.reply({"type": "ADDED", "object": current})
                    else:
                        self.reply(
                            {
                                "apiVersion": "apps/v1",
                                "kind": "DeploymentList",
                                "metadata": {
                                    "resourceVersion": current["metadata"]["resourceVersion"]
                                },
                                "items": [current],
                            }
                        )
                else:
                    fixture.errors.append("unexpected GET " + self.path)
                    self.reply(
                        {"kind": "Status", "status": "Failure", "message": "unexpected path"}, 404
                    )

            def do_PATCH(self):
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= 8192:
                    fixture.errors.append("unexpected PATCH size")
                    self.reply({}, 400)
                    return
                patch = json.loads(self.rfile.read(size))
                # Only accept the exact client-generated image patch this experiment studies.
                expected = {
                    "spec": {
                        "template": {
                            "spec": {
                                "$setElementOrder/containers": [{"name": "api"}],
                                "containers": [{"image": IMAGE, "name": "api"}],
                            }
                        }
                    }
                }
                if (
                    urlsplit(self.path).path != RESOURCE + "/api"
                    or patch != expected
                    or self.headers.get("Content-Type") != "application/strategic-merge-patch+json"
                ):
                    fixture.errors.append("unexpected PATCH shape or content type")
                    self.reply({}, 400)
                    return
                with fixture.lock:
                    fixture.requests.append({"method": "PATCH", "path": self.path, "body": patch})
                    before = facts(fixture.obj)
                    fixture.obj["spec"]["template"]["spec"]["containers"][0]["image"] = IMAGE
                    fixture.obj["metadata"]["resourceVersion"] = "21"
                    fixture.obj["metadata"]["generation"] += 1
                    fixture.obj["metadata"]["annotations"]["deployment.kubernetes.io/revision"] = (
                        "2"
                    )
                    fixture.obj["status"]["availableReplicas"] = 0
                    current = copy.deepcopy(fixture.obj)
                    fixture.effects.append(
                        {"actor": "kubectl-patch", "before": before, "after": facts(current)}
                    )
                fixture.persisted.set()
                if fixture.fault == "kill-after-persistence":
                    fixture.release.wait(10)
                if fixture.fault in ("lost-response", "kill-after-persistence"):
                    self.close_connection = True
                    with contextlib.suppress(OSError):
                        self.connection.shutdown(socket.SHUT_RDWR)
                    self.connection.close()
                else:
                    self.reply(current)

        return Handler

    def settle(self):
        with self.lock:
            self.obj["status"].update(
                observedGeneration=self.obj["metadata"]["generation"], availableReplicas=1
            )
            self.effects.append({"actor": "fixture-status", "after": facts(self.obj)})


@contextlib.contextmanager
def environment(kubectl, fault=None):
    fixture = Fixture(fault)
    with tempfile.TemporaryDirectory(prefix="kubectl-corpus-") as directory:
        server = ThreadingHTTPServer(("127.0.0.1", 0), fixture.handler())
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        root = Path(directory)
        endpoint = f"http://127.0.0.1:{server.server_port}"
        config = root / "config.json"
        config.write_text(
            json.dumps(
                {
                    "apiVersion": "v1",
                    "kind": "Config",
                    "current-context": "fixture",
                    "clusters": [{"name": "fixture", "cluster": {"server": endpoint}}],
                    "contexts": [
                        {"name": "fixture", "context": {"cluster": "fixture", "namespace": "demo"}}
                    ],
                    "users": [],
                }
            )
        )
        env = {
            "PATH": os.defpath,
            "HOME": directory,
            "TMPDIR": directory,
            "KUBECONFIG": str(config),
            "LANG": "C",
            "LC_ALL": "C",
        }
        base = [
            kubectl,
            "--kubeconfig",
            str(config),
            "--context=fixture",
            "--server",
            endpoint,
            "--namespace=demo",
            "--cache-dir",
            str(root / "cache"),
            "--request-timeout=3s",
        ]

        def run(args, kill=False):
            started = time.monotonic()
            process = subprocess.Popen(
                base + args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
            )
            try:
                if kill:
                    assert fixture.persisted.wait(5), "PATCH did not reach kill seam"
                    process.kill()
                    fixture.release.set()
                out, err = process.communicate(timeout=8)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate(timeout=2)
            record = {
                "argv": args,
                "exit": process.returncode,
                "stdout": out,
                "stderr": err,
                "killed": kill,
                "elapsed_s": round(time.monotonic() - started, 3),
            }
            fixture.calls.append(record)
            return record

        try:
            yield fixture, run
        finally:
            fixture.release.set()
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)


def experiment(kubectl, case):
    fault = (
        case
        if case in ("lost-response", "kill-after-persistence", "stale-between-get-patch")
        else None
    )
    with environment(kubectl, fault) as (fixture, run):
        intended = {**facts(fixture.obj), "image": IMAGE}
        initial = run(
            ["set", "image", "deployment/api", "api=" + IMAGE],
            kill=case == "kill-after-persistence",
        )
        if fault == "lost-response":
            assert initial["exit"] == 1 and "EOF" in initial["stderr"]
        elif fault == "kill-after-persistence":
            assert initial["exit"] == -9
        else:
            assert initial["exit"] == 0 and "image updated" in initial["stdout"]
        assert facts(fixture.obj)["image"] == IMAGE
        if case == "stale-between-get-patch":
            assert fixture.effects[0]["actor"] == "fixture-writer"
            assert fixture.effects[1]["before"]["image"] == OTHER_IMAGE
            assert fixture.effects[1]["before"]["resourceVersion"] != intended["resourceVersion"]
        if case in ("intervening-writer", "recreated-same-revision"):
            fixture.writer(
                recreate=case == "recreated-same-revision",
                same_revision=case == "recreated-same-revision",
            )
        elif case != "pending-no-watch":
            fixture.settle()
        status = run(["rollout", "status", "deployment/api", "--watch=false", "--timeout=2s"])
        assert status["exit"] == 0
        if case == "pending-no-watch":
            assert "Waiting for" in status["stdout"]
            assert "successfully rolled out" not in status["stdout"]
            assert facts(fixture.obj)["availableReplicas"] == 0
        else:
            assert "successfully rolled out" in status["stdout"]
        if case in ("intervening-writer", "recreated-same-revision", "healthy"):
            pinned = run(["rollout", "status", "deployment/api", "--revision=2", "--timeout=2s"])
            if case == "intervening-writer":
                assert pinned["exit"] != 0 and "different from" in pinned["stderr"]
            else:
                assert pinned["exit"] == 0 and "successfully rolled out" in pinned["stdout"]
        patches = [r for r in fixture.requests if r["method"] == "PATCH"]
        assert len(patches) == 1, "unexpected retry or mutation during status reconstruction"
        assert not fixture.errors, fixture.errors
        return {
            "case": case,
            "intended": intended,
            "caller": fixture.calls,
            "receiver_requests": fixture.requests,
            "fixture_effects": fixture.effects,
            "retained_state": {
                "caller": "argv and captured output only, no action journal",
                "receiver": facts(fixture.obj),
            },
            "reported_conclusion": status["stdout"].strip(),
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", choices=CASES, help="replay one case instead of the full corpus")
    args = parser.parse_args()
    kubectl = shutil.which("kubectl")
    if kubectl is None:
        raise SystemExit("kubectl v1.33.9 is required")
    version = json.loads(
        subprocess.check_output([kubectl, "version", "--client", "-o", "json"], timeout=5)
    )
    client = version["clientVersion"]
    if (client["gitVersion"], client["gitCommit"]) != (VERSION, COMMIT):
        raise SystemExit(f"requires kubectl {VERSION} at {COMMIT}")
    started = time.monotonic()
    cases = [experiment(kubectl, case) for case in ((args.case,) if args.case else CASES)]
    print(
        json.dumps(
            {"client": client, "cases": cases, "elapsed_s": round(time.monotonic() - started, 3)},
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
