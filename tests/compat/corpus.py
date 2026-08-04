#!/usr/bin/env python3
"""Import, build, and verify the retained C compatibility corpus."""

from __future__ import annotations

import argparse
import concurrent.futures
from contextlib import contextmanager
import csv
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from typing import Callable, Iterator

ROOT = Path(__file__).resolve().parent
ORACLE = ROOT / "oracle"
LOCAL = ROOT / "local" / "tests" / "compat"
INVENTORY = ROOT / "build-plan.tsv"
ARTIFACTS = ROOT / "artifacts" / "full"
REPORT = ROOT / "build-report.tsv"
BUILT = ROOT / "artifacts" / "manifest.tsv"
RUNTIME = ROOT / "artifacts" / "runtime"
REQUIRED = {"case", "source", "isas", "cflags", "exit", "stdout", "disposition"}
BUILDABLE = {"active", "excluded-macos", "excluded-windows"}
COMPILERS = {
    "aarch64": "aarch64-linux-gnu-gcc",
    "x86_64": "x86_64-linux-gnu-gcc",
}
PIN_FIELDS = [
    "suite", "case", "isa", "artifact", "sha256", "size", "toolchain",
    "source", "source_sha256", "cflags", "recipe_sha256", "exit", "stdout",
]


def cmake_artifacts(build: Path) -> dict[tuple[str, str], list[Path]]:
    """Index authoritative CMake guest outputs by retained source and ISA."""
    graph = build / "build.ninja"
    if not graph.is_file():
        raise ValueError(f"CMake Ninja graph is absent: {graph}")
    indexed: dict[tuple[str, str], list[Path]] = {}
    marker = ": CUSTOM_COMMAND "
    for line in graph.read_text().splitlines():
        if not line.startswith("build ") or marker not in line:
            continue
        output, dependencies = line.removeprefix("build ").split(marker, 1)
        output = output.split(" | ", 1)[0].replace("$ ", " ")
        isa = next((value for value in COMPILERS if f"/{value}/" in f"/{output}"), None)
        if isa is None:
            continue
        for token in dependencies.split():
            match = re.search(r"/(tests/(?:compat|soak)/\S+)$", token)
            if match is not None:
                indexed.setdefault((match.group(1), isa), []).append(build / output)
    return indexed


def cmake_sources(build: Path) -> dict[str, set[Path]]:
    """Recover the retained source paths named by the generated Ninja graph."""
    result: dict[str, set[Path]] = {}
    for line in (build / "build.ninja").read_text().splitlines():
        if not line.startswith("build ") or ": CUSTOM_COMMAND " not in line:
            continue
        dependencies = line.split(": CUSTOM_COMMAND ", 1)[1]
        for token in dependencies.split():
            match = re.search(r"/(tests/(?:compat|soak)/\S+)$", token)
            if match is not None:
                result.setdefault(match.group(1), set()).add(Path(token))
    return result


def cmake_commands(build: Path) -> dict[str, str]:
    """Map generated output paths to their exact CMake/Ninja command contract."""
    result: dict[str, str] = {}
    output: str | None = None
    for line in (build / "build.ninja").read_text().splitlines():
        if line.startswith("build ") and ": CUSTOM_COMMAND " in line:
            output = line.removeprefix("build ").split(": CUSTOM_COMMAND ", 1)[0]
            output = output.split(" | ", 1)[0].replace("$ ", " ")
        elif output is not None and line.startswith("  COMMAND = "):
            result[output] = line.removeprefix("  COMMAND = ")
            output = None
        elif line and not line.startswith(" "):
            output = None
    return result


def parity_rows(build: Path, pins: list[dict[str, str]]) -> list[dict[str, str]]:
    """Compare persistent fixtures with the exact CMake-produced binaries."""
    indexed = cmake_artifacts(build)
    comparisons = []
    for pin in pins:
        candidates = [path for path in indexed.get((pin["source"], pin["isa"]), [])
                      if path.is_file()]
        suite_prefix = "soak" if pin["suite"] == "soak" else (
            "compat/abi-corpus" if pin["suite"] == "abi/corpus" else
            "compat/isa" if pin["suite"].startswith("isa/") else
            f"compat/{pin['suite']}"
        )
        scoped = [path for path in candidates
                  if path.relative_to(build).as_posix().startswith(suite_prefix + "/")]
        if scoped:
            candidates = scoped
        artifact = ROOT / pin["artifact"]
        if not candidates:
            state, c_path = "missing-c", "-"
        elif len(candidates) != 1:
            state, c_path = "ambiguous-c", ",".join(str(path) for path in candidates)
        elif not artifact.is_file():
            state, c_path = "missing-rust", str(candidates[0])
        else:
            c_path = str(candidates[0])
            state = "identical" if digest(candidates[0]) == digest(artifact) else "different"
        comparisons.append({
            "suite": pin["suite"], "case": pin["case"], "isa": pin["isa"],
            "state": state, "c_artifact": c_path, "rust_artifact": str(artifact),
        })
    return comparisons


