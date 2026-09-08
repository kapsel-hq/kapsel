#!/usr/bin/env python3
"""Assemble the fixed Kapsel x86-64 GNU/Linux release artifact."""

from __future__ import annotations

import argparse
import datetime
import gzip
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-gnu"
BUILDER_IMAGE = "rust@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922"
SMOKE_IMAGE = "python@sha256:86adf8dbadc3d6e82ee5dd2c74bec2e1c2467cdad47886280501df722372d2e1"
NON_CLAIMS = "developer-beta;not-production;no-public-rust-api;no-other-targets"
SBOM_GENERATOR = "kapsel-release-sbom/1"
ARCHIVE_BYTES_MAX = 32 * 1024 * 1024
EXPANDED_BYTES_MAX = 64 * 1024 * 1024
SBOM_BYTES_MAX = 2 * 1024 * 1024
MANIFEST_BYTES_MAX = 1024


def run(*arguments: str, cwd: pathlib.Path = ROOT) -> str:
    result = subprocess.run(
        arguments,
        cwd=cwd,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return result.stdout.strip()


def file_sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def package_version() -> str:
    manifest = tomllib.loads(ROOT.joinpath("Cargo.toml").read_text())
    return str(manifest["workspace"]["package"]["version"])


def canonical_sha1(value: str, label: str) -> str:
    if len(value) != 40 or any(character not in "0123456789abcdef" for character in value):
        raise RuntimeError(f"{label} is not canonical lowercase SHA-1")
    return value


def git_provenance(allow_dirty: bool) -> tuple[str, str, str, bool]:
    revision = canonical_sha1(run("git", "rev-parse", "HEAD"), "source revision")
    tree = canonical_sha1(run("git", "rev-parse", "HEAD^{tree}"), "source tree")
    committed = datetime.datetime.fromisoformat(run("git", "show", "-s", "--format=%cI", "HEAD"))
    committed_utc = committed.astimezone(datetime.timezone.utc).replace(microsecond=0)
    source_date = committed_utc.isoformat().replace("+00:00", "Z")
    dirty = bool(run("git", "status", "--porcelain=v1", "--untracked-files=all"))
    if dirty and not allow_dirty:
        raise RuntimeError("release assembly requires a clean worktree")
    return revision, tree, source_date, dirty


def build_binaries(
    target_directory: pathlib.Path,
) -> tuple[pathlib.Path, pathlib.Path, pathlib.Path]:
    # Linux bind mounts retain container ownership. Restore the host owner before temporary cleanup.
    build_script = f"""
        set -eu
        restore_target_ownership() {{
            chown -R "$HOST_UID:$HOST_GID" /target
        }}
        trap restore_target_ownership EXIT
        cargo metadata --locked --format-version 1 > /target/cargo-metadata.json
        cargo build --release --locked --target {TARGET} --bin kapsel
        cp /target/{TARGET}/release/kapsel /target/ordinary-kapsel
        cargo build --release --locked --target {TARGET} --bin kapsel --features demo-harness
        cp /target/{TARGET}/release/kapsel /target/demo-kapsel
    """
    command = [
        "docker",
        "run",
        "--rm",
        "--platform",
        "linux/amd64",
        "--volume",
        f"{ROOT}:/workspace:ro",
        "--volume",
        f"{target_directory}:/target",
        "--workdir",
        "/workspace",
        "--env",
        "CARGO_TARGET_DIR=/target",
        "--env",
        "RUSTFLAGS=--remap-path-prefix=/workspace=.",
        "--env",
        f"HOST_UID={os.getuid()}",
        "--env",
        f"HOST_GID={os.getgid()}",
        BUILDER_IMAGE,
        "sh",
        "-eu",
        "-c",
        build_script,
    ]
    subprocess.run(command, cwd=ROOT, check=True)
    ordinary = target_directory / "ordinary-kapsel"
    demonstration = target_directory / "demo-kapsel"
    metadata = target_directory / "cargo-metadata.json"
    if not ordinary.is_file() or not demonstration.is_file() or not metadata.is_file():
        raise RuntimeError("Cargo did not produce the expected release build outputs")
    return ordinary, demonstration, metadata


def copy_file(source: pathlib.Path, destination: pathlib.Path, mode: int) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(mode)


def write_exclusive(path: pathlib.Path, value: bytes, mode: int = 0o644) -> None:
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        mode,
    )
    with os.fdopen(descriptor, "wb") as output:
        output.write(value)


