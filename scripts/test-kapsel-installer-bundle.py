#!/usr/bin/env python3
"""Stage the test bundle and launch named Linux installer integration tests."""

from __future__ import annotations

import base64
import hashlib
import os
import pathlib
import secrets
import shutil
import struct
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-gnu"
BUILDER_IMAGE = "rust@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922"
TOOLCHAIN = "1.98.0"


def cache_key() -> str:
    """Bind reusable build state to the exact Linux build inputs."""
    lock_digest = hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest()
    material = f"{BUILDER_IMAGE}|{TOOLCHAIN}|{TARGET}|{lock_digest}".encode()
    return hashlib.sha256(material).hexdigest()[:16]


def cache_paths() -> tuple[pathlib.Path, pathlib.Path, pathlib.Path]:
    """Create build-only caches without retaining disposable test evidence."""
    base = (
        pathlib.Path(
            os.environ.get(
                "KAPSEL_INSTALLER_CACHE_DIR",
                pathlib.Path.home() / ".cache/kapsel/installer",
            )
        )
        / cache_key()
    )
    paths = (base / "rustup", base / "registry", base / "target")
    for path in paths:
        path.mkdir(parents=True, exist_ok=True)
    return paths


def test_elf() -> bytes:
    """Return a minimal structurally valid ELF64 fixture; it is never executed."""
    value = bytearray(120)
    value[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HHI", value, 16, 3, 62, 1)
    struct.pack_into("<Q", value, 32, 64)
    struct.pack_into("<HHH", value, 52, 64, 56, 1)
    struct.pack_into("<II", value, 64, 1, 1)
    struct.pack_into("<QQQ", value, 72, 0, 0, 0)
    struct.pack_into("<QQ", value, 96, len(value), len(value))
    struct.pack_into("<Q", value, 112, 4096)
    return bytes(value)


def copy(source: pathlib.Path, destination: pathlib.Path, mode: int) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
    shutil.copyfile(source, destination)
    destination.chmod(mode)


def stage_bundle(stage: pathlib.Path) -> None:
    stage.chmod(0o755)
    for path in [
        "usr/bin/kapsel",
        "usr/bin/kapsel-service-client",
        "usr/libexec/kapsel/kapseld",
    ]:
        destination = stage / path
        destination.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
        destination.write_bytes(test_elf())
        destination.chmod(0o755)
    for source, destination in [
        ("crates/kapseld/deploy/kapseld.service", "usr/lib/systemd/system/kapseld.service"),
        ("crates/kapseld/deploy/kapseld.conf", "usr/lib/sysusers.d/kapseld.conf"),
        ("crates/kapseld/deploy/kapseld-rbac.yaml", "usr/share/kapsel/kapseld-rbac.yaml"),
        (
            "docs/KAPSEL_SERVICE_OPERATOR.md",
            "usr/share/doc/kapsel/KAPSEL_SERVICE_OPERATOR.md",
        ),
        ("LICENSE", "LICENSE"),
    ]:
        copy(ROOT / source, stage / destination, 0o644)
    metadata = stage / "KAPSEL-SERVICE-METADATA.json"
    metadata.write_text('{"fixture":"test-only;not-candidate-metadata"}\n')
    metadata.chmod(0o644)
    for directory in [stage, *[path for path in stage.rglob("*") if path.is_dir()]]:
        directory.chmod(0o755)


def operator_input(directory: pathlib.Path, certificate_authority: bytes) -> None:
    directory.chmod(0o700)
    files = {
        "grant.bin": bytes.fromhex((ROOT / "vectors/effect-gateway-grant.hex").read_text().strip()),
        "authorization.pub": bytes.fromhex(
            "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
        ),
        "receipt.seed": bytes([9]) * 32,
        "receipt.trust": bytes.fromhex(
            (ROOT / "vectors/effect-gateway-trust.hex").read_text().strip()
        ),
        "bootstrap-kubeconfig.yaml": f"""apiVersion: v1
kind: Config
clusters:
- name: fixture
  cluster:
    server: https://127.0.0.1:6443
    certificate-authority-data: {base64.b64encode(certificate_authority).decode()}
users:
- name: fixture
  user:
    token: fixture-token
contexts:
- name: nonprod
  context:
    cluster: fixture
    user: fixture
    namespace: demo
current-context: nonprod
""".encode(),
    }
    for name, contents in files.items():
        path = directory / name
        path.write_bytes(contents)
        path.chmod(0o600)


def generate_tls_fixture(target: pathlib.Path) -> None:
    openssl = {
        "check": True,
        "stdout": subprocess.DEVNULL,
        "stderr": subprocess.DEVNULL,
    }
    subprocess.run(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            target / "ca.key",
            "-out",
            target / "ca.crt",
            "-days",
            "1",
            "-subj",
            "/CN=kapsel-test-ca",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ],
        **openssl,
    )
    subprocess.run(
        [
            "openssl",
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            target / "kube.key",
            "-out",
            target / "kube.csr",
            "-subj",
            "/CN=127.0.0.1",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
        ],
        **openssl,
    )
    (target / "kube.ext").write_text(
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\n"
        "extendedKeyUsage=serverAuth\n"
        "subjectAltName=IP:127.0.0.1\n"
    )
    subprocess.run(
        [
            "openssl",
            "x509",
            "-req",
            "-in",
            target / "kube.csr",
            "-CA",
            target / "ca.crt",
            "-CAkey",
            target / "ca.key",
            "-CAcreateserial",
            "-out",
            target / "kube.crt",
            "-days",
            "1",
            "-extfile",
            target / "kube.ext",
        ],
        **openssl,
    )
    subprocess.run(
        [
            "openssl",
            "x509",
            "-in",
            target / "kube.crt",
            "-outform",
            "DER",
            "-out",
            target / "kube.der",
        ],
        **openssl,
    )
    subprocess.run(
        [
            "openssl",
            "pkcs8",
            "-topk8",
            "-nocrypt",
            "-in",
            target / "kube.key",
            "-outform",
            "DER",
            "-out",
            target / "kube-key.der",
        ],
        **openssl,
    )