def import_rows(build: Path, pins: list[dict[str, str]]) -> list[dict[str, str]]:
    """Classify every pin before any artifact is staged or published."""
    comparisons = parity_rows(build, pins)
    sources = cmake_sources(build)
    commands = cmake_commands(build)
    for pin, row in zip(pins, comparisons, strict=True):
        if row["state"] in {"missing-c", "ambiguous-c"}:
            row["import_state"] = row["state"]
            continue
        retained = ORACLE / pin["source"]
        origins = sources.get(pin["source"], set())
        if not retained.is_file() or len(origins) != 1 or not next(iter(origins)).is_file():
            row["import_state"] = "source-missing"
            continue
        if retained.suffix != ".c":
            row["import_state"] = "source-prebuilt"
            continue
        origin = next(iter(origins))
        retained_digest = digest(retained)
        origin_digest = digest(origin)
        if (
            retained_digest != pin["source_sha256"]
            or origin_digest != retained_digest
        ):
            row["import_state"] = "source-different"
            continue
        relative = Path(row["c_artifact"]).relative_to(build).as_posix()
        command = commands.get(relative)
        if command is None:
            row["import_state"] = "command-missing"
            continue
        row["import_state"] = "importable"
        row["command"] = command
        row["retained_source"] = str(retained)
        row["c_source"] = str(origin)
        row["source_digest"] = retained_digest
    return comparisons


def selected_pins(
    pins: list[dict[str, str]], cases: set[str] | None,
    suites: set[str] | None, isas: set[str] | None,
) -> list[dict[str, str]]:
    return [
        pin for pin in pins
        if (cases is None or pin["case"] in cases)
        and (suites is None or pin["suite"] in suites)
        and (isas is None or pin["isa"] in isas)
    ]


def staged_copy(source: Path, target: Path) -> Path:
    target.parent.mkdir(parents=True, exist_ok=True)
    temporary = tempfile.NamedTemporaryFile(
        prefix=f".{target.name}.", suffix=".cmake-import-stage",
        dir=target.parent, delete=False,
    )
    staged = Path(temporary.name)
    temporary.close()
    try:
        shutil.copy2(source, staged)
        with staged.open("rb") as stream:
            os.fsync(stream.fileno())
        return staged
    except BaseException:
        staged.unlink(missing_ok=True)
        raise


def staged_table(path: Path, records: list[dict[str, str]]) -> Path:
    temporary = tempfile.NamedTemporaryFile(
        mode="w", newline="", prefix=f".{path.name}.",
        suffix=".cmake-import-stage", dir=path.parent, delete=False,
    )
    staged = Path(temporary.name)
    try:
        with temporary as target:
            writer = csv.DictWriter(target, PIN_FIELDS, delimiter="\t", lineterminator="\n")
            writer.writeheader()
            writer.writerows(
                {name: record.get(name, "") for name in PIN_FIELDS}
                for record in records
            )
            target.flush()
            os.fsync(target.fileno())
        return staged
    except BaseException:
        staged.unlink(missing_ok=True)
        raise


def sync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def journal_path(manifest: Path) -> Path:
    return manifest.parent / ".cmake-import.json"


def write_journal(path: Path, record: dict[str, object]) -> None:
    temporary = tempfile.NamedTemporaryFile(
        mode="w", prefix=f".{path.name}.", suffix=".tmp",
        dir=path.parent, delete=False,
    )
    staged = Path(temporary.name)
    try:
        with temporary as target:
            json.dump(record, target, sort_keys=True, separators=(",", ":"))
            target.write("\n")
            target.flush()
            os.fsync(target.fileno())
        os.replace(staged, path)
        sync_directory(path.parent)
    finally:
        staged.unlink(missing_ok=True)


def transaction_paths(record: dict[str, object]) -> Iterator[tuple[Path, Path | None, Path]]:
    for entry in record["artifacts"]:
        assert isinstance(entry, dict)
        backup = entry["backup"]
        yield (
            Path(str(entry["target"])),
            Path(str(backup)) if backup is not None else None,
            Path(str(entry["staged"])),
        )


def validate_journal(
    record: dict[str, object], manifest: Path,
) -> tuple[Path, Path, list[tuple[Path, Path | None, Path]]]:
    if record.get("version") != 1 or Path(str(record.get("manifest"))) != manifest:
        raise SystemExit(f"invalid CMake import journal for {manifest}")
    root = Path(str(record.get("root"))).resolve()
    manifest_backup = Path(str(record["manifest_backup"]))
    manifest_stage = Path(str(record["manifest_stage"]))
    entries = list(transaction_paths(record))
    if (
        not manifest.resolve().is_relative_to(root)
        or manifest_backup.parent != manifest.parent
        or not manifest_backup.name.endswith(".cmake-import-backup")
        or manifest_stage.parent != manifest.parent
        or not manifest_stage.name.endswith(".cmake-import-stage")
    ):
        raise SystemExit(f"unsafe CMake import journal for {manifest}")
    targets = set()
    for target, backup, staged in entries:
        if (
            not target.resolve().is_relative_to(root)
            or target in targets
            or staged.parent != target.parent
            or not staged.name.endswith(".cmake-import-stage")
            or (
                backup is not None
                and (backup.parent != target.parent
                     or not backup.name.endswith(".cmake-import-backup"))
            )
        ):
            raise SystemExit(f"unsafe CMake import journal for {manifest}")
        targets.add(target)
    return manifest_backup, manifest_stage, entries


