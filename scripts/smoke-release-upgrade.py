#!/usr/bin/env python3
"""Exercise an extracted v0.2 binary across the exact v0.1.1 upgrade pair."""

from __future__ import annotations

import argparse
import hashlib
import http.server
import importlib.util
import io
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import threading

OLD_VERSION = "0.1.1"
OLD_ARCHIVE_SHA256 = "d1a5bfec47012e126a9c8b351fbe2331aa52e12c2df0fd41dd2a91c03b7c7fb4"
OLD_BASENAME = "kapsel-0.1.1-x86_64-unknown-linux-gnu"
ARCHIVE_BYTES_MAX = 32 * 1024 * 1024
EXPANDED_BYTES_MAX = 64 * 1024 * 1024
FILE_BYTES_MAX = 32 * 1024 * 1024

SMOKE_PATH = pathlib.Path(__file__).with_name("smoke-release-artifact.py")
SMOKE_SPEC = importlib.util.spec_from_file_location("smoke_release_artifact", SMOKE_PATH)
if SMOKE_SPEC is None or SMOKE_SPEC.loader is None:
    raise RuntimeError("could not load the release artifact verifier")
SMOKE = importlib.util.module_from_spec(SMOKE_SPEC)
SMOKE_SPEC.loader.exec_module(SMOKE)


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def read_bounded_regular(path: pathlib.Path, maximum: int) -> bytes:
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise RuntimeError("upgrade artifact is not a bounded regular file")
        value = source.read(maximum + 1)
    if len(value) > maximum:
        raise RuntimeError("upgrade artifact exceeded its byte bound")
    return value


def extract_old_archive(archive: pathlib.Path, destination: pathlib.Path) -> pathlib.Path:
    archive_bytes = read_bounded_regular(archive, ARCHIVE_BYTES_MAX)
    if (
        archive.name != f"{OLD_BASENAME}.tar.gz"
        or sha256_bytes(archive_bytes) != OLD_ARCHIVE_SHA256
    ):
        raise RuntimeError("v0.1.1 archive identity disagrees with the accepted release")
    expected = {
        f"{OLD_BASENAME}/",
        f"{OLD_BASENAME}/bin/",
        f"{OLD_BASENAME}/bin/kapsel",
        f"{OLD_BASENAME}/libexec/",
        f"{OLD_BASENAME}/libexec/kapsel-demo-harness",
        f"{OLD_BASENAME}/share/",
        f"{OLD_BASENAME}/share/kapsel/",
        f"{OLD_BASENAME}/share/kapsel/demo-kind-crash-recovery.sh",
        f"{OLD_BASENAME}/share/kapsel/kap0038-trust.hex",
        f"{OLD_BASENAME}/share/doc/",
        f"{OLD_BASENAME}/share/doc/kapsel/",
        f"{OLD_BASENAME}/share/doc/kapsel/EVALUATOR.md",
        f"{OLD_BASENAME}/CHANGELOG.md",
        f"{OLD_BASENAME}/LICENSE",
        f"{OLD_BASENAME}/RELEASE-METADATA.json",
    }
    with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:gz") as release:
        members = release.getmembers()
        names = {member.name + ("/" if member.isdir() else "") for member in members}
        if names != expected or [member.name for member in members] != sorted(
            member.name for member in members
        ):
            raise RuntimeError("v0.1.1 archive layout or ordering changed")
        if sum(member.size for member in members if member.isfile()) > EXPANDED_BYTES_MAX:
            raise RuntimeError("v0.1.1 archive exceeds its expanded bound")
        for member in members:
            path = pathlib.PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts:
                raise RuntimeError("v0.1.1 archive path is unsafe")
            if not (member.isdir() or member.isfile()) or member.size > FILE_BYTES_MAX:
                raise RuntimeError("v0.1.1 archive entry type or size is unsafe")
            expected_mode = (
                0o755
                if member.isdir()
                or member.name.endswith(("/kapsel", "/kapsel-demo-harness", ".sh"))
                else 0o644
            )
            identity = (member.uid, member.gid, member.uname, member.gname, member.mtime)
            if member.mode != expected_mode or identity != (0, 0, "", "", 0):
                raise RuntimeError("v0.1.1 archive metadata changed")
        for member in members:
            target = destination.joinpath(*pathlib.PurePosixPath(member.name).parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                source = release.extractfile(member)
                if source is None:
                    raise RuntimeError("v0.1.1 archive entry could not be read")
                with target.open("xb") as output:
                    shutil.copyfileobj(source, output)
            target.chmod(member.mode)
    return destination / OLD_BASENAME


def mcp_open(binary: pathlib.Path, operator: pathlib.Path) -> None:
    result = SMOKE.run_binary(binary, ["mcp", "--operator-config", str(operator)])
    if result.returncode != 0 or result.stdout or result.stderr:
        raise RuntimeError("migration-only MCP open failed")


def operate(binary: pathlib.Path, paths: dict[str, pathlib.Path]) -> bytes:
    result = SMOKE.run_binary(
        binary,
        [
            "operate",
            "--request",
            str(paths["request"]),
            "--operator-config",
            str(paths["operator"]),
        ],
    )
    if result.returncode != 0:
        raise RuntimeError("upgrade-pair operation failed")
    return result.stdout


def require_private_directory(path: pathlib.Path) -> None:
    metadata = path.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_IMODE(metadata.st_mode) != 0o700
        or metadata.st_uid != os.getuid()
    ):
        raise RuntimeError("upgrade directory is not owner-private")


