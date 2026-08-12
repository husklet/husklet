#!/usr/bin/env python3
"""Join retained-C and Rust compatibility outcomes by stable case identity."""

from __future__ import annotations

import csv
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPORT = ROOT / "report"


def read(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as source:
        return list(csv.DictReader(source, delimiter="\t"))


def main() -> int:
    c_rows = read(ROOT / "c_results.tsv")
    rust = {(row["suite"], row["case"], row["isa"]): row for row in read(REPORT / "results.tsv")}
    rows = []
    for c_row in c_rows:
        key = (c_row["suite"], c_row["case"], c_row["isa"])
        rust_row = rust.get(key)
        rust_status = rust_row["status"] if rust_row else "missing"
        rust_exit = rust_row["exit"] if rust_row else "-"
        if c_row["status"] == "timeout":
            classification = "c-timeout"
        elif c_row["status"] != "pass":
            classification = "c-fail"
        elif rust_status == "pass":
            classification = "both-pass"
        else:
            classification = "rust-gap"
        rows.append({
            "suite": key[0], "case": key[1], "isa": key[2],
            "classification": classification,
            "c_status": c_row["status"], "c_exit": c_row["exit"],
            "rust_status": rust_status, "rust_exit": rust_exit,
            "artifact": c_row["artifact"],
            "reason": c_row["reason"] if classification.startswith("c-") else (
                rust_row["reason"] if rust_row else "missing-rust-result"
            ),
        })
    fields = list(rows[0])
    with (REPORT / "differential.tsv").open("w", newline="") as target:
        writer = csv.DictWriter(target, fields, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)
    totals = Counter(row["classification"] for row in rows)
    clusters = Counter(
        (row["suite"], row["isa"], row["rust_status"], row["rust_exit"])
        for row in rows if row["classification"] == "rust-gap"
    )
    lines = ["# C/Rust differential", "", "## Totals", ""]
    lines.extend(f"- {name}: {totals[name]}" for name in sorted(totals))
    lines.extend(["", "## Rust gaps by suite, ISA, and exit", "", "| Suite | ISA | Rust status | Rust exit | Count |", "|---|---|---|---:|---:|"])
    for (suite, isa, status, exit_value), count in sorted(clusters.items(), key=lambda item: (-item[1], item[0])):
        lines.append(f"| {suite} | {isa} | {status} | {exit_value} | {count} |")
    (REPORT / "DIFFERENTIAL.md").write_text("\n".join(lines) + "\n")
    print(" ".join(f"{name}={totals[name]}" for name in sorted(totals)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