def recover_transaction(manifest: Path) -> None:
    """Finish cleanup or restore the old coherent corpus after a hard exit."""
    journal = journal_path(manifest)
    if not journal.is_file():
        return
    record = json.loads(journal.read_text())
    committed = record.get("state") == "committed"
    manifest_backup, manifest_stage, entries = validate_journal(record, manifest)
    if not committed:
        if manifest_backup.exists():
            os.replace(manifest_backup, manifest)
        for target, backup, _ in reversed(entries):
            if backup is None:
                target.unlink(missing_ok=True)
            elif backup.exists():
                os.replace(backup, target)
    manifest_stage.unlink(missing_ok=True)
    manifest_backup.unlink(missing_ok=True)
    for _, backup, staged in entries:
        staged.unlink(missing_ok=True)
        if backup is not None:
            backup.unlink(missing_ok=True)
    for directory in {target.parent for target, _, _ in entries}:
        sync_directory(directory)
    sync_directory(manifest.parent)
    journal.unlink()
    sync_directory(journal.parent)


def cleanup_orphans(artifact_root: Path, manifest: Path) -> None:
    """Remove importer-owned staging left before a journal was published."""
    removed = set()
    patterns = ("*.cmake-import-stage", "*.cmake-import-backup")
    for pattern in patterns:
        for path in artifact_root.rglob(pattern):
            removed.add(path.parent)
            path.unlink()
        for path in manifest.parent.glob(pattern):
            removed.add(path.parent)
            path.unlink()
    for directory in removed:
        sync_directory(directory)


@contextmanager
def import_lock(manifest: Path) -> Iterator[None]:
    lock_path = manifest.parent / ".cmake-import.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a+") as lock:
        try:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise SystemExit("another CMake artifact import is active") from error
        recover_transaction(manifest)
        yield


def replace_transaction(
    replacements: list[tuple[Path, Path]], manifest_stage: Path,
    manifest: Path | None = None, fault: Callable[[str], None] | None = None,
) -> None:
    """Publish artifacts and pins with durable old-or-new crash recovery."""
    manifest = BUILT if manifest is None else manifest
    notify = fault if fault is not None else lambda _: None
    journal = journal_path(manifest)
    if journal.exists():
        raise SystemExit(f"unrecovered CMake import journal: {journal}")
    transaction = f"{os.getpid()}-{os.urandom(8).hex()}"
    manifest_backup = manifest.with_name(
        f".{manifest.name}.{transaction}.cmake-import-backup"
    )
    entries = []
    for staged, target in replacements:
        backup = target.with_name(
            f".{target.name}.{transaction}.cmake-import-backup"
        ) if target.exists() else None
        entries.append({
            "target": str(target), "backup": str(backup) if backup else None,
            "staged": str(staged),
        })
    record: dict[str, object] = {
        "version": 1, "state": "planned", "manifest": str(manifest),
        "root": os.path.commonpath([
            str(manifest.parent), *(str(target) for _, target in replacements),
        ]),
        "manifest_stage": str(manifest_stage),
        "manifest_backup": str(manifest_backup), "artifacts": entries,
    }
    write_journal(journal, record)
    try:
        notify("planned")
        for target, backup, _ in transaction_paths(record):
            if backup is not None:
                os.link(target, backup)
        os.link(manifest, manifest_backup)
        for directory in {target.parent for target, _, _ in transaction_paths(record)}:
            sync_directory(directory)
        sync_directory(manifest.parent)
        record["state"] = "prepared"
        write_journal(journal, record)
        notify("prepared")
        for staged, target in replacements:
            os.replace(staged, target)
            notify("artifact")
        for directory in {target.parent for _, target in replacements}:
            sync_directory(directory)
        notify("artifacts")
        os.replace(manifest_stage, manifest)
        sync_directory(manifest.parent)
        notify("manifest")
        record["state"] = "committed"
        write_journal(journal, record)
        notify("committed")
    except BaseException:
        recover_transaction(manifest)
        raise
    recover_transaction(manifest)
    notify("cleanup")


def import_cmake(
    build: Path, cases: set[str] | None = None, suites: set[str] | None = None,
    isas: set[str] | None = None, import_unique: bool = False,
) -> None:
    """Replace selected persistent guests with uniquely mapped CMake outputs."""
    with import_lock(BUILT):
        cleanup_orphans(ARTIFACTS, BUILT)
        import_cmake_locked(build, cases, suites, isas, import_unique)