def require_private_file(path: pathlib.Path, maximum: int) -> None:
    metadata = path.lstat()
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_uid != os.getuid()
        or metadata.st_nlink != 1
        or metadata.st_size > maximum
    ):
        raise RuntimeError("upgrade file identity is not owner-private and bounded")


def write_private_exclusive(path: pathlib.Path, value: bytes) -> None:
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    with os.fdopen(descriptor, "wb") as output:
        output.write(value)
        output.flush()
        os.fsync(output.fileno())


def fsync_directory(path: pathlib.Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def require_no_sidecars(journal: pathlib.Path) -> None:
    for suffix in ("-journal", "-wal", "-shm"):
        sidecar = journal.with_name(journal.name + suffix)
        if sidecar.exists() or sidecar.is_symlink():
            raise RuntimeError("SQLite sidecar exists during stopped upgrade procedure")


def private_backup(journal: pathlib.Path) -> tuple[pathlib.Path, pathlib.Path]:
    parent = journal.parent
    backup = journal.with_name(journal.name + ".kapsel-v011.backup")
    checksum = backup.with_name(backup.name + ".sha256")
    require_private_directory(parent)
    require_private_file(journal, 64 * 1024 * 1024)
    require_no_sidecars(journal)
    if backup.exists() or backup.is_symlink() or checksum.exists() or checksum.is_symlink():
        raise RuntimeError("upgrade backup destination already exists")
    journal_bytes = read_bounded_regular(journal, 64 * 1024 * 1024)
    write_private_exclusive(backup, journal_bytes)
    require_private_file(backup, 64 * 1024 * 1024)
    digest = sha256_bytes(journal_bytes)
    if sha256_bytes(read_bounded_regular(backup, 64 * 1024 * 1024)) != digest:
        raise RuntimeError("v0.1.1 backup identity mismatch")
    write_private_exclusive(checksum, (digest + "\n").encode())
    require_private_file(checksum, 65)
    fsync_directory(parent)
    return backup, checksum


def restore_backup(
    journal: pathlib.Path,
    backup: pathlib.Path,
    checksum: pathlib.Path,
) -> pathlib.Path:
    parent = journal.parent
    require_private_directory(parent)
    require_private_file(journal, 64 * 1024 * 1024)
    require_private_file(backup, 64 * 1024 * 1024)
    require_private_file(checksum, 65)
    require_no_sidecars(journal)
    recorded = read_bounded_regular(checksum, 65)
    if len(recorded) != 65 or not recorded.endswith(b"\n"):
        raise RuntimeError("v0.1.1 backup checksum format is invalid")
    expected = recorded[:-1].decode("ascii")
    if any(character not in "0123456789abcdef" for character in expected):
        raise RuntimeError("v0.1.1 backup checksum syntax is invalid")
    backup_bytes = read_bounded_regular(backup, 64 * 1024 * 1024)
    if sha256_bytes(backup_bytes) != expected:
        raise RuntimeError("v0.1.1 backup checksum mismatch")
    replacement = journal.with_name(f"{journal.name}.restore.{os.getpid()}")
    if replacement.exists() or replacement.is_symlink():
        raise RuntimeError("upgrade restore destination already exists")
    quarantine = pathlib.Path(tempfile.mkdtemp(prefix=f"{journal.name}.quarantine.", dir=parent))
    quarantine.chmod(0o700)
    quarantined = quarantine / journal.name
    quarantined_checksum = quarantine / f"{journal.name}.sha256"
    published = False
    try:
        write_private_exclusive(replacement, backup_bytes)
        require_private_file(replacement, 64 * 1024 * 1024)
        if read_bounded_regular(replacement, 64 * 1024 * 1024) != backup_bytes:
            raise RuntimeError("prepared restore differs from verified backup")
        active_bytes = read_bounded_regular(journal, 64 * 1024 * 1024)
        active_digest = sha256_bytes(active_bytes)
        write_private_exclusive(quarantined, active_bytes)
        write_private_exclusive(quarantined_checksum, (active_digest + "\n").encode())
        require_private_directory(quarantine)
        require_private_file(quarantined, 64 * 1024 * 1024)
        require_private_file(quarantined_checksum, 65)
        fsync_directory(quarantine)
        require_private_file(journal, 64 * 1024 * 1024)
        require_no_sidecars(journal)
        if sha256_bytes(read_bounded_regular(replacement, 64 * 1024 * 1024)) != expected:
            raise RuntimeError("prepared restore changed before publication")
        os.replace(replacement, journal)
        published = True
        require_private_file(journal, 64 * 1024 * 1024)
        if sha256_bytes(read_bounded_regular(journal, 64 * 1024 * 1024)) != expected:
            raise RuntimeError("published restore identity changed")
        descriptor = os.open(journal, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        fsync_directory(parent)
        fsync_directory(quarantine)
        return quarantine
    except BaseException:
        if replacement.exists() and not replacement.is_symlink():
            replacement.unlink()
        if not published and quarantine.exists() and not quarantine.is_symlink():
            shutil.rmtree(quarantine)
        raise


def smoke_upgrade(candidate_archive: pathlib.Path, old_archive: pathlib.Path) -> None:
    checksum = candidate_archive.with_name(candidate_archive.name + ".sha256")
    candidate_bytes, checksum_bytes = SMOKE.verify_checksum(candidate_archive, checksum)
    sbom = candidate_archive.with_name(candidate_archive.name + ".spdx.json")
    manifest = candidate_archive.with_name(candidate_archive.name + ".SHA256SUMS")
    sbom_bytes = SMOKE.verify_digest_manifest(
        candidate_archive,
        checksum,
        sbom,
        manifest,
        candidate_bytes,
        checksum_bytes,
    )
    metadata = SMOKE.validate_archive(candidate_archive, candidate_bytes)
    SMOKE.validate_sbom(candidate_archive, candidate_bytes, sbom_bytes, metadata)

    with tempfile.TemporaryDirectory(prefix="kapsel-release-upgrade-") as temporary:
        root = pathlib.Path(temporary)
        candidate_root = SMOKE.extract_exact_archive(
            candidate_archive, candidate_bytes, root / "new"
        )
        old_root = extract_old_archive(old_archive, root / "old")
        candidate_binary = candidate_root / "bin" / "kapsel"
        old_binary = old_root / "bin" / "kapsel"
        SMOKE.exercise_version(candidate_binary, metadata["package_version"])

        SMOKE.reset_kubernetes_fixture()
        fixture = http.server.ThreadingHTTPServer(("127.0.0.1", 0), SMOKE.KubernetesFixture)
        thread = threading.Thread(target=fixture.serve_forever, daemon=True)
        thread.start()
        evaluation = root / "evaluation"
        evaluation.mkdir(mode=0o700)
        try:
            paths = SMOKE.prepare_inputs(evaluation, fixture.server_address)
            SMOKE.provision_and_write_operator(old_binary, evaluation, paths)
            receipt = SMOKE.execute_and_restart(old_binary, paths)
            old_report = operate(old_binary, paths)
            frozen_receipt = receipt.read_bytes()
            journal = evaluation / "journal.sqlite3"
            backup, backup_checksum = private_backup(journal)

            mcp_open(candidate_binary, paths["operator"])
            mcp_open(candidate_binary, paths["operator"])
            if operate(candidate_binary, paths) != old_report:
                raise RuntimeError("v0.2 upgrade changed the finalized report")
            if receipt.read_bytes() != frozen_receipt or SMOKE.KubernetesFixture.requests != 3:
                raise RuntimeError("v0.2 upgrade changed frozen receipt or provider activity")
            trust = evaluation / "receipt.trust"
            trust_hex = (
                candidate_root.joinpath("share", "kapsel", "kap0038-trust.hex").read_text().strip()
            )
            SMOKE.write_private(trust, bytes.fromhex(trust_hex))
            SMOKE.inspect_receipt(candidate_binary, receipt, trust)

            mcp_open(old_binary, paths["operator"])
            if operate(old_binary, paths) != old_report:
                raise RuntimeError("exact v0.1.1 direct downgrade changed the report")

            candidate_generation = read_bounded_regular(journal, 64 * 1024 * 1024)
            quarantine = restore_backup(journal, backup, backup_checksum)
            quarantined = quarantine / journal.name
            quarantined_checksum = quarantine / f"{journal.name}.sha256"
            if (
                read_bounded_regular(quarantined, 64 * 1024 * 1024) != candidate_generation
                or read_bounded_regular(quarantined_checksum, 65)
                != (sha256_bytes(candidate_generation) + "\n").encode()
            ):
                raise RuntimeError("rollback quarantine did not preserve the active generation")
            mcp_open(candidate_binary, paths["operator"])
            mcp_open(candidate_binary, paths["operator"])
            if (
                operate(candidate_binary, paths) != old_report
                or receipt.read_bytes() != frozen_receipt
            ):
                raise RuntimeError("restored v0.1.1 generation changed candidate behavior")
            shutil.rmtree(quarantine)
            backup.unlink()
            backup_checksum.unlink()
            fsync_directory(journal.parent)
        finally:
            fixture.shutdown()
            fixture.server_close()
            thread.join(timeout=5)
        shutil.rmtree(evaluation)
        if evaluation.exists():
            raise RuntimeError("artifact upgrade smoke did not clean its evaluation directory")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate-archive", required=True, type=pathlib.Path)
    parser.add_argument("--v011-archive", required=True, type=pathlib.Path)
    arguments = parser.parse_args()
    try:
        candidate = pathlib.Path(os.path.abspath(arguments.candidate_archive))
        old = pathlib.Path(os.path.abspath(arguments.v011_archive))
        smoke_upgrade(candidate, old)
    except (OSError, RuntimeError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(f"Kapsel release upgrade smoke failed: {error}", file=sys.stderr)
        return 1
    print("Kapsel release artifact v0.1.1 upgrade and rollback: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
