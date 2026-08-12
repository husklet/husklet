#!/usr/bin/env python3
"""Run persistent guests through the retained C engines without early exit."""

from __future__ import annotations

import argparse
import csv
import os
from pathlib import Path
import signal
import subprocess
import time

ROOT = Path(__file__).resolve().parent
ARTIFACTS = ROOT / "artifacts" / "manifest.tsv"
PREBUILT = ROOT / "prebuilt" / "manifest.tsv"
FIELDS = [
    "suite", "case", "isa", "status", "exit", "expected_exit",
    "stdout_match", "stderr_match", "duration_ms", "artifact", "reason",
]
OUTPUT_LIMIT = 1024 * 1024


def bytes_at(path: str) -> bytes | None:
    if path == "-":
        return b""
    for target in (ROOT / "oracle" / path, ROOT / path):
        if target.is_file():
            return target.read_bytes()
    return None


def bootstrap() -> dict[str, dict[str, str]]:
    lines = (ROOT / "manifest.tsv").read_text().splitlines()
    header = lines[0].removeprefix("# ").split("\t")
    return {
        row["case"]: row for row in csv.DictReader(
            lines[1:], fieldnames=header, delimiter="\t",
        )
    }


def run(record: dict[str, str], engines: dict[str, Path], timeout_ms: int) -> dict[str, str]:
    artifact = ROOT / record["artifact"]
    engine = engines.get(record["isa"])
    expected = bytes_at(record["stdout"])
    base = {
        "suite": record["suite"], "case": record["case"], "isa": record["isa"],
        "expected_exit": record["exit"], "artifact": record["artifact"],
    }
    if engine is None or not engine.is_file():
        return base | verdict("missing", "-", "false", "false", 0, "missing-c-engine")
    if not artifact.is_file():
        return base | verdict("missing", "-", "false", "false", 0, "missing-artifact")
    if expected is None:
        return base | verdict("missing", "-", "false", "false", 0, "missing-stdout-golden")
    started = time.monotonic_ns()
    process = subprocess.Popen(
        [str(engine), str(artifact)], cwd=ROOT, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
    )
    reason = "-"
    status = "fail"
    try:
        stdout, stderr = process.communicate(timeout=timeout_ms / 1000)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            stdout, stderr = process.communicate(timeout=1)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
        status = "timeout"
        reason = "timeout"
    duration = (time.monotonic_ns() - started) // 1_000_000
    stdout_bounded = len(stdout) <= OUTPUT_LIMIT
    stderr_bounded = len(stderr) <= OUTPUT_LIMIT
    stdout_match = stdout_bounded and stdout == expected
    stderr_match = stderr_bounded and stderr == b""
    exit_value = str(process.returncode)
    if status != "timeout":
        expected_exit = int(record["exit"])
        if process.returncode == expected_exit and stdout_match and stderr_match:
            status = "pass"
        elif not stdout_bounded or not stderr_bounded:
            reason = "output-limit"
        elif process.returncode != expected_exit:
            reason = "exit-mismatch"
        elif not stdout_match:
            reason = "stdout-mismatch"
        else:
            reason = "stderr-mismatch"
    return base | verdict(
        status, exit_value, str(stdout_match).lower(), str(stderr_match).lower(), duration, reason,
    )


def verdict(status: str, exit_value: str, stdout: str, stderr: str, duration: int, reason: str) -> dict[str, str]:
    return {
        "status": status, "exit": exit_value, "stdout_match": stdout,
        "stderr_match": stderr, "duration_ms": str(duration), "reason": reason,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--aarch64-engine", type=Path, required=True)
    parser.add_argument("--x86-64-engine", type=Path, required=True)
    parser.add_argument("--timeout-ms", type=int, default=10_000)
    parser.add_argument("--output", type=Path, default=ROOT / "c_results.tsv")
    parser.add_argument("--resume", action="store_true")
    arguments = parser.parse_args()
    if arguments.timeout_ms < 100 or arguments.timeout_ms > 3_600_000:
        parser.error("timeout must be between 100ms and one hour")
    with ARTIFACTS.open(newline="") as source:
        records = list(csv.DictReader(source, delimiter="\t"))
    with PREBUILT.open(newline="") as source:
        expected = bootstrap()
        header = source.readline().removeprefix("# ").rstrip("\n").split("\t")
        for record in csv.DictReader(source, fieldnames=header, delimiter="\t"):
            oracle = expected[record["case"]]
            records.append({
                "suite": "bootstrap", "case": record["case"], "isa": record["isa"],
                "artifact": record["artifact"], "exit": oracle["exit"],
                "stdout": oracle["stdout"],
            })
    engines = {"aarch64": arguments.aarch64_engine.resolve(), "x86_64": arguments.x86_64_engine.resolve()}
    prior = []
    if arguments.resume and arguments.output.is_file():
        with arguments.output.open(newline="") as source:
            prior = list(csv.DictReader(source, delimiter="\t"))
    completed = {
        (row["suite"], row["case"], row["isa"])
        for row in prior if row["status"] != "missing"
    }
    prior = [row for row in prior if row["status"] != "missing"]
    results = prior + [
        run(record, engines, arguments.timeout_ms) for record in records
        if (record["suite"], record["case"], record["isa"]) not in completed
    ]
    results.sort(key=lambda row: (row["suite"], row["case"], row["isa"]))
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    with arguments.output.open("w", newline="") as target:
        writer = csv.DictWriter(target, FIELDS, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        writer.writerows(results)
    counts: dict[str, int] = {}
    for result in results:
        counts[result["status"]] = counts.get(result["status"], 0) + 1
    markdown = arguments.output.with_name("C_RESULTS.md")
    markdown.write_text(
        "# Retained C baseline\n\n" + "\n".join(
            f"- {name}: {counts[name]}" for name in sorted(counts)
        ) + "\n",
    )
    print(" ".join(f"{name}={counts[name]}" for name in sorted(counts)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