def import_cmake_locked(
    build: Path, cases: set[str] | None = None, suites: set[str] | None = None,
    isas: set[str] | None = None, import_unique: bool = False,
) -> None:
    build = build.resolve()
    with BUILT.open(newline="") as source:
        pins = list(csv.DictReader(source, delimiter="\t"))
    selected = selected_pins(pins, cases, suites, isas)
    if not selected:
        raise SystemExit("no persistent pins match the import selection")
    comparisons = import_rows(build, selected)
    refused = [row for row in comparisons if row["import_state"] != "importable"]
    if refused and not import_unique:
        details = "\n".join(
            f"  {row['suite']}/{row['case']}/{row['isa']}: {row['import_state']}"
            for row in refused
        )
        raise SystemExit(f"refusing {len(refused)} unresolved CMake import(s):\n{details}")
    comparisons = [row for row in comparisons if row not in refused]
    if not comparisons:
        raise SystemExit("no uniquely mapped CMake outputs match the import selection")
    by_key = {(pin["suite"], pin["case"], pin["isa"]): pin for pin in pins}
    replacements: list[tuple[Path, Path]] = []
    updated = []
    targets: set[Path] = set()
    prepared: list[tuple[dict[str, str], Path, Path, str]] = []
    for row in comparisons:
        key = (row["suite"], row["case"], row["isa"])
        pin = dict(by_key[key])
        source = Path(row["c_artifact"])
        target = (ROOT / pin["artifact"]).resolve()
        if not target.is_relative_to(ARTIFACTS.resolve()) or target in targets:
            raise SystemExit(f"artifact target collision or escape: {target}")
        targets.add(target)
        if (
            digest(Path(row["retained_source"])) != row["source_digest"]
            or digest(Path(row["c_source"])) != row["source_digest"]
        ):
            raise SystemExit(f"source changed during CMake import: {pin['source']}")
        prepared.append((pin, source, target, row["command"]))
    staged_bytes = sum(source.stat().st_size for _, source, _, _ in prepared)
    reserve = 64 * 1024 * 1024
    if shutil.disk_usage(ARTIFACTS).free < staged_bytes + reserve:
        raise SystemExit(
            f"insufficient disk for transactional staging: need={staged_bytes + reserve}"
        )
    try:
        for pin, source, target, command in prepared:
            key = (pin["suite"], pin["case"], pin["isa"])
            staged = staged_copy(source, target)
            replacements.append((staged, target))
            pin.update(
                sha256=digest(staged), size=str(staged.stat().st_size),
                toolchain=f"cmake-command-{text_digest(command)}",
            )
            by_key[key] = pin
        updated = sorted(by_key.values(), key=lambda pin: (
            pin["suite"], pin["case"], pin["isa"]
        ))
        manifest_stage = staged_table(BUILT, updated)
        replace_transaction(replacements, manifest_stage)
    except BaseException:
        for staged, _ in replacements:
            staged.unlink(missing_ok=True)
        raise
    imported = parity_rows(build, [by_key[
        (row["suite"], row["case"], row["isa"])
    ] for row in comparisons])
    drift = [row for row in imported if row["state"] != "identical"]
    if drift:
        raise SystemExit(f"post-import CMake verification failed for {len(drift)} artifact(s)")
    refusal_counts: dict[str, int] = {}
    for row in refused:
        state = row["import_state"]
        refusal_counts[state] = refusal_counts.get(state, 0) + 1
    refusal_summary = " ".join(
        f"{state}={refusal_counts[state]}" for state in sorted(refusal_counts)
    )
    print(
        f"import-cmake: imported={len(imported)} verified={len(imported)} "
        f"refused={len(refused)}{(' ' + refusal_summary) if refusal_summary else ''}"
    )


def audit_cmake(build: Path) -> None:
    with BUILT.open(newline="") as source:
        pins = list(csv.DictReader(source, delimiter="\t"))
    comparisons = import_rows(build.resolve(), pins)
    counts: dict[str, int] = {}
    for row in comparisons:
        counts[row["state"]] = counts.get(row["state"], 0) + 1
    print("audit-cmake: " + " ".join(
        f"{state}={counts[state]}" for state in sorted(counts)
    ))
    import_counts: dict[str, int] = {}
    for row in comparisons:
        state = row["import_state"]
        import_counts[state] = import_counts.get(state, 0) + 1
    print("import-preflight: " + " ".join(
        f"{state}={import_counts[state]}" for state in sorted(import_counts)
    ))
    drift = [
        row for row in comparisons
        if row["state"] != "identical" or row["import_state"] != "importable"
    ]
    if drift:
        examples = "\n".join(
            f"  {row['suite']}/{row['case']}/{row['isa']}: "
            f"{row['import_state'] if row['import_state'] != 'importable' else row['state']}"
            for row in drift[:20]
        )
        raise SystemExit(f"CMake artifact drift ({len(drift)}/{len(comparisons)}):\n{examples}")


def aliases() -> dict[tuple[str, str], str]:
    result = {}
    with (ROOT / "aliases.tsv").open(newline="") as source:
        for row in csv.reader(source, delimiter="\t"):
            if not row or row[0].startswith("#"):
                continue
            result[(row[0], row[1])] = row[3]
    return result


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def text_digest(value: str) -> str:
    return hashlib.sha256(value.encode()).hexdigest()


def oracle_source(value: str) -> Path:
    return ORACLE / value


def rows() -> list[dict[str, str]]:
    if not INVENTORY.exists():
        raise SystemExit("build-plan.tsv is absent; run corpus.py import first")
    with INVENTORY.open(newline="") as source:
        return list(csv.DictReader(source, delimiter="\t"))


