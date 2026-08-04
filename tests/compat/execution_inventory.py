#!/usr/bin/env python3
"""Generate the executable compatibility inventory from pinned artifacts."""

from __future__ import annotations

import csv
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent
KEY = ("suite", "case", "isa")
FIELDS = [
    "suite", "case", "isa", "artifact", "expected_exit", "stdout_golden",
    "stderr_golden", "timeout_ms", "source_manifest", "dependencies",
    "environment", "disposition", "note",
]
ORACLE_TIMEOUT_MS = "120000"
SOAK_TIMEOUT_MS = "240000"


def table(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as source:
        return list(csv.DictReader(source, delimiter="\t"))


def render(root: Path = ROOT) -> str:
    plan_rows = table(root / "build-plan.tsv")
    plan = {tuple(row[name] for name in KEY): row for row in plan_rows}
    if len(plan) != len(plan_rows):
        raise ValueError(f"duplicate build-plan key: {len(plan_rows) - len(plan)}")
    expected = {key for key, row in plan.items() if row["state"] == "build"}
    artifacts = table(root / "artifacts/manifest.tsv")
    artifact_keys = [tuple(row[name] for name in KEY) for row in artifacts]
    present = set(artifact_keys)
    missing = sorted(expected - present)
    orphan = sorted(present - expected)
    duplicates = len(artifact_keys) - len(present)
    if missing or orphan or duplicates:
        raise ValueError(
            f"artifact coverage drift: missing={len(missing)} "
            f"orphan={len(orphan)} duplicates={duplicates}"
        )
    rows = []
    for artifact in artifacts:
        key = tuple(artifact[name] for name in KEY)
        record = plan.get(key)
        if record is None:
            raise ValueError(f"artifact has no buildable build-plan row: {key}")
        if record["state"] != "build":
            continue
        suite = record["suite"]
        source_manifest = (f"oracle/tests/soak/manifest.tsv" if suite == "soak"
                           else f"oracle/tests/compat/{suite}/manifest.tsv")
        stdout = record["stdout"]
        if stdout.startswith("tests/"):
            stdout = f"oracle/{stdout}"
        rows.append({
            "suite": suite, "case": record["case"], "isa": record["isa"],
            "artifact": artifact["artifact"], "expected_exit": record["exit"],
            "stdout_golden": stdout, "stderr_golden": "-",
            # matrix_runner.c uses 120s when HL_MATRIX_CASE_TIMEOUT_MS is absent;
            # Phase3Compat.cmake explicitly gives the soak suite 240s.
            "timeout_ms": SOAK_TIMEOUT_MS if suite == "soak" else ORACLE_TIMEOUT_MS,
            "source_manifest": source_manifest, "dependencies": record["dependencies"],
            "environment": record["env"], "disposition": record["disposition"],
            "note": record["note"],
        })
    prior = table(root / "inventory.tsv")
    rows.extend(row for row in prior if row["suite"] == "bootstrap")
    rows.sort(key=lambda row: tuple(row[name] for name in KEY))
    keys = [tuple(row[name] for name in KEY) for row in rows]
    if len(keys) != len(set(keys)):
        raise ValueError("duplicate execution inventory key")
    lines = ["\t".join(FIELDS)]
    lines.extend("\t".join(row[name] for name in FIELDS) for row in rows)
    return "\n".join(lines) + "\n"


def main() -> None:
    output = ROOT / "inventory.tsv"
    rendered = render()
    if "--check" in sys.argv[1:]:
        if not output.is_file() or output.read_text() != rendered:
            raise SystemExit(f"stale execution inventory: {output}; run execution_inventory.py")
        return
    output.write_text(rendered)


if __name__ == "__main__":
    main()