def copy_document(source: pathlib.Path, destination: pathlib.Path, revision: str) -> None:
    def absolute_link(match: re.Match[str]) -> str:
        target = match.group(1)
        if target.startswith(("#", "http://", "https://", "mailto:")):
            return match.group(0)
        path_text, separator, fragment = target.partition("#")
        resolved = (source.parent / path_text).resolve()
        try:
            repository_path = resolved.relative_to(ROOT)
        except ValueError as error:
            raise RuntimeError(f"bundled document link escapes the repository: {target}") from error
        if not resolved.is_file():
            raise RuntimeError(f"bundled document link target is missing: {target}")
        suffix = f"#{fragment}" if separator else ""
        url = (
            "https://github.com/kapsel-cloud/kapsel/blob/"
            f"{revision}/{repository_path.as_posix()}{suffix}"
        )
        return f"]({url})"

    text = re.sub(r"]\(([^)\s]+[.]md(?:#[^)]+)?)\)", absolute_link, source.read_text())
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(text)
    destination.chmod(0o644)


def stage_release(
    staging: pathlib.Path,
    revision: str,
    tree: str,
    dirty: bool,
) -> dict[str, object]:
    with tempfile.TemporaryDirectory(prefix="kapsel-release-target-") as temporary:
        ordinary, demonstration, cargo_metadata_path = build_binaries(pathlib.Path(temporary))
        cargo_metadata = json.loads(cargo_metadata_path.read_text())
        copy_file(ordinary, staging / "bin" / "kapsel", 0o755)
        copy_file(demonstration, staging / "libexec" / "kapsel-demo-harness", 0o755)

    assets = {
        ROOT / "scripts" / "demo-kind-crash-recovery.sh": (
            staging / "share" / "kapsel" / "demo-kind-crash-recovery.sh",
            0o755,
        ),
        ROOT / "vectors" / "effect-gateway-trust.hex": (
            staging / "share" / "kapsel" / "kap0038-trust.hex",
            0o644,
        ),
        ROOT / "docs" / "COMMANDS.md": (
            staging / "share" / "doc" / "kapsel" / "COMMANDS.md",
            0o644,
        ),
        ROOT / "docs" / "EVALUATOR.md": (
            staging / "share" / "doc" / "kapsel" / "EVALUATOR.md",
            0o644,
        ),
        ROOT / "docs" / "MCP.md": (
            staging / "share" / "doc" / "kapsel" / "MCP.md",
            0o644,
        ),
        ROOT / "docs" / "PRIVACY.md": (
            staging / "share" / "doc" / "kapsel" / "PRIVACY.md",
            0o644,
        ),
        ROOT / "docs" / "RELEASE.md": (
            staging / "share" / "doc" / "kapsel" / "RELEASE.md",
            0o644,
        ),
        ROOT / "SECURITY.md": (
            staging / "share" / "doc" / "kapsel" / "SECURITY.md",
            0o644,
        ),
        ROOT / "docs" / "UPGRADE.md": (
            staging / "share" / "doc" / "kapsel" / "UPGRADE.md",
            0o644,
        ),
        ROOT / "CHANGELOG.md": (staging / "CHANGELOG.md", 0o644),
        ROOT / "LICENSE": (staging / "LICENSE", 0o644),
    }
    for source, (destination, mode) in assets.items():
        if not source.is_file():
            raise RuntimeError(f"required release input is missing: {source.relative_to(ROOT)}")
        if source.suffix == ".md":
            copy_document(source, destination, revision)
        else:
            copy_file(source, destination, mode)

    ordinary = staging / "bin" / "kapsel"
    demonstration = staging / "libexec" / "kapsel-demo-harness"
    cargo_packages, cargo_relationships, root_package_id = cargo_graph(cargo_metadata)
    graph = {
        "packages": cargo_packages,
        "relationships": cargo_relationships,
        "root_package_id": root_package_id,
    }
    graph_sha256 = hashlib.sha256(
        json.dumps(graph, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    metadata = {
        "artifact_schema": "kapsel.release-artifact.v2",
        "package_version": package_version(),
        "rust_target": TARGET,
        "source_revision": revision,
        "source_tree": tree,
        "source_dirty": dirty,
        "cargo_lock_sha256": file_sha256(ROOT / "Cargo.lock"),
        "cargo_graph_sha256": graph_sha256,
        "cargo_package_count": len(cargo_packages),
        "cargo_relationship_count": len(cargo_relationships),
        "license": "Apache-2.0",
        "license_sha256": file_sha256(staging / "LICENSE"),
        "builder_image": BUILDER_IMAGE,
        "smoke_image": SMOKE_IMAGE,
        "ordinary_binary_bytes": ordinary.stat().st_size,
        "ordinary_binary_sha256": file_sha256(ordinary),
        "demo_binary_bytes": demonstration.stat().st_size,
        "demo_binary_sha256": file_sha256(demonstration),
        "non_claims": NON_CLAIMS,
    }
    staging.joinpath("RELEASE-METADATA.json").write_text(
        json.dumps(metadata, indent=2, separators=(",", ": ")) + "\n"
    )
    staging.joinpath("RELEASE-METADATA.json").chmod(0o644)
    return cargo_metadata


def tar_info(path: pathlib.Path, arcname: str) -> tarfile.TarInfo:
    information = tarfile.TarInfo(arcname + ("/" if path.is_dir() else ""))
    information.uid = 0
    information.gid = 0
    information.uname = ""
    information.gname = ""
    information.mtime = 0
    if path.is_dir():
        information.type = tarfile.DIRTYPE
        information.mode = 0o755
    else:
        information.type = tarfile.REGTYPE
        information.mode = path.stat().st_mode & 0o777
        information.size = path.stat().st_size
    return information


def create_archive(staging: pathlib.Path, archive: pathlib.Path) -> None:
    paths = [staging, *staging.rglob("*")]
    paths.sort(key=lambda path: path.relative_to(staging.parent).as_posix())
    descriptor = os.open(
        archive,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o644,
    )
    with os.fdopen(descriptor, "wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as release:
                for path in paths:
                    arcname = path.relative_to(staging.parent).as_posix()
                    information = tar_info(path, arcname)
                    if path.is_dir():
                        release.addfile(information)
                    else:
                        with path.open("rb") as source:
                            release.addfile(information, source)


def spdx_id(prefix: str, value: str) -> str:
    return f"SPDXRef-{prefix}-{hashlib.sha256(value.encode()).hexdigest()[:16]}"


def cargo_graph(
    metadata: dict[str, object],
) -> tuple[list[dict[str, object]], list[dict[str, str]], str]:
    root_packages = [
        package
        for package in metadata["packages"]
        if package["name"] == "kapsel" and package["manifest_path"] == "/workspace/Cargo.toml"
    ]
    if len(root_packages) != 1:
        raise RuntimeError("Cargo metadata did not identify the root package")
    root_id = root_packages[0]["id"]
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    reachable = {root_id}
    pending = [root_id]
    edges: list[tuple[str, str]] = []
    while pending:
        source = pending.pop()
        for dependency in nodes[source]["deps"]:
            if not any(kind["kind"] != "dev" for kind in dependency["dep_kinds"]):
                continue
            target = dependency["pkg"]
            edges.append((source, target))
            if target not in reachable:
                reachable.add(target)
                pending.append(target)

    lock = tomllib.loads(ROOT.joinpath("Cargo.lock").read_text())
    lock_checksums = {
        (package["name"], package["version"], package.get("source")): package.get("checksum")
        for package in lock["package"]
    }
    identifiers = {
        package["id"]: (
            "SPDXRef-Package-kapsel-source"
            if package["id"] == root_id
            else spdx_id("Package", package["id"])
        )
        for package in metadata["packages"]
        if package["id"] in reachable
    }
    packages: list[dict[str, object]] = []
    for package in sorted(
        (package for package in metadata["packages"] if package["id"] in reachable),
        key=lambda value: (value["name"], value["version"], value["id"]),
    ):
        entry: dict[str, object] = {
            "SPDXID": identifiers[package["id"]],
            "name": package["name"],
            "versionInfo": package["version"],
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": package["license"] or "NOASSERTION",
            "copyrightText": "NOASSERTION",
        }
        checksum = lock_checksums.get((package["name"], package["version"], package["source"]))
        if checksum is not None:
            entry["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
        if package["source"] and package["source"].startswith("registry+"):
            entry["externalRefs"] = [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}",
                }
            ]
        packages.append(entry)
    relationships = [
        {
            "spdxElementId": identifiers[source],
            "relationshipType": "DEPENDS_ON",
            "relatedSpdxElement": identifiers[target],
            "comment": "Cargo locked graph; target-conditioned and build dependencies are conservative identity evidence.",
        }
        for source, target in sorted(
            set(edges), key=lambda edge: (identifiers[edge[0]], identifiers[edge[1]])
        )
    ]
    return packages, relationships, identifiers[root_id]


def create_sbom(
    archive: pathlib.Path,
    revision: str,
    tree: str,
    source_date: str,
    cargo_metadata: dict[str, object],
) -> pathlib.Path:
    basename = archive.name.removesuffix(".tar.gz")
    with tarfile.open(archive, "r:gz") as release:
        metadata_file = release.extractfile(f"{basename}/RELEASE-METADATA.json")
        if metadata_file is None:
            raise RuntimeError("release metadata is missing while creating the SBOM")
        metadata = json.load(metadata_file)
    cargo_packages, cargo_relationships, root_package_id = cargo_graph(cargo_metadata)
    archive_id = "SPDXRef-Package-kapsel-archive"
    ordinary_file_id = "SPDXRef-File-bin-kapsel"
    demo_file_id = "SPDXRef-File-libexec-kapsel-demo-harness"
    archive_digest = file_sha256(archive)
    packages: list[dict[str, object]] = [
        {
            "SPDXID": archive_id,
            "name": archive.name,
            "versionInfo": metadata["package_version"],
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "packageFileName": archive.name,
            "checksums": [{"algorithm": "SHA256", "checksumValue": archive_digest}],
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "Apache-2.0",
            "copyrightText": "NOASSERTION",
        },
        *cargo_packages,
    ]
    relationships: list[dict[str, str]] = [
        {
            "spdxElementId": "SPDXRef-DOCUMENT",
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": archive_id,
        },
        {
            "spdxElementId": archive_id,
            "relationshipType": "GENERATED_FROM",
            "relatedSpdxElement": root_package_id,
        },
        {
            "spdxElementId": archive_id,
            "relationshipType": "CONTAINS",
            "relatedSpdxElement": ordinary_file_id,
        },
        {
            "spdxElementId": archive_id,
            "relationshipType": "CONTAINS",
            "relatedSpdxElement": demo_file_id,
        },
        *cargo_relationships,
    ]
    sbom = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"{archive.name} software bill of materials",
        "documentNamespace": (
            f"https://github.com/kapsel-cloud/kapsel/sbom/{revision}/{archive_digest}"
        ),
        "comment": (
            f"source_revision={revision};source_tree={tree};rust_target={TARGET};"
            f"builder_image={BUILDER_IMAGE};cargo_lock_sha256={file_sha256(ROOT / 'Cargo.lock')};"
            f"cargo_graph_sha256={metadata['cargo_graph_sha256']};"
            "created is normalized to the source commit time for reproducible serialization"
        ),
        "creationInfo": {
            "created": source_date,
            "creators": [f"Tool: {SBOM_GENERATOR}"],
        },
        "packages": packages,
        "files": [
            {
                "SPDXID": ordinary_file_id,
                "fileName": "./bin/kapsel",
                "checksums": [
                    {"algorithm": "SHA256", "checksumValue": metadata["ordinary_binary_sha256"]}
                ],
                "licenseConcluded": "NOASSERTION",
                "copyrightText": "NOASSERTION",
            },
            {
                "SPDXID": demo_file_id,
                "fileName": "./libexec/kapsel-demo-harness",
                "checksums": [
                    {"algorithm": "SHA256", "checksumValue": metadata["demo_binary_sha256"]}
                ],
                "licenseConcluded": "NOASSERTION",
                "copyrightText": "NOASSERTION",
            },
        ],
        "relationships": relationships,
    }
    sbom_path = archive.with_name(archive.name + ".spdx.json")
    encoded = (json.dumps(sbom, indent=2, separators=(",", ": ")) + "\n").encode()
    write_exclusive(sbom_path, encoded)
    if sbom_path.stat().st_size > SBOM_BYTES_MAX:
        sbom_path.unlink()
        raise RuntimeError("release SBOM exceeded its byte bound")
    return sbom_path


def create_digest_manifest(
    archive: pathlib.Path, checksum: pathlib.Path, sbom: pathlib.Path
) -> pathlib.Path:
    manifest = archive.with_name(archive.name + ".SHA256SUMS")
    entries = sorted([archive, checksum, sbom], key=lambda path: path.name)
    value = "".join(f"{file_sha256(path)}  {path.name}\n" for path in entries).encode()
    write_exclusive(manifest, value)
    if manifest.stat().st_size > MANIFEST_BYTES_MAX:
        manifest.unlink()
        raise RuntimeError("release digest manifest exceeded its byte bound")
    return manifest


def assemble(output_directory: pathlib.Path, allow_dirty: bool) -> pathlib.Path:
    revision, tree, source_date, dirty = git_provenance(allow_dirty)
    if shutil.which("docker") is None:
        raise RuntimeError("Docker is required for release assembly")
    run("docker", "info")
    version = package_version()
    basename = f"kapsel-{version}-{TARGET}"
    output_directory.mkdir(parents=True, exist_ok=True)
    archive = output_directory / f"{basename}.tar.gz"
    checksum = output_directory / f"{archive.name}.sha256"
    sbom = output_directory / f"{archive.name}.spdx.json"
    manifest = output_directory / f"{archive.name}.SHA256SUMS"
    if any(os.path.lexists(path) for path in (archive, checksum, sbom, manifest)):
        raise RuntimeError("release output already exists")

    with tempfile.TemporaryDirectory(prefix="kapsel-release-stage-") as temporary:
        staging = pathlib.Path(temporary) / basename
        staging.mkdir(mode=0o755)
        cargo_metadata = stage_release(staging, revision, tree, dirty)
        expanded_size = sum(path.stat().st_size for path in staging.rglob("*") if path.is_file())
        if expanded_size > EXPANDED_BYTES_MAX:
            raise RuntimeError("release staging tree exceeded its expanded bound")
        create_archive(staging, archive)

    if archive.stat().st_size > ARCHIVE_BYTES_MAX:
        archive.unlink()
        raise RuntimeError("release archive exceeded its compressed bound")
    checksum_value = f"{file_sha256(archive)}  {archive.name}\n".encode()
    write_exclusive(checksum, checksum_value)
    created_sbom = create_sbom(archive, revision, tree, source_date, cargo_metadata)
    create_digest_manifest(archive, checksum, created_sbom)
    return archive


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-directory", required=True, type=pathlib.Path)
    parser.add_argument("--allow-dirty", action="store_true")
    arguments = parser.parse_args()
    try:
        archive = assemble(arguments.output_directory.resolve(), arguments.allow_dirty)
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Kapsel release assembly failed: {error}", file=sys.stderr)
        return 1
    print(archive)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