def import_corpus(source: Path) -> None:
    retained = source.resolve() / "tests" / "compat"
    if not retained.is_dir():
        raise SystemExit(f"retained corpus is absent: {retained}")
    destination = ORACLE / "tests" / "compat"
    if destination.exists():
        shutil.rmtree(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(retained, destination, symlinks=True)
    retained_soak = source.resolve() / "tests" / "soak"
    destination_soak = ORACLE / "tests" / "soak"
    if destination_soak.exists():
        shutil.rmtree(destination_soak)
    if retained_soak.is_dir():
        shutil.copytree(retained_soak, destination_soak, symlinks=True)
    overlay_local(destination, LOCAL)
    (ORACLE / ".hl-external-corpus").write_text(
        "Imported read-only by corpus.py from the retained C behavioral oracle.\n"
    )
    inventory: list[dict[str, str]] = []
    schemas: list[tuple[str, str]] = []
    manifests = list(destination.rglob("manifest.tsv"))
    if (destination_soak / "manifest.tsv").is_file():
        manifests.append(destination_soak / "manifest.tsv")
    for manifest in sorted(manifests):
        suite = (manifest.parent.relative_to(destination).as_posix()
                 if manifest.is_relative_to(destination) else "soak")
        with manifest.open(newline="") as stream:
            lines = list(stream)
            header_index = next(
                (index for index, line in enumerate(lines) if line.startswith("# case\t")),
                None,
            )
            if header_index is None:
                legacy = legacy_rows(suite, lines)
                if legacy is None:
                    schemas.append((suite, "unsupported-manifest-schema"))
                    continue
                for record in legacy:
                    for isa in record["isas"].split(","):
                        disposition = record.get("disposition", "")
                        state = "build" if disposition in BUILDABLE else "skip"
                        reason = "-" if state == "build" else disposition or "unclassified"
                        inventory.append(entry(suite, record, isa, state, reason))
                continue
            header = lines[header_index].removeprefix("# ")
            data = [line for line in lines[header_index + 1:] if not line.startswith("#")]
            reader = csv.DictReader([header, *data], delimiter="\t")
            fields = set(reader.fieldnames or [])
            if not REQUIRED.issubset(fields):
                schemas.append((suite, "unsupported-manifest-schema"))
                continue
            for record in reader:
                if not record.get("case"):
                    continue
                isas = [value for value in record["isas"].split(",") if value]
                if not isas:
                    inventory.append(entry(suite, record, "-", "skip", "no-isa"))
                    continue
                for isa in isas:
                    disposition = record.get("disposition", "")
                    state = "build" if disposition in BUILDABLE else "skip"
                    reason = "-" if state == "build" else disposition or "unclassified"
                    inventory.append(entry(suite, record, isa, state, reason))
    INVENTORY.parent.mkdir(parents=True, exist_ok=True)
    fields = [
        "suite", "case", "source", "isa", "cflags", "exit", "stdout",
        "defines", "env", "dependencies", "disposition", "note", "state", "reason",
    ]
    write_table(INVENTORY, fields, inventory)
    with (ROOT / "schema-report.tsv").open("w", newline="") as target:
        writer = csv.writer(target, delimiter="\t", lineterminator="\n")
        writer.writerow(["suite", "reason"])
        writer.writerows(schemas)
    report_counts(inventory, "import")


def overlay_local(destination: Path, local: Path = LOCAL) -> None:
    if not local.is_dir():
        return
    for source in sorted(path for path in local.rglob("*") if path.is_file()):
        relative = source.relative_to(local)
        target = destination / relative
        if source.name == "manifest.tsv":
            merge_manifest(target, source)
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)


def merge_manifest(target: Path, overlay: Path) -> None:
    local_prefix, local_fields, local_order, local_rows = manifest_rows(overlay)
    if not target.is_file():
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(overlay, target)
        return
    prefix, fields, order, records = manifest_rows(target)
    if fields != local_fields:
        raise SystemExit(f"local manifest schema differs: {overlay}")
    for case in local_order:
        if case not in records:
            order.append(case)
        records[case] = local_rows[case]
    lines = [*prefix, *(records[case] for case in order)]
    target.write_text("\n".join(lines) + "\n")


def manifest_rows(path: Path) -> tuple[list[str], str, list[str], dict[str, str]]:
    lines = path.read_text().splitlines()
    header = next((index for index, line in enumerate(lines)
                   if line.startswith("# case\t")), None)
    if header is None:
        raise SystemExit(f"manifest header absent: {path}")
    fields = lines[header].removeprefix("# ")
    order = []
    records = {}
    for line in lines[header + 1:]:
        if not line or line.startswith("#"):
            continue
        case = line.split("\t", 1)[0]
        if case in records:
            raise SystemExit(f"duplicate manifest case: {path}:{case}")
        order.append(case)
        records[case] = line
    return lines[:header + 1], fields, order, records


def entry(
    suite: str, record: dict[str, str], isa: str, state: str, reason: str
) -> dict[str, str]:
    prefix = f"tests/compat/{suite}" if suite != "soak" else "tests/soak"
    return {
        "suite": suite,
        "case": record["case"],
        "source": f"{prefix}/{record['source']}",
        "isa": isa,
        "cflags": record["cflags"],
        "exit": record["exit"],
        "stdout": f"{prefix}/{record['stdout']}" if record["stdout"] != "-" else "-",
        "defines": record.get("defines", "-"),
        "env": record.get("env", "-"),
        "dependencies": record.get("dependencies", "-"),
        "disposition": record.get("disposition", ""),
        "note": record.get("note", ""),
        "state": state,
        "reason": reason,
    }