def main() -> int:
    if shutil.which("docker") is None or shutil.which("openssl") is None:
        raise RuntimeError("Docker and openssl are required")
    subprocess.run(["docker", "info"], check=True, stdout=subprocess.DEVNULL, timeout=30)
    with (
        tempfile.TemporaryDirectory(prefix="kapsel-installer-stage-") as stage_name,
        tempfile.TemporaryDirectory(prefix="kapsel-installer-target-") as target_name,
        tempfile.TemporaryDirectory(prefix="kapsel-installer-input-") as operator_name,
    ):
        stage = pathlib.Path(stage_name)
        target = pathlib.Path(target_name)
        operator = pathlib.Path(operator_name)
        stage_bundle(stage)
        generate_tls_fixture(target)
        operator_input(operator, (target / "ca.crt").read_bytes())
        rustup_cache, registry_cache, target_cache = cache_paths()
        container = f"kapsel-installer-bundle-{os.getpid()}-{secrets.token_hex(4)}"
        test_command = (
            "set -eu; "
            "trap 'chown -R \"$HOST_UID:$HOST_GID\" /target' EXIT; "
            f"cargo test --release --locked --target {TARGET} -p kapsel-installer "
            "--bin kapsel-installer; "
            f"cargo test --release --locked --target {TARGET} -p kapsel-installer "
            "--test linux_installer_scenarios -- --ignored --test-threads=1"
        )
        command = [
            "docker",
            "run",
            "--rm",
            "--name",
            container,
            "--platform",
            "linux/amd64",
            "--volume",
            f"{ROOT}:/workspace:ro",
            "--volume",
            f"{stage}:/stage:ro",
            "--volume",
            f"{target}:/target",
            "--volume",
            f"{operator}:/operator-fixture:ro",
            "--volume",
            f"{rustup_cache}:/usr/local/rustup",
            "--volume",
            f"{registry_cache}:/usr/local/cargo/registry",
            "--volume",
            f"{target_cache}:/cargo-target",
            "--workdir",
            "/workspace",
            "--env",
            "CARGO_TARGET_DIR=/cargo-target",
            "--env",
            "KAPSEL_INSTALLER_STAGE=/stage",
            "--env",
            "KAPSEL_INSTALLER_TEST_CRASH_SEAMS=1",
            "--env",
            f"HOST_UID={os.getuid()}",
            "--env",
            f"HOST_GID={os.getgid()}",
            BUILDER_IMAGE,
            "sh",
            "-c",
            test_command,
        ]
        try:
            subprocess.run(command, cwd=ROOT, check=True, timeout=2400)
        finally:
            subprocess.run(
                ["docker", "rm", "--force", container],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=30,
                check=False,
            )
    print("named Linux installer scenarios: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
