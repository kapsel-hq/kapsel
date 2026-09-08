#!/usr/bin/env python3
"""Execute the extracted Kapsel release artifact in a clean Linux environment."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import http.server
import io
import json
import os
import pathlib
import shutil
import sqlite3
import stat
import subprocess
import tarfile
import tempfile
import threading
import time

IMAGE = (
    "registry.example/kapsel/agent-api@sha256:"
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
)
OLD_IMAGE = (
    "registry.example/kapsel/agent-api@sha256:"
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
)
OPERATION = "artifact-op-1"
TARGET = "x86_64-unknown-linux-gnu"
BUILDER_IMAGE = (
    "rust@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922"
)
SMOKE_IMAGE = (
    "python@sha256:86adf8dbadc3d6e82ee5dd2c74bec2e1c2467cdad47886280501df722372d2e1"
)
NON_CLAIMS = "developer-beta;not-production;no-public-rust-api;no-other-targets"
SBOM_GENERATOR = "kapsel-release-sbom/1"
ARCHIVE_BYTES_MAX = 32 * 1024 * 1024
EXPANDED_BYTES_MAX = 64 * 1024 * 1024
FILE_BYTES_MAX = 32 * 1024 * 1024
SBOM_BYTES_MAX = 2 * 1024 * 1024
MANIFEST_BYTES_MAX = 1024
TAR_STREAM_BYTES_MAX = EXPANDED_BYTES_MAX + 64 * 1024
AUTHORIZATION_PUBLIC_KEY = bytes.fromhex(
    "fd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f618"
)
FORBIDDEN = [
    b"SECRET_PROVIDER_CANARY",
    bytes([9]) * 32,
    b"KUBECONFIG_AMBIENT_CANARY",
]


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
            raise RuntimeError("release input is not a bounded regular file")
        data = source.read(maximum + 1)
    if len(data) > maximum:
        raise RuntimeError("release input exceeded its byte bound")
    return data


def verify_checksum(archive: pathlib.Path, checksum: pathlib.Path) -> tuple[bytes, bytes]:
    checksum_bytes = read_bounded_regular(checksum, 256)
    archive_bytes = read_bounded_regular(archive, ARCHIVE_BYTES_MAX)
    expected = f"{hashlib.sha256(archive_bytes).hexdigest()}  {archive.name}\n".encode()
    if checksum_bytes != expected:
        raise RuntimeError("release archive checksum mismatch")
    return archive_bytes, checksum_bytes


def verify_digest_manifest(
    archive: pathlib.Path,
    checksum: pathlib.Path,
    sbom: pathlib.Path,
    manifest: pathlib.Path,
    archive_bytes: bytes,
    checksum_bytes: bytes,
) -> bytes:
    sbom_bytes = read_bounded_regular(sbom, SBOM_BYTES_MAX)
    manifest_bytes = read_bounded_regular(manifest, MANIFEST_BYTES_MAX)
    entries = sorted(
        [
            (archive.name, hashlib.sha256(archive_bytes).hexdigest()),
            (checksum.name, hashlib.sha256(checksum_bytes).hexdigest()),
            (sbom.name, hashlib.sha256(sbom_bytes).hexdigest()),
        ]
    )
    expected = "".join(f"{digest}  {name}\n" for name, digest in entries).encode()
    if manifest_bytes != expected:
        raise RuntimeError("release digest manifest mismatch")
    return sbom_bytes


def validate_sbom(
    archive: pathlib.Path,
    archive_bytes: bytes,
    sbom_bytes: bytes,
    metadata: dict[str, object],
) -> None:
    if not sbom_bytes.endswith(b"\n"):
        raise RuntimeError("release SBOM has no trailing newline")
    sbom = json.loads(sbom_bytes)
    if sbom.get("spdxVersion") != "SPDX-2.3" or sbom.get("dataLicense") != "CC0-1.0":
        raise RuntimeError("release SBOM SPDX identity changed")
    if sbom.get("SPDXID") != "SPDXRef-DOCUMENT":
        raise RuntimeError("release SBOM document identity changed")
    creation = sbom.get("creationInfo")
    if not isinstance(creation, dict) or creation.get("creators") != [f"Tool: {SBOM_GENERATOR}"]:
        raise RuntimeError("release SBOM generator identity changed")
    created = creation.get("created")
    if not isinstance(created, str) or not created.endswith("Z"):
        raise RuntimeError("release SBOM normalized creation time is invalid")
    archive_digest = hashlib.sha256(archive_bytes).hexdigest()
    expected_namespace = (
        "https://github.com/kapsel-cloud/kapsel/sbom/"
        f"{metadata['source_revision']}/{archive_digest}"
    )
    if sbom.get("documentNamespace") != expected_namespace:
        raise RuntimeError("release SBOM namespace disagrees with archive identity")
    comment = sbom.get("comment")
    required_comment_facts = [
        f"source_revision={metadata['source_revision']}",
        f"source_tree={metadata['source_tree']}",
        f"rust_target={TARGET}",
        f"builder_image={BUILDER_IMAGE}",
        f"cargo_lock_sha256={metadata['cargo_lock_sha256']}",
        f"cargo_graph_sha256={metadata['cargo_graph_sha256']}",
    ]
    if not isinstance(comment, str) or any(fact not in comment for fact in required_comment_facts):
        raise RuntimeError("release SBOM source or builder binding changed")
    packages = sbom.get("packages")
    if not isinstance(packages, list) or len(packages) < 2:
        raise RuntimeError("release SBOM package inventory is incomplete")
    package_ids = [package.get("SPDXID") for package in packages if isinstance(package, dict)]
    if len(package_ids) != len(packages) or len(set(package_ids)) != len(package_ids):
        raise RuntimeError("release SBOM package identities are invalid")
    archive_packages = [
        package for package in packages if package.get("SPDXID") == "SPDXRef-Package-kapsel-archive"
    ]
    root_packages = [
        package for package in packages if package.get("SPDXID") == "SPDXRef-Package-kapsel-source"
    ]
    if len(archive_packages) != 1 or len(root_packages) != 1:
        raise RuntimeError("release SBOM root package identities changed")
    cargo_packages = [
        package
        for package in packages
        if package.get("SPDXID") != "SPDXRef-Package-kapsel-archive"
    ]
    relationships = sbom.get("relationships")
    if not isinstance(relationships, list):
        raise RuntimeError("release SBOM relationships are invalid")
    cargo_relationships = [
        relationship
        for relationship in relationships
        if isinstance(relationship, dict) and relationship.get("relationshipType") == "DEPENDS_ON"
    ]
    graph = {
        "packages": cargo_packages,
        "relationships": cargo_relationships,
        "root_package_id": "SPDXRef-Package-kapsel-source",
    }
    graph_digest = hashlib.sha256(
        json.dumps(graph, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    if (
        len(cargo_packages) != metadata["cargo_package_count"]
        or len(cargo_relationships) != metadata["cargo_relationship_count"]
        or graph_digest != metadata["cargo_graph_sha256"]
    ):
        raise RuntimeError("release SBOM Cargo graph is incomplete")
    archive_package = archive_packages[0]
    if (
        archive_package.get("name") != archive.name
        or archive_package.get("filesAnalyzed") is not False
        or archive_package.get("versionInfo") != metadata["package_version"]
        or archive_package.get("checksums")
        != [{"algorithm": "SHA256", "checksumValue": archive_digest}]
    ):
        raise RuntimeError("release SBOM archive package disagrees")
    if root_packages[0].get("versionInfo") != metadata["package_version"]:
        raise RuntimeError("release SBOM root package version disagrees")
    files = sbom.get("files")
    expected_files = {
        "./bin/kapsel": metadata["ordinary_binary_sha256"],
        "./libexec/kapsel-demo-harness": metadata["demo_binary_sha256"],
    }
    actual_files = {}
    if isinstance(files, list):
        for entry in files:
            if isinstance(entry, dict):
                checksums = entry.get("checksums")
                if isinstance(checksums, list) and len(checksums) == 1:
                    actual_files[entry.get("fileName")] = checksums[0].get("checksumValue")
    if actual_files != expected_files:
        raise RuntimeError("release SBOM binary inventory disagrees")
    contains = {
        relationship.get("relatedSpdxElement")
        for relationship in relationships
        if isinstance(relationship, dict)
        and relationship.get("spdxElementId") == "SPDXRef-Package-kapsel-archive"
        and relationship.get("relationshipType") == "CONTAINS"
    }
    if contains != {"SPDXRef-File-bin-kapsel", "SPDXRef-File-libexec-kapsel-demo-harness"}:
        raise RuntimeError("release SBOM binary containment relationships changed")
    if not any(
        relationship.get("spdxElementId") == "SPDXRef-DOCUMENT"
        and relationship.get("relationshipType") == "DESCRIBES"
        and relationship.get("relatedSpdxElement") == "SPDXRef-Package-kapsel-archive"
        for relationship in relationships
        if isinstance(relationship, dict)
    ):
        raise RuntimeError("release SBOM document relationship changed")


def bounded_ustar_bytes(archive_bytes: bytes, expected_entries: int) -> bytes:
    with gzip.GzipFile(fileobj=io.BytesIO(archive_bytes), mode="rb") as compressed:
        value = compressed.read(TAR_STREAM_BYTES_MAX + 1)
    if len(value) > TAR_STREAM_BYTES_MAX:
        raise RuntimeError("release tar stream exceeds its decompressed bound")
    offset = 0
    entries = 0
    zero_blocks = 0
    while offset + 512 <= len(value):
        header = value[offset : offset + 512]
        if header == bytes(512):
            zero_blocks += 1
            offset += 512
            if zero_blocks == 2:
                break
            continue
        if zero_blocks:
            raise RuntimeError("release tar has data after an end marker")
        entries += 1
        if entries > expected_entries:
            raise RuntimeError("release tar has too many raw entries")
        if header[257:263] != b"ustar\0":
            raise RuntimeError("release tar is not exact USTAR")
        if header[156:157] not in {b"\0", b"0", b"5"}:
            raise RuntimeError("release tar contains an extension, link, or special header")
        size_field = header[124:136].rstrip(b"\0 ")
        if any(character not in b"01234567" for character in size_field):
            raise RuntimeError("release tar size is not canonical octal")
        size = int(size_field or b"0", 8)
        offset += 512 + ((size + 511) // 512) * 512
        if offset > len(value):
            raise RuntimeError("release tar entry exceeds the decompressed stream")
    if entries != expected_entries or zero_blocks != 2 or any(value[offset:]):
        raise RuntimeError("release tar framing or padding is not canonical")
    return value


def validate_archive(archive: pathlib.Path, archive_bytes: bytes) -> dict[str, object]:
    suffix = f"-{TARGET}.tar.gz"
    if not archive.name.startswith("kapsel-") or not archive.name.endswith(suffix):
        raise RuntimeError("release archive name does not identify the supported target")
    version = archive.name[len("kapsel-") : -len(suffix)]
    if not version:
        raise RuntimeError("release archive name has no package version")
    basename = archive.name.removesuffix(".tar.gz")
    expected = {
        f"{basename}/",
        f"{basename}/bin/",
        f"{basename}/bin/kapsel",
        f"{basename}/libexec/",
        f"{basename}/libexec/kapsel-demo-harness",
        f"{basename}/share/",
        f"{basename}/share/kapsel/",
        f"{basename}/share/kapsel/demo-kind-crash-recovery.sh",
        f"{basename}/share/kapsel/kap0038-trust.hex",
        f"{basename}/share/doc/",
        f"{basename}/share/doc/kapsel/",
        f"{basename}/share/doc/kapsel/COMMANDS.md",
        f"{basename}/share/doc/kapsel/EVALUATOR.md",
        f"{basename}/share/doc/kapsel/MCP.md",
        f"{basename}/share/doc/kapsel/PRIVACY.md",
        f"{basename}/share/doc/kapsel/RELEASE.md",
        f"{basename}/share/doc/kapsel/SECURITY.md",
        f"{basename}/share/doc/kapsel/UPGRADE.md",
        f"{basename}/CHANGELOG.md",
        f"{basename}/LICENSE",
        f"{basename}/RELEASE-METADATA.json",
    }
    expected_order = sorted(expected)
    tar_bytes = bounded_ustar_bytes(archive_bytes, len(expected_order))
    evidence_names = {
        f"{basename}/RELEASE-METADATA.json": "metadata",
        f"{basename}/LICENSE": "license",
        f"{basename}/bin/kapsel": "ordinary",
        f"{basename}/libexec/kapsel-demo-harness": "demonstration",
    }
    evidence: dict[str, bytes] = {}
    expanded_size = 0
    entry_count = 0
    with tarfile.open(fileobj=io.BytesIO(tar_bytes), mode="r|") as release:
        for member in release:
            entry_count += 1
            if entry_count > len(expected_order):
                raise RuntimeError("release archive has too many entries")
            canonical_name = member.name + ("/" if member.isdir() else "")
            if canonical_name != expected_order[entry_count - 1]:
                raise RuntimeError("release archive layout or ordering is not canonical")
            path = pathlib.PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts:
                raise RuntimeError("release archive path is unsafe")
            if not (member.isdir() or member.isfile()):
                raise RuntimeError("release archive contains a link or special entry")
            if member.isfile():
                if member.size > FILE_BYTES_MAX:
                    raise RuntimeError("release archive entry exceeds its file bound")
                expanded_size += member.size
                if expanded_size > EXPANDED_BYTES_MAX:
                    raise RuntimeError("release archive exceeds its expanded bound")
            identity = (member.uid, member.gid, member.uname, member.gname, member.mtime)
            if identity != (0, 0, "", "", 0):
                raise RuntimeError("release archive metadata is not normalized")
            executable = member.isdir() or member.name.endswith(
                ("/kapsel", "/kapsel-demo-harness", ".sh")
            )
            expected_mode = 0o755 if executable else 0o644
            if member.mode != expected_mode:
                raise RuntimeError("release archive mode is not canonical")
            evidence_key = evidence_names.get(member.name)
            if evidence_key is not None:
                source = release.extractfile(member)
                if source is None:
                    raise RuntimeError("release archive evidence could not be read")
                value = source.read(member.size + 1)
                if len(value) != member.size:
                    raise RuntimeError("release archive evidence size changed while reading")
                evidence[evidence_key] = value
    if entry_count != len(expected_order) or len(evidence) != len(evidence_names):
        raise RuntimeError("release archive layout or evidence is incomplete")
    metadata_bytes = evidence["metadata"]
    license_bytes = evidence["license"]
    ordinary_bytes = evidence["ordinary"]
    demonstration_bytes = evidence["demonstration"]
    if not metadata_bytes.endswith(b"\n"):
        raise RuntimeError("release metadata has no trailing newline")
    metadata = json.loads(metadata_bytes)
    expected_keys = [
        "artifact_schema",
        "package_version",
        "rust_target",
        "source_revision",
        "source_tree",
        "source_dirty",
        "cargo_lock_sha256",
        "cargo_graph_sha256",
        "cargo_package_count",
        "cargo_relationship_count",
        "license",
        "license_sha256",
        "builder_image",
        "smoke_image",
        "ordinary_binary_bytes",
        "ordinary_binary_sha256",
        "demo_binary_bytes",
        "demo_binary_sha256",
        "non_claims",
    ]
    if list(metadata) != expected_keys:
        raise RuntimeError("release metadata fields or order changed")
    if metadata["artifact_schema"] != "kapsel.release-artifact.v2":
        raise RuntimeError("release metadata schema changed")
    if metadata["package_version"] != version or metadata["rust_target"] != TARGET:
        raise RuntimeError("release metadata disagrees with archive identity")
    revision = metadata["source_revision"]
    invalid_revision = (
        not isinstance(revision, str)
        or len(revision) != 40
        or any(character not in "0123456789abcdef" for character in revision)
    )
    if invalid_revision:
        raise RuntimeError("release source revision is not canonical")
    tree = metadata["source_tree"]
    if (
        not isinstance(tree, str)
        or len(tree) != 40
        or any(character not in "0123456789abcdef" for character in tree)
    ):
        raise RuntimeError("release source tree is not canonical")
    lock_digest = metadata["cargo_lock_sha256"]
    if (
        not isinstance(lock_digest, str)
        or len(lock_digest) != 64
        or any(character not in "0123456789abcdef" for character in lock_digest)
    ):
        raise RuntimeError("release lockfile digest is not canonical")
    graph_digest = metadata["cargo_graph_sha256"]
    if (
        not isinstance(graph_digest, str)
        or len(graph_digest) != 64
        or any(character not in "0123456789abcdef" for character in graph_digest)
    ):
        raise RuntimeError("release Cargo graph digest is not canonical")
    if (
        not isinstance(metadata["cargo_package_count"], int)
        or metadata["cargo_package_count"] < 1
        or not isinstance(metadata["cargo_relationship_count"], int)
        or metadata["cargo_relationship_count"] < 1
    ):
        raise RuntimeError("release Cargo graph counts are invalid")
    if not isinstance(metadata["source_dirty"], bool):
        raise RuntimeError("release dirty state is not boolean")
    license_digest = hashlib.sha256(license_bytes).hexdigest()
    if metadata["license"] != "Apache-2.0" or metadata["license_sha256"] != license_digest:
        raise RuntimeError("release license provenance disagrees")
    if metadata["builder_image"] != BUILDER_IMAGE or metadata["smoke_image"] != SMOKE_IMAGE:
        raise RuntimeError("release container provenance disagrees")
    if metadata["non_claims"] != NON_CLAIMS:
        raise RuntimeError("release non-claims changed")
    if metadata["ordinary_binary_bytes"] != len(ordinary_bytes):
        raise RuntimeError("release ordinary binary size disagrees")
    if metadata["ordinary_binary_sha256"] != hashlib.sha256(ordinary_bytes).hexdigest():
        raise RuntimeError("release ordinary binary digest disagrees")
    if metadata["demo_binary_bytes"] != len(demonstration_bytes):
        raise RuntimeError("release demo binary size disagrees")
    if metadata["demo_binary_sha256"] != hashlib.sha256(demonstration_bytes).hexdigest():
        raise RuntimeError("release demo binary digest disagrees")
    return metadata


def extract_exact_archive(
    archive: pathlib.Path,
    archive_bytes: bytes,
    destination: pathlib.Path,
) -> pathlib.Path:
    with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:gz") as release:
        members = release.getmembers()
        top_levels = {pathlib.PurePosixPath(member.name).parts[0] for member in members}
        if len(top_levels) != 1:
            raise RuntimeError("release archive must have one top-level directory")
        top_level = top_levels.pop()
        for member in members:
            path = pathlib.PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or member.issym() or member.islnk():
                raise RuntimeError("release archive contains an unsafe entry")
            target = destination.joinpath(*path.parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                target.chmod(member.mode)
            elif member.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                source = release.extractfile(member)
                if source is None:
                    raise RuntimeError("release archive file could not be read")
                with target.open("xb") as output:
                    shutil.copyfileobj(source, output)
                target.chmod(member.mode)
            else:
                raise RuntimeError("release archive contains an unsupported entry")
    return destination / top_level


def deployment(resource_version: str, generation: int, observed: bool) -> bytes:
    metadata: dict[str, object] = {
        "name": "agent-api",
        "namespace": "demo",
        "uid": "artifact-deployment-uid",
        "resourceVersion": resource_version,
        "generation": generation,
    }
    value: dict[str, object] = {
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": metadata,
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": "agent-api"}},
            "template": {
                "metadata": {"labels": {"app": "agent-api"}},
                "spec": {
                    "containers": [
                        {
                            "name": "api",
                            "image": OLD_IMAGE if generation == 1 else IMAGE,
                        }
                    ]
                },
            },
        },
    }
    if observed:
        metadata["annotations"] = {"kapsel.dev/kap0038-operation-id": OPERATION}
        value["status"] = {
            "observedGeneration": 2,
            "updatedReplicas": 1,
            "availableReplicas": 1,
            "unavailableReplicas": 0,
            "conditions": [
                {
                    "type": "Available",
                    "status": "True",
                    "reason": "MinimumReplicasAvailable",
                }
            ],
        }
    return json.dumps(value, separators=(",", ":")).encode()


class KubernetesFixture(http.server.BaseHTTPRequestHandler):
    responses: list[bytes] = []
    requests = 0

    def log_message(self, format: str, *arguments: object) -> None:
        del format, arguments

    def respond(self) -> None:
        type(self).requests += 1
        if not type(self).responses:
            self.send_error(500)
            return
        body = type(self).responses.pop(0)
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = respond
    do_PATCH = respond


def reset_kubernetes_fixture() -> None:
    KubernetesFixture.responses = [
        deployment("1", 1, False),
        deployment("2", 2, False),
        deployment("3", 2, True),
    ]
    KubernetesFixture.requests = 0


def write_private(path: pathlib.Path, data: bytes) -> None:
    path.write_bytes(data)
    path.chmod(0o600)


def run_binary(binary: pathlib.Path, arguments: list[str]) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        [str(binary), *arguments],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=60,
        env={"PATH": "/usr/local/bin:/usr/bin:/bin", "KUBECONFIG": "KUBECONFIG_AMBIENT_CANARY"},
    )
    if len(result.stdout) > 64 * 1024 or len(result.stderr) > 4 * 1024:
        raise RuntimeError("installed binary output exceeded its bound")
    for canary in FORBIDDEN:
        if canary in result.stdout or canary in result.stderr:
            raise RuntimeError("installed binary disclosed a canary")
    return result


def prepare_inputs(root: pathlib.Path, server_address: tuple[str, int]) -> dict[str, pathlib.Path]:
    receipts = root / "receipts"
    receipts.mkdir(mode=0o700)
    authorization = root / "authorization.json"
    request = root / "request.json"
    operator = root / "operator.json"
    write_private(
        authorization,
        json.dumps(
            {
                "authorization_id": "artifact-auth-1",
                "operation_id": OPERATION,
                "namespace": "demo",
                "deployment": "agent-api",
                "container": "api",
                "immutable_image_digest": IMAGE,
            },
            separators=(",", ":"),
        ).encode(),
    )
    write_private(
        request,
        json.dumps(
            {
                "operation_id": OPERATION,
                "namespace": "demo",
                "deployment": "agent-api",
                "container": "api",
                "immutable_image_digest": IMAGE,
            },
            separators=(",", ":"),
        ).encode(),
    )
    write_private(root / "authorization.seed", bytes([9]) * 32)
    write_private(root / "authorization.pub", AUTHORIZATION_PUBLIC_KEY)
    write_private(root / "receipt.seed", bytes([9]) * 32)
    host, port = server_address
    write_private(
        root / "kubeconfig.yaml",
        (
            "apiVersion: v1\nkind: Config\nclusters:\n- name: fixture\n"
            f"  cluster:\n    server: http://{host}:{port}\n"
            "contexts:\n- name: fixture\n  context:\n"
            "    cluster: fixture\n    user: fixture\ncurrent-context: fixture\n"
            "users:\n- name: fixture\n  user: {}\n"
        ).encode(),
    )
    return {
        "authorization": authorization,
        "request": request,
        "operator": operator,
        "receipts": receipts,
    }


def write_operator(
    root: pathlib.Path,
    grant: pathlib.Path,
    receipts: pathlib.Path,
    receipt_seed: pathlib.Path,
    receipt_key_id: str,
    output: pathlib.Path,
) -> None:
    write_private(
        output,
        json.dumps(
            {
                "signed_authorization_grant": str(grant),
                "authorization_key_id": "artifact-authorization-key",
                "authorization_public_key": str(root / "authorization.pub"),
                "kubeconfig": str(root / "kubeconfig.yaml"),
                "journal": str(root / "journal.sqlite3"),
                "receipt_directory": str(receipts),
                "receipt_signing_seed": str(receipt_seed),
                "receipt_signing_key_id": receipt_key_id,
            },
            separators=(",", ":"),
        ).encode(),
    )


def provision_and_write_operator(
    binary: pathlib.Path,
    root: pathlib.Path,
    paths: dict[str, pathlib.Path],
) -> None:
    grant = root / "grant.bin"
    provision = run_binary(
        binary,
        [
            "provision-grant",
            "--authorization",
            str(paths["authorization"]),
            "--signing-seed",
            str(root / "authorization.seed"),
            "--signing-key-id",
            "artifact-authorization-key",
            "--output",
            str(grant),
        ],
    )
    if provision.returncode != 0 or b'"status":"PROVISIONED"' not in provision.stdout:
        raise RuntimeError("installed grant provisioning failed")
    write_operator(
        root,
        grant,
        paths["receipts"],
        root / "receipt.seed",
        "kap0038-test-key",
        paths["operator"],
    )


def execute_and_restart(binary: pathlib.Path, paths: dict[str, pathlib.Path]) -> pathlib.Path:
    arguments = [
        "operate",
        "--request",
        str(paths["request"]),
        "--operator-config",
        str(paths["operator"]),
    ]
    first = run_binary(binary, arguments)
    if first.returncode != 0:
        raise RuntimeError("installed operation failed")
    report = json.loads(first.stdout)
    if report["state"] != "FINALIZED" or report["result"] != "SUCCEEDED":
        raise RuntimeError("installed operation returned the wrong outcome")
    restarted = run_binary(binary, arguments)
    if restarted.returncode != 0 or json.loads(restarted.stdout) != report:
        raise RuntimeError("installed ordinary restart changed the report")
    receipts = list(paths["receipts"].glob("*.receipt"))
    if len(receipts) != 1:
        raise RuntimeError("installed operation did not publish exactly one receipt")
    return receipts[0]


def wait_for_marker(process: subprocess.Popen[bytes], marker: pathlib.Path) -> None:
    deadline = time.monotonic() + 15
    while not marker.exists():
        if process.poll() is not None:
            raise RuntimeError("installed demo process exited before its marker")
        if time.monotonic() >= deadline:
            raise RuntimeError("installed demo marker timed out")
        time.sleep(0.02)


def kill_demo_at_seam(
    binary: pathlib.Path,
    paths: dict[str, pathlib.Path],
    control: pathlib.Path,
    seam: str,
    marker: pathlib.Path,
    log: pathlib.Path,
) -> None:
    environment = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "KAPSEL_DEMO_CONTROL_DIRECTORY": str(control),
        "KAPSEL_DEMO_PAUSE": seam,
    }
    with log.open("wb") as output:
        process = subprocess.Popen(
            [
                str(binary),
                "operate",
                "--request",
                str(paths["request"]),
                "--operator-config",
                str(paths["operator"]),
            ],
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=subprocess.STDOUT,
            env=environment,
        )
        try:
            wait_for_marker(process, marker)
            process.kill()
            process.wait(timeout=5)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
    if process.returncode == 0 or log.stat().st_size > 64 * 1024:
        raise RuntimeError("installed demo seam did not fail within bounds")


def exercise_demo_binary(
    binary: pathlib.Path,
    artifact_root: pathlib.Path,
    temporary_root: pathlib.Path,
) -> None:
    reset_kubernetes_fixture()
    fixture = http.server.ThreadingHTTPServer(("127.0.0.1", 0), KubernetesFixture)
    thread = threading.Thread(target=fixture.serve_forever, daemon=True)
    thread.start()
    evaluation = temporary_root / "demo-evaluation"
    evaluation.mkdir(mode=0o700)
    control = evaluation / "control"
    control.mkdir(mode=0o700)
    try:
        paths = prepare_inputs(evaluation, fixture.server_address)
        provision_and_write_operator(binary, evaluation, paths)
        kill_demo_at_seam(
            binary,
            paths,
            control,
            "after_apply",
            control / "after-apply.ready",
            evaluation / "after-apply.log",
        )
        if control.joinpath("provider-apply-count").read_text() != "1":
            raise RuntimeError("installed demo repeated its provider mutation")
        kill_demo_at_seam(
            binary,
            paths,
            control,
            "after_receipt_commit",
            control / "after-receipt-commit.ready",
            evaluation / "after-commit.log",
        )
        if KubernetesFixture.requests != 3:
            raise RuntimeError("installed demo recovery repeated provider activity")
        if any(paths["receipts"].iterdir()):
            raise RuntimeError("installed demo exported a receipt before restart")
        with sqlite3.connect(evaluation / "journal.sqlite3") as connection:
            rows = connection.execute(
                "SELECT receipt_bytes FROM kubernetes_image_operations "
                "WHERE state = 'finalized'"
            ).fetchall()
        if len(rows) != 1 or not isinstance(rows[0][0], bytes):
            raise RuntimeError("installed demo did not commit one frozen receipt")
        frozen = rows[0][0]

        rotated_receipts = evaluation / "rotated-receipts"
        rotated_receipts.mkdir(mode=0o700)
        rotated_seed = evaluation / "rotated.seed"
        write_private(rotated_seed, bytes([8]) * 32)
        rotated_operator = evaluation / "rotated-operator.json"
        write_operator(
            evaluation,
            evaluation / "grant.bin",
            rotated_receipts,
            rotated_seed,
            "rotated-receipt-key",
            rotated_operator,
        )
        finalized = run_binary(
            binary,
            [
                "operate",
                "--request",
                str(paths["request"]),
                "--operator-config",
                str(rotated_operator),
            ],
        )
        final_report = json.loads(finalized.stdout) if finalized.returncode == 0 else {}
        if final_report.get("state") != "FINALIZED" or final_report.get("result") != "SUCCEEDED":
            raise RuntimeError("installed demo did not finalize the receiver outcome after restart")
        if control.joinpath("provider-apply-count").read_text() != "1":
            raise RuntimeError("installed demo changed its provider apply count")
        receipts = list(rotated_receipts.glob("*.receipt"))
        if (
            len(receipts) != 1
            or receipts[0].read_bytes() != frozen
            or any(paths["receipts"].iterdir())
        ):
            raise RuntimeError("installed demo changed the committed receipt during export")
        trust = evaluation / "receipt.trust"
        trust_hex = artifact_root.joinpath(
            "share", "kapsel", "kap0038-trust.hex"
        ).read_text().strip()
        write_private(trust, bytes.fromhex(trust_hex))
        inspect_receipt(binary, receipts[0], trust)
    finally:
        fixture.shutdown()
        fixture.server_close()
        thread.join(timeout=5)
        shutil.rmtree(evaluation)


def inspect_receipt(binary: pathlib.Path, receipt: pathlib.Path, trust: pathlib.Path) -> None:
    inspection = run_binary(
        binary,
        [
            "inspect",
            "--receipt",
            str(receipt),
            "--trust",
            str(trust),
            "--evaluation-time-unix-s",
            "150",
        ],
    )
    if inspection.returncode != 0:
        raise RuntimeError("installed offline inspection failed")
    report = json.loads(inspection.stdout)
    if report["status"] != "INSPECTED" or report["result"] != "SUCCEEDED":
        raise RuntimeError("installed offline inspection returned the wrong result")
    if b"VERIFIED" in inspection.stdout:
        raise RuntimeError("installed offline inspection emitted forbidden vocabulary")


def exercise_mcp(binary: pathlib.Path, operator: pathlib.Path, version: str) -> None:
    messages = [
        {
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "artifact-smoke", "version": "1"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        {
            "jsonrpc": "2.0",
            "id": "call",
            "method": "tools/call",
            "params": {
                "name": "kubernetes.set_deployment_image",
                "arguments": {
                    "operation_id": OPERATION,
                    "namespace": "demo",
                    "deployment": "agent-api",
                    "container": "api",
                    "immutable_image_digest": IMAGE,
                },
            },
        },
    ]
    input_bytes = b"".join(
        json.dumps(message, separators=(",", ":")).encode() + b"\n" for message in messages
    )
    process = subprocess.run(
        [str(binary), "mcp", "--operator-config", str(operator)],
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=30,
        env={"PATH": "/usr/local/bin:/usr/bin:/bin"},
    )
    if process.returncode != 0 or process.stderr:
        raise RuntimeError("installed MCP lifecycle failed")
    responses = [json.loads(line) for line in process.stdout.splitlines()]
    if responses[0]["result"]["serverInfo"] != {"name": "kapsel", "version": version}:
        raise RuntimeError("installed MCP version disagrees with artifact metadata")
    tools = responses[1]["result"]["tools"]
    if len(tools) != 1 or tools[0]["name"] != "kubernetes.set_deployment_image":
        raise RuntimeError("installed MCP tool list is not fixed")
    properties = set(tools[0]["inputSchema"]["properties"])
    expected = {
        "operation_id",
        "namespace",
        "deployment",
        "container",
        "immutable_image_digest",
    }
    if properties != expected:
        raise RuntimeError("installed MCP tool schema exposed the wrong fields")
    call = responses[2]["result"]
    if call["isError"] or json.loads(call["content"][0]["text"])["result"] != "SUCCEEDED":
        raise RuntimeError("installed MCP call changed the application outcome")


def exercise_version(binary: pathlib.Path, expected_version: object) -> None:
    if not isinstance(expected_version, str):
        raise RuntimeError("release metadata package version is invalid")
    result = subprocess.run(
        [str(binary), "--version"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={},
    )
    if result.stdout != f"kapsel {expected_version}\n".encode() or result.stderr:
        raise RuntimeError("installed executable version identity disagrees")


def smoke(
    archive: pathlib.Path,
    checksum: pathlib.Path,
    expected_revision: str | None = None,
) -> None:
    archive_bytes, checksum_bytes = verify_checksum(archive, checksum)
    sbom = archive.with_name(archive.name + ".spdx.json")
    manifest = archive.with_name(archive.name + ".SHA256SUMS")
    sbom_bytes = verify_digest_manifest(
        archive, checksum, sbom, manifest, archive_bytes, checksum_bytes
    )
    metadata = validate_archive(archive, archive_bytes)
    validate_sbom(archive, archive_bytes, sbom_bytes, metadata)
    if expected_revision is not None and metadata["source_revision"] != expected_revision:
        raise RuntimeError("release source revision disagrees with the expected revision")
    with tempfile.TemporaryDirectory(prefix="kapsel-clean-smoke-") as temporary:
        root = extract_exact_archive(archive, archive_bytes, pathlib.Path(temporary))
        extracted_binary = root / "bin" / "kapsel"
        installation = pathlib.Path(temporary) / "installation"
        installation.mkdir(mode=0o755)
        binary = installation / "kapsel"
        shutil.copyfile(extracted_binary, binary)
        binary.chmod(0o755)
        if sha256(binary) != metadata["ordinary_binary_sha256"]:
            raise RuntimeError("installed ordinary binary digest mismatch")
        if sha256(root / "libexec" / "kapsel-demo-harness") != metadata["demo_binary_sha256"]:
            raise RuntimeError("installed demo binary digest mismatch")
        exercise_version(binary, metadata["package_version"])

        reset_kubernetes_fixture()
        fixture = http.server.ThreadingHTTPServer(("127.0.0.1", 0), KubernetesFixture)
        thread = threading.Thread(target=fixture.serve_forever, daemon=True)
        thread.start()
        evaluation = pathlib.Path(temporary) / "evaluation"
        evaluation.mkdir(mode=0o700)
        try:
            paths = prepare_inputs(evaluation, fixture.server_address)
            provision_and_write_operator(binary, evaluation, paths)
            receipt = execute_and_restart(binary, paths)
            if KubernetesFixture.requests != 3:
                raise RuntimeError("installed ordinary restart repeated provider activity")
            trust = evaluation / "receipt.trust"
            trust_hex = root.joinpath("share", "kapsel", "kap0038-trust.hex").read_text().strip()
            write_private(trust, bytes.fromhex(trust_hex))
            inspect_receipt(binary, receipt, trust)
            exercise_mcp(binary, paths["operator"], metadata["package_version"])
        finally:
            fixture.shutdown()
            fixture.server_close()
            thread.join(timeout=5)
        shutil.rmtree(evaluation)
        if evaluation.exists():
            raise RuntimeError("artifact smoke did not clean its evaluation directory")
        exercise_demo_binary(
            root / "libexec" / "kapsel-demo-harness",
            root,
            pathlib.Path(temporary),
        )
        binary.unlink()
        installation.rmdir()
        if installation.exists():
            raise RuntimeError("artifact smoke did not uninstall the ordinary binary")


def run_live_demo(archive: pathlib.Path, checksum: pathlib.Path) -> None:
    archive_bytes, checksum_bytes = verify_checksum(archive, checksum)
    sbom = archive.with_name(archive.name + ".spdx.json")
    manifest = archive.with_name(archive.name + ".SHA256SUMS")
    sbom_bytes = verify_digest_manifest(
        archive, checksum, sbom, manifest, archive_bytes, checksum_bytes
    )
    metadata = validate_archive(archive, archive_bytes)
    validate_sbom(archive, archive_bytes, sbom_bytes, metadata)
    with tempfile.TemporaryDirectory(prefix="kapsel-live-artifact-") as temporary:
        root = extract_exact_archive(archive, archive_bytes, pathlib.Path(temporary))
        subprocess.run(
            [str(root / "share" / "kapsel" / "demo-kind-crash-recovery.sh")],
            cwd=root,
            check=True,
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=pathlib.Path)
    parser.add_argument("--expected-revision")
    parser.add_argument("--live-demo", action="store_true")
    arguments = parser.parse_args()
    archive = pathlib.Path(os.path.abspath(arguments.archive))
    checksum = archive.with_name(archive.name + ".sha256")
    if arguments.live_demo:
        run_live_demo(archive, checksum)
        print("Kapsel release artifact live demo: ok")
    else:
        smoke(archive, checksum, arguments.expected_revision)
        print("Kapsel release artifact smoke: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