def legacy_rows(suite: str, lines: list[str]) -> list[dict[str, str]] | None:
    """Adapt ABI's headerless seven-column schema and current-schema tail."""
    records = []
    for values in csv.reader(
        (line for line in lines if line.strip() and not line.startswith("#")),
        delimiter="\t",
    ):
        if suite != "abi":
            return None
        if len(values) == 7:
            destination, origin, isas, exit_status, golden, fingerprint, note = values
            if not destination.endswith(".c") or isas != "aarch64,x86_64":
                return None
            records.append({
                "case": destination.removesuffix(".c"), "source": destination,
                "isas": isas, "cflags": "-static -O2 -lm", "exit": exit_status,
                "stdout": golden, "defines": "-", "env": "-",
                "dependencies": "linux-libc,abi", "disposition": "active",
                "note": f"legacy-origin={origin};{fingerprint};{note}",
            })
        elif len(values) == 13:
            (case, _, source, _, isas, cflags, defines, env, exit_status,
             stdout, dependencies, disposition, note) = values
            records.append({
                "case": case, "source": source, "isas": isas, "cflags": cflags,
                "exit": exit_status, "stdout": stdout, "defines": defines,
                "env": env, "dependencies": dependencies,
                "disposition": disposition, "note": note,
            })
        else:
            return None
    return records or None


def compiler(isa: str, recipe: str = "") -> list[str] | None:
    prefix = isa.upper()
    override = os.environ.get(f"HL_{prefix}_CC")
    flags = shlex.split(recipe)
    dynamic = "-static" not in flags and "-static-pie" not in flags
    configured = override
    if configured is None and dynamic:
        configured = os.environ.get(f"{prefix}_LINUX_CC")
    if configured is None:
        configured = (
            os.environ.get(f"{prefix}_LINUX_STATIC_CC")
            or os.environ.get(f"{prefix}_LINUX_CC")
        )
    if configured:
        return shlex.split(configured)
    executable = shutil.which(COMPILERS[isa])
    return [executable] if executable else None


def compiler_command(
    cc: str | list[str], recipe: str, source: Path, output: Path
) -> list[str]:
    """Place library inputs after the translation unit for static link resolution."""
    driver = shlex.split(cc) if isinstance(cc, str) else cc
    tokens = shlex.split(recipe)
    libraries = [token for token in tokens if token.startswith("-l") and len(token) > 2]
    driver_flags = [token for token in tokens if token not in libraries]
    return [*driver, *driver_flags, str(source), "-o", str(output), *libraries]


def build_one(record: dict[str, str]) -> dict[str, str]:
    result = dict(record)
    source = ORACLE / record["source"]
    if not source.is_file():
        alias = aliases().get((record["suite"], record["case"]))
        if alias is not None:
            source = ORACLE / alias
            result["source"] = alias
    output = ARTIFACTS / record["suite"] / record["isa"] / record["case"]
    if not source.is_file():
        result.update(state="skipped", reason="missing-source")
        return result
    if source.suffix != ".c":
        return copy_executable(result, source, output)
    cc = compiler(record["isa"], record["cflags"])
    if cc is None:
        result.update(state="skipped", reason="missing-compiler")
        return result
    driver = shlex.split(cc) if isinstance(cc, str) else cc
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = tempfile.NamedTemporaryFile(
        prefix=f".{output.name}.", suffix=".tmp", dir=output.parent, delete=False
    )
    temporary_path = Path(temporary.name)
    temporary.close()
    # The compiler must create the output so its executable mode is not
    # inherited from NamedTemporaryFile's intentionally private 0600 mode.
    temporary_path.unlink()
    command = compiler_command(driver, record["cflags"], source, temporary_path)
    effective = record["cflags"]
    try:
        completed = subprocess.run(command, cwd=ORACLE, capture_output=True, text=True)
        if completed.returncode != 0 and "undefined reference" in completed.stderr:
            completed = subprocess.run(
                [*command, "-lm"], cwd=ORACLE, capture_output=True, text=True
            )
            if completed.returncode == 0:
                effective += " -lm"
        if completed.returncode == 0:
            os.replace(temporary_path, output)
    finally:
        temporary_path.unlink(missing_ok=True)
    if completed.returncode != 0:
        reason = "compile:" + " ".join(completed.stderr.split())
        result.update(state="failed", reason=reason)
        return result
    version = subprocess.run(
        [*driver, "-dumpfullversion", "-dumpversion"], capture_output=True, text=True
    ).stdout.strip()
    result.update(
        state="built",
        reason="-",
        artifact=str(output.relative_to(ROOT)),
        sha256=digest(output),
        size=str(output.stat().st_size),
        toolchain=f"{Path(driver[0]).name}-{version}",
        source_sha256=digest(source),
        cflags=effective,
        recipe_sha256=text_digest(effective),
    )
    return result


def copy_executable(result: dict[str, str], source: Path, output: Path) -> dict[str, str]:
    """Atomically pin an already-built guest fixture without interpreting its recipe."""
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = tempfile.NamedTemporaryFile(
        prefix=f".{output.name}.", suffix=".tmp", dir=output.parent, delete=False
    )
    temporary_path = Path(temporary.name)
    temporary.close()
    try:
        shutil.copy2(source, temporary_path)
        os.replace(temporary_path, output)
    finally:
        temporary_path.unlink(missing_ok=True)
    recipe = result["cflags"]
    result.update(
        state="built",
        reason="-",
        artifact=str(output.relative_to(ROOT)),
        sha256=digest(output),
        size=str(output.stat().st_size),
        toolchain="prebuilt-copy",
        source_sha256=digest(source),
        cflags=recipe,
        recipe_sha256=text_digest(recipe),
    )
    return result


