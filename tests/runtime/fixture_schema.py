#!/usr/bin/env python3
"""Classify launch and fixture requirements for every built corpus artifact."""

from __future__ import annotations

import csv
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent
KEY = ("suite", "case", "isa")


def table(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as source:
        return list(csv.DictReader(source, delimiter="\t"))


def tokens(value: str) -> set[str]:
    return set() if value == "-" else set(value.lower().split(","))


def groups(root: Path) -> dict[tuple[str, str], str]:
    result = {}
    base = root / "oracle/tests/compat"
    manifests = list(base.rglob("manifest.tsv"))
    soak = root / "oracle/tests/soak/manifest.tsv"
    if soak.is_file():
        manifests.append(soak)
    for manifest in sorted(manifests):
        suite = (manifest.parent.relative_to(base).as_posix()
                 if manifest.is_relative_to(base) else "soak")
        lines = manifest.read_text().splitlines()
        header = next((line.removeprefix("# ") for line in lines if line.startswith("# case\t")), None)
        if header is None:
            continue
        records = csv.DictReader([header, *(line for line in lines if line and not line.startswith("#"))], delimiter="\t")
        for record in records:
            result[(suite, record["case"])] = record.get("group", "-")
    return result


def classify(record: dict[str, str], group: str = "-") -> dict[str, str]:
    dependencies = tokens(record["dependencies"])
    arguments = record["defines"].removeprefix("argv:") if record["defines"].startswith("argv:") else "-"
    rootfs = next((name for name in ("dynamic-rootfs", "alpine-rootfs", "scratch-rootfs", "mapping-data-rootfs") if name in dependencies), "-")
    side_files = "-"
    if record["case"] == "pc-libmap":
        side_files = "pclib_blob_arm.bin" if record["isa"] == "aarch64" else "pclib_blob_x86.bin"
    symlink = record["case"] == "exec-symlink-entry"
    volume = record["env"].startswith("HL_VOLUMES=")
    process_terms = {"process", "fork", "fork/wait", "wait", "clone", "clone3", "vfork", "exec", "execve", "self-exec", "posix-spawn", "pthreads", "threads", "ipc"}
    device_terms = {"devnode", "pty", "termios", "mqueue", "sysv-ipc"}
    network_terms = {"network", "socket", "sockets", "ipv6", "bridge"}
    multiprocess = bool(dependencies & process_terms)
    self_contained_process = (group == "exec" or record["case"].startswith("exec-")
                              or record["case"].startswith("close-range-unshare")
                              or "self-contained-process" in dependencies)
    special = bool(dependencies & device_terms) or group == "device"
    explicit_transport = record["env"] in {"HL_NET_HOST=1", "HL_NET_ISOLATE=1"}
    network = bool(dependencies & network_terms) or ("HL_NET" in record["env"] and not explicit_transport) or group == "network"
    tree = rootfs in {"alpine-rootfs", "mapping-data-rootfs"} or volume
    if rootfs == "dynamic-rootfs":
        fixture = "rootfs-interpreter"
    elif rootfs != "-":
        fixture = "rootfs-tree" if tree else "rootfs-executable"
    elif side_files != "-":
        fixture = "side-file"
    elif symlink:
        fixture = "entry-symlink"
    elif volume:
        fixture = "directory-tree"
    elif special:
        fixture = "special-device"
    elif network:
        fixture = "network-sandbox"
    elif multiprocess and not self_contained_process:
        fixture = "multi-process-service"
    else:
        fixture = "executable"
    golden = record["stdout"]
    golden_schema = "isa" if any(f"/expected/{isa}/" in golden for isa in ("aarch64", "x86_64")) else "shared" if "/expected/shared/" in golden else "common"
    return {
        **{name: record[name] for name in KEY},
        "group": group,
        "fixture": fixture,
        "arguments": arguments,
        "environment": record["env"],
        "dependencies": record["dependencies"],
        "golden": record["stdout"],
        "golden_schema": golden_schema,
        "side_files": side_files,
        "rootfs": rootfs,
        "directory_tree": str(tree).lower(),
        "entry_symlink": str(symlink).lower(),
        "multi_process": str(multiprocess).lower(),
        "special_device": str(special).lower(),
        "network_setup": str(network).lower(),
    }


def bootstrap(root: Path) -> list[dict[str, str]]:
    records = []
    for row in table(root / "inventory.tsv"):
        if row["suite"] != "bootstrap":
            continue
        artifact = root / row["artifact"]
        if not artifact.is_file():
            raise ValueError(f"bootstrap artifact absent: {row['artifact']}")
        records.append(classify({
            "suite": row["suite"],
            "case": row["case"],
            "isa": row["isa"],
            "defines": "-",
            "env": row["environment"],
            "dependencies": row["dependencies"],
            "stdout": row["stdout_golden"],
        }))
    return records


def analyze(root: Path = ROOT) -> list[dict[str, str]]:
    plan_rows = [
        row for row in table(root / "build-plan.tsv") if row["state"] == "build"
    ]
    plan = {tuple(row[name] for name in KEY): row for row in plan_rows}
    if len(plan) != len(plan_rows):
        raise ValueError(f"duplicate build-plan key: {len(plan_rows) - len(plan)}")
    artifacts = table(root / "artifacts/manifest.tsv")
    artifact_keys = [tuple(row[name] for name in KEY) for row in artifacts]
    orphan = sorted(set(artifact_keys) - set(plan))
    missing = sorted(set(plan) - set(artifact_keys))
    duplicates = len(artifact_keys) - len(set(artifact_keys))
    if missing or orphan or duplicates:
        raise ValueError(
            f"corpus key drift: missing={len(missing)} "
            f"orphan={len(orphan)} duplicates={duplicates}"
        )
    source_groups = groups(root)
    rows = [classify(plan[key], source_groups.get(key[:2], "-")) for key in sorted(artifact_keys)]
    rows.extend(bootstrap(root))
    rows.sort(key=lambda row: tuple(row[name] for name in KEY))
    inventory = {tuple(row[name] for name in KEY) for row in table(root / "inventory.tsv")}
    row_keys = {tuple(row[name] for name in KEY) for row in rows}
    if row_keys != inventory or len(row_keys) != len(rows):
        raise ValueError(
            f"fixture inventory drift: missing={len(inventory - row_keys)} "
            f"extra={len(row_keys - inventory)} duplicates={len(rows) - len(row_keys)}"
        )
    return rows


def render(rows: list[dict[str, str]]) -> tuple[str, str]:
    fields = list(rows[0])
    manifest = "\n".join(["\t".join(fields), *("\t".join(row[name] for name in fields) for row in rows)]) + "\n"
    fixtures = Counter(row["fixture"] for row in rows)
    flags = [(name, sum(row[name] == "true" for row in rows)) for name in ("directory_tree", "entry_symlink", "multi_process", "special_device", "network_setup")]
    arguments = Counter(row["arguments"] for row in rows if row["arguments"] != "-")
    environments = Counter(row["environment"] for row in rows if row["environment"] != "-")
    rootfs = Counter(row["rootfs"] for row in rows if row["rootfs"] != "-")
    goldens = Counter(row["golden_schema"] for row in rows)
    dependency_values = {row["dependencies"] for row in rows}
    dependency_tokens = set().union(*(tokens(value) for value in dependency_values))
    report = [
        "# Compatibility fixture schema", "",
        "Generated by `fixture_schema.py` from every execution-inventory row: built full-corpus artifacts plus checked bootstrap seeds.",
        "Categories describe harness requirements; capability flags may overlap.", "",
        f"Rows audited: {len(rows)}", "",
        f"Distinct dependency strings: {len(dependency_values)}", f"Distinct dependency tokens: {len(dependency_tokens)}", "",
        "## Primary fixture", "", "| Fixture | Rows |", "|---|---:|",
        *(f"| `{name}` | {count} |" for name, count in sorted(fixtures.items())), "",
        "## Overlapping capabilities", "", "| Capability | Rows |", "|---|---:|",
        *(f"| `{name}` | {count} |" for name, count in flags), "",
        "## Root filesystems", "", "| Schema token | Rows |", "|---|---:|",
        *(f"| `{name}` | {count} |" for name, count in sorted(rootfs.items())), "",
        "## Golden schemas", "", "| Schema | Rows |", "|---|---:|",
        *(f"| `{name}` | {count} |" for name, count in sorted(goldens.items())), "",
        "## Launch arguments", "", "| Arguments | Rows |", "|---|---:|",
        *(f"| `{name}` | {count} |" for name, count in sorted(arguments.items())), "",
        "## Environments", "", "| Environment | Rows |", "|---|---:|",
        *(f"| `{name}` | {count} |" for name, count in sorted(environments.items())), "",
        "Every row has an explicit stdout golden path in the retained corpus.", "",
    ]
    return manifest, "\n".join(report)


def main() -> None:
    rows = analyze()
    manifest, report = render(rows)
    outputs = {ROOT / "fixture-schema.tsv": manifest, ROOT / "FIXTURE_SCHEMA.md": report}
    if "--check" in sys.argv[1:]:
        for path, expected in outputs.items():
            if not path.is_file() or path.read_text() != expected:
                raise SystemExit(f"stale fixture schema: {path}; run fixture_schema.py")
    else:
        for path, contents in outputs.items():
            path.write_text(contents)
    print(f"audited {len(rows)} execution fixture rows")


if __name__ == "__main__":
    main()