def select_rows(
    inventory: list[dict[str, str]], pins: list[dict[str, str]],
    cases: set[str] | None, suites: set[str] | None, missing_only: bool,
) -> list[dict[str, str]]:
    pinned = {(row["suite"], row["case"], row["isa"]) for row in pins}
    return [
        row for row in inventory
        if row["state"] == "build"
        and (cases is None or row["case"] in cases)
        and (suites is None or row["suite"] in suites)
        and (not missing_only or (row["suite"], row["case"], row["isa"]) not in pinned)
    ]


def build_corpus(
    jobs: int, cases: set[str] | None = None, suites: set[str] | None = None,
    missing_only: bool = False, rebuild: bool = False,
    batch_size: int | None = None,
) -> None:
    inventory = rows()
    prior = []
    if BUILT.is_file():
        with BUILT.open(newline="") as source:
            prior = list(csv.DictReader(source, delimiter="\t"))
    resume = missing_only or (cases is None and suites is None and not rebuild)
    pending = select_rows(inventory, prior, cases, suites, resume)
    if batch_size is not None:
        pending = pending[:batch_size]
    if cases is not None or suites is not None or resume or batch_size is not None:
        found = {record["case"] for record in pending}
        if cases is not None and not resume and batch_size is None and found != cases:
            raise SystemExit(f"unknown or inactive cases: {sorted(cases - found)}")
        build_selected(pending, jobs)
        updated = []
        if BUILT.is_file():
            with BUILT.open(newline="") as source:
                updated = list(csv.DictReader(source, delimiter="\t"))
        remaining = len(select_rows(inventory, updated, cases, suites, True))
        if remaining > 0:
            print(f"build: batch complete; {remaining} selected artifact(s) remain unpinned")
        return
    skipped = [dict(record, state="skipped") for record in inventory if record["state"] != "build"]
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        results = list(pool.map(build_one, pending))
    results.extend(skipped)
    results.sort(key=lambda item: (item["suite"], item["case"], item["isa"]))
    built = [row for row in results if row["state"] == "built"]
    BUILT.parent.mkdir(parents=True, exist_ok=True)
    built_fields = [
        "suite", "case", "isa", "artifact", "sha256", "size", "toolchain",
        "source", "source_sha256", "cflags", "recipe_sha256", "exit", "stdout",
    ]
    write_table(BUILT, built_fields, built)
    write_report(inventory, built)
    report_counts(results, "build")


def build_selected(pending: list[dict[str, str]], jobs: int) -> None:
    if not pending:
        pinned = []
        if BUILT.is_file():
            with BUILT.open(newline="") as source:
                pinned = list(csv.DictReader(source, delimiter="\t"))
        write_report(rows(), pinned)
        print("targeted-build: discovered=0")
        return
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        results = list(pool.map(build_one, pending))
    failed = [record for record in results if record["state"] != "built"]
    if failed:
        for record in failed:
            print(f"build failed: {record['case']}/{record['isa']}: {record['reason']}")
        raise SystemExit(1)
    prior = []
    if BUILT.is_file():
        with BUILT.open(newline="") as source:
            prior = list(csv.DictReader(source, delimiter="\t"))
    merged = merge_pins(prior, results)
    fields = [
        "suite", "case", "isa", "artifact", "sha256", "size", "toolchain",
        "source", "source_sha256", "cflags", "recipe_sha256", "exit", "stdout",
    ]
    write_table(BUILT, fields, merged)
    write_report(rows(), merged)
    report_counts(results, "targeted-build")


def write_report(
    inventory: list[dict[str, str]], pins: list[dict[str, str]]
) -> None:
    key = lambda record: (record["suite"], record["case"], record["isa"])
    pinned = {key(record): record for record in pins}
    report = []
    for record in inventory:
        rendered = dict(record)
        if record["state"] == "build":
            pin = pinned.get(key(record))
            rendered["state"] = "built" if pin else "pending"
            rendered["reason"] = "-" if pin else "unbuilt"
            if pin:
                rendered["cflags"] = pin["cflags"]
        else:
            rendered["state"] = "skipped"
        report.append(rendered)
    fields = list(inventory[0])
    write_table(REPORT, fields, report)


def write_table(
    path: Path, fields: list[str], records: list[dict[str, str]]
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = tempfile.NamedTemporaryFile(
        mode="w",
        newline="",
        prefix=f".{path.name}.",
        suffix=".tmp",
        dir=path.parent,
        delete=False,
    )
    temporary_path = Path(temporary.name)
    try:
        with temporary as target:
            writer = csv.DictWriter(
                target, fields, delimiter="\t", lineterminator="\n"
            )
            writer.writeheader()
            writer.writerows(
                {name: record.get(name, "") for name in fields}
                for record in records
            )
        os.replace(temporary_path, path)
    finally:
        temporary_path.unlink(missing_ok=True)


def merge_pins(
    prior: list[dict[str, str]], replacements: list[dict[str, str]]
) -> list[dict[str, str]]:
    key = lambda record: (record["suite"], record["case"], record["isa"])
    selected = {key(record): record for record in replacements}
    # A focused rebuild must be monotonic. Rust-owned fixture pins can remain
    # valid while the retained C oracle changes disposition or temporarily
    # omits their source; only an explicit replacement may alter them.
    merged = [record for record in prior if key(record) not in selected]
    merged.extend(replacements)
    merged.sort(key=key)
    return merged


def verify() -> None:
    failures = []
    plan = rows()
    expected = {
        (record["suite"], record["case"], record["isa"])
        for record in plan if record["state"] == "build"
    }
    seen = []
    with BUILT.open(newline="") as source:
        for record in csv.DictReader(source, delimiter="\t"):
            seen.append((record["suite"], record["case"], record["isa"]))
            artifact = ROOT / record["artifact"]
            source_path = ORACLE / record["source"]
            if not artifact.is_file() or digest(artifact) != record["sha256"]:
                failures.append(f"artifact:{record['artifact']}")
            elif artifact.stat().st_size != int(record["size"]):
                failures.append(f"size:{record['artifact']}")
            if not source_path.is_file() or digest(source_path) != record["source_sha256"]:
                failures.append(f"source:{record['source']}")
            if text_digest(record["cflags"]) != record["recipe_sha256"]:
                failures.append(f"recipe:{record['suite']}/{record['case']}")
    present = set(seen)
    for key in sorted(expected - present):
        failures.append(f"missing-pin:{'/'.join(key)}")
    for key in sorted(present - expected):
        failures.append(f"orphan-pin:{'/'.join(key)}")
    if len(seen) != len(present):
        failures.append(f"duplicate-pins:{len(seen) - len(present)}")
    verify_runtime(failures)
    if failures:
        raise SystemExit("corpus drift:\n  " + "\n  ".join(failures))
    print("verify: ok")


def verify_runtime(failures: list[str]) -> None:
    manifest = RUNTIME / "manifest.tsv"
    if not manifest.is_file():
        failures.append("runtime:manifest")
        return
    license_path = RUNTIME / "COPYING.LIB"
    if (not license_path.is_file() or license_path.is_symlink()
            or digest(license_path) != "20e50fe7aae3e56378ebf0417d9de904f55a0e61e4df315333e632a4d3555d95"):
        failures.append("runtime:license")
    if not (RUNTIME / "NOTICE.md").is_file():
        failures.append("runtime:notice")
    with manifest.open(newline="") as source:
        records = list(csv.DictReader(source, delimiter="\t"))
    declared = {record["path"] for record in records}
    present = {
        str(path.relative_to(RUNTIME)) for path in RUNTIME.rglob("*")
        if path.is_file() and path.name not in {"manifest.tsv", "NOTICE.md", "COPYING.LIB"}
    }
    if declared != present:
        failures.append("runtime:file-set")
    for record in records:
        path = RUNTIME / record["path"]
        if (record["type"] != "file" or record["target"] != "-"
                or not path.is_file() or path.is_symlink()):
            failures.append(f"runtime:type:{record['path']}")
            continue
        if digest(path) != record["sha256"]:
            failures.append(f"runtime:digest:{record['path']}")
        if path.stat().st_size != int(record["size"]):
            failures.append(f"runtime:size:{record['path']}")
        if path.stat().st_mode & 0o7777 != int(record["mode"], 8):
            failures.append(f"runtime:mode:{record['path']}")


def report_counts(records: list[dict[str, str]], label: str) -> None:
    counts: dict[str, int] = {}
    for record in records:
        counts[record["state"]] = counts.get(record["state"], 0) + 1
    summary = " ".join(f"{key}={counts[key]}" for key in sorted(counts))
    print(f"{label}: discovered={len(records)} {summary}")


def main() -> None:
    parser = argparse.ArgumentParser()
    subcommands = parser.add_subparsers(dest="command", required=True)
    importer = subcommands.add_parser("import")
    importer.add_argument("source", type=Path)
    builder = subcommands.add_parser("build")
    builder.add_argument("--jobs", type=int, default=1)
    builder.add_argument("--case", action="append", default=[])
    builder.add_argument("--suite", action="append", default=[])
    builder.add_argument("--missing-only", action="store_true")
    builder.add_argument("--rebuild", action="store_true")
    builder.add_argument("--batch-size", type=int)
    subcommands.add_parser("verify")
    auditor = subcommands.add_parser("audit-cmake")
    auditor.add_argument("build", type=Path)
    cmake_importer = subcommands.add_parser("import-cmake")
    cmake_importer.add_argument("build", type=Path)
    cmake_importer.add_argument("--case", action="append", default=[])
    cmake_importer.add_argument("--suite", action="append", default=[])
    cmake_importer.add_argument("--isa", action="append", choices=sorted(COMPILERS), default=[])
    cmake_importer.add_argument(
        "--unique", action="store_true",
        help="import unique outputs while reporting and skipping unresolved rows",
    )
    arguments = parser.parse_args()
    if arguments.command == "import":
        import_corpus(arguments.source)
    elif arguments.command == "build":
        if arguments.jobs < 1:
            raise SystemExit("--jobs must be at least one")
        if arguments.batch_size is not None and arguments.batch_size < 1:
            raise SystemExit("--batch-size must be at least one")
        build_corpus(
            arguments.jobs, set(arguments.case) or None,
            set(arguments.suite) or None, arguments.missing_only,
            arguments.rebuild, arguments.batch_size,
        )
    elif arguments.command == "verify":
        verify()
    elif arguments.command == "audit-cmake":
        audit_cmake(arguments.build)
    else:
        import_cmake(
            arguments.build, set(arguments.case) or None,
            set(arguments.suite) or None, set(arguments.isa) or None,
            arguments.unique,
        )


if __name__ == "__main__":
    main()
