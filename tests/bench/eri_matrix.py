#!/usr/bin/env python3
"""Resumable, fail-closed E/R/I performance cutover matrix."""

import argparse
import fcntl
import hashlib
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

SCHEMA = "husklet-eri-v1"
CELLS = (("E", "E"), ("R", "R"), ("I", "I"), ("E", "R"), ("E", "I"), ("R", "I"))
ORDER = ((0, 1), (1, 0), (1, 0), (0, 1))
PHASE = re.compile(r"^PHASE\s+(\S+)\s+.*?us=(\d+)\s+.*?ok=(\S+)(?:\s|$)")
TIMING = re.compile(rb"(?m)(^PHASE\s+\S+\s+.*?)us=\d+")


def digest(path):
    value = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def canonical(stdout, stderr):
    """Timing is nondeterministic; every other output byte is contractual."""
    return hashlib.sha256(TIMING.sub(rb"\1us=<time>", stdout) + b"\0stderr\0" + stderr).hexdigest()


def parse_phases(stdout):
    found = {}
    for line in stdout.decode("utf-8", "strict").splitlines():
        match = PHASE.match(line)
        if match:
            name, micros, checksum = match.groups()
            if name in found:
                raise RuntimeError(f"duplicate PHASE {name}")
            found[name] = (int(micros), checksum)
    if not found:
        raise RuntimeError("no PHASE lines")
    return found


def validate(config):
    if config.get("schema") != SCHEMA:
        raise ValueError(f"schema must be {SCHEMA}")
    if set(config.get("arms", {})) != {"E", "R", "I"}:
        raise ValueError("arms must be exactly E, R, and I")
    if set(config.get("workloads", {})) != {"python", "sqlite", "malloc"}:
        raise ValueError("workloads must be exactly python, sqlite, and malloc")
    for label, arm in config["arms"].items():
        if not isinstance(arm.get("command"), list) or not arm["command"]:
            raise ValueError(f"arm {label} command must be a nonempty argv array")
        executable = Path(arm["command"][0]).resolve()
        if not executable.is_file():
            raise ValueError(f"arm {label} executable does not exist: {executable}")
        arm["command"][0] = str(executable)
        arm["sha256"] = digest(executable)
    for name, workload in config["workloads"].items():
        if not isinstance(workload.get("argv"), list) or not workload["argv"]:
            raise ValueError(f"workload {name} argv must be a nonempty array")
        guest = Path(workload["argv"][0]).resolve()
        if not guest.is_file():
            raise ValueError(f"workload {name} guest does not exist: {guest}")
        workload["argv"][0] = str(guest)
        workload["sha256"] = digest(guest)
    rounds = config.get("rounds", 8)
    if not isinstance(rounds, int) or rounds < 4 or rounds % 4:
        raise ValueError("rounds must be a positive multiple of four (minimum 4)")
    config["rounds"] = rounds
    return config


def campaign_identity(config):
    return hashlib.sha256(json.dumps(config, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def acquire(path, timeout):
    descriptor = open(path, "a+")
    deadline = time.monotonic() + timeout
    while True:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return descriptor
        except BlockingIOError:
            if time.monotonic() >= deadline:
                descriptor.close()
                raise TimeoutError(f"timed out acquiring {path}")
            time.sleep(1)


def busy():
    commands = (("pgrep", "-cx", "testing"), ("pgrep", "-c", "hl_engine-|hl-aarch64|hl-x86_64"), ("pgrep", "-cx", "cargo"))
    return any(subprocess.run(command, capture_output=True, check=False).returncode == 0 for command in commands)


def sustained_quiet(seconds, timeout):
    start = None
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if busy():
            start = None
        elif start is None:
            start = time.monotonic()
        elif time.monotonic() - start >= seconds:
            return
        time.sleep(min(5, max(0.1, seconds)))
    raise TimeoutError("box did not remain quiet for the required interval")


def load_rows(path):
    rows = []
    if path.exists():
        with open(path, encoding="utf-8") as source:
            for number, line in enumerate(source, 1):
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError as error:
                    raise RuntimeError(f"corrupt ledger line {number}: {error}") from error
    return rows


def run_one(config, workload, arm):
    argv = config["arms"][arm]["command"] + config["workloads"][workload]["argv"]
    result = subprocess.run(argv, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(f"{workload}/{arm} exited {result.returncode}: {result.stderr.decode(errors='replace')}")
    phases = parse_phases(result.stdout)
    expected = config["workloads"][workload].get("phases")
    if expected is not None and set(phases) != set(expected):
        raise RuntimeError(f"{workload}/{arm} phases {sorted(phases)} != expected {sorted(expected)}")
    return phases, canonical(result.stdout, result.stderr)


def append_row(handle, row):
    handle.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
    handle.flush()
    os.fsync(handle.fileno())


def summarize(rows, limit):
    by_key = {(r["workload"], r["cell"], r["round"], r["position"]): r for r in rows}
    verdict = "PASS"
    lines = ["workload\tcell\tphase\tratio\tnull_floor\tupper\tverdict"]
    nulls = {}
    for workload in ("python", "sqlite", "malloc"):
        for arm in "ERI":
            ratios = {}
            for round_number in sorted({r["round"] for r in rows if r["workload"] == workload and r["cell"] == arm + arm}):
                a = by_key[(workload, arm + arm, round_number, 0)]
                b = by_key[(workload, arm + arm, round_number, 1)]
                for phase in a["phases"]:
                    ratios.setdefault(phase, []).append(b["phases"][phase]["us"] / a["phases"][phase]["us"])
            for phase, values in ratios.items():
                nulls[(workload, arm, phase)] = max(abs(statistics.median(values) - 1.0), max(abs(v - 1.0) for v in values))
        for left, right in CELLS[3:]:
            cell = left + right
            ratios = {}
            rounds = sorted({r["round"] for r in rows if r["workload"] == workload and r["cell"] == cell})
            for round_number in rounds:
                first = by_key[(workload, cell, round_number, 0)]
                second = by_key[(workload, cell, round_number, 1)]
                samples = {first["arm"]: first, second["arm"]: second}
                for phase in samples[left]["phases"]:
                    ratios.setdefault(phase, []).append(samples[right]["phases"][phase]["us"] / samples[left]["phases"][phase]["us"])
            for phase, values in sorted(ratios.items()):
                ratio = statistics.median(values)
                floor = max(nulls[(workload, left, phase)], nulls[(workload, right, phase)])
                upper = ratio * (1.0 + floor)
                passed = upper <= limit
                verdict = verdict if passed else "FAIL"
                lines.append(f"{workload}\t{cell}\t{phase}\t{ratio:.6f}\t{floor:.6f}\t{upper:.6f}\t{'PASS' if passed else 'FAIL'}")
    return verdict, "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--results", required=True, type=Path, help="new campaign directory")
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--limit", type=float, default=1.10)
    parser.add_argument("--minimum-free-gib", type=float, default=30.0)
    parser.add_argument("--quiet-seconds", type=int, default=120)
    parser.add_argument("--lock-timeout", type=int, default=900)
    args = parser.parse_args(argv)
    config = validate(json.loads(args.config.read_text()))
    identity = campaign_identity(config)
    manifest = args.results / "manifest.json"
    ledger = args.results / "raw.jsonl"
    if args.resume:
        if not manifest.exists() or json.loads(manifest.read_text()).get("identity") != identity:
            raise RuntimeError("resume refused: campaign manifest is absent or has a different identity")
    else:
        args.results.mkdir(parents=True, exist_ok=False)
        manifest.write_text(json.dumps({"identity": identity, "config": config}, sort_keys=True, indent=2) + "\n")
    if shutil.disk_usage(args.results).free < args.minimum_free_gib * 1024**3:
        raise RuntimeError("free disk is below --minimum-free-gib")
    rows = load_rows(ledger)
    completed = {(r["workload"], r["cell"], r["round"], r["position"]) for r in rows}
    intent = acquire("/var/tmp/husklet-box.wanted", args.lock_timeout)
    try:
        sustained_quiet(args.quiet_seconds, args.lock_timeout)
        box = acquire("/var/tmp/husklet-box.lock", args.lock_timeout)
    finally:
        fcntl.flock(intent, fcntl.LOCK_UN)
        intent.close()
    try:
        with open(ledger, "a", encoding="utf-8") as output:
            for workload in ("python", "sqlite", "malloc"):
                for left, right in CELLS:
                    cell = left + right
                    for round_number in range(config["rounds"]):
                        arms = (left, right)
                        for position, index in enumerate(ORDER[round_number % 4]):
                            key = (workload, cell, round_number, position)
                            if key in completed:
                                continue
                            arm = arms[index]
                            phases, output_identity = run_one(config, workload, arm)
                            row = {"arm": arm, "cell": cell, "output": output_identity, "phases": {name: {"us": us, "ok": ok} for name, (us, ok) in phases.items()}, "position": position, "round": round_number, "workload": workload}
                            append_row(output, row)
                            rows.append(row)
        expected_outputs = {}
        for row in rows:
            key = row["workload"]
            observed = (row["output"], tuple(sorted((p, v["ok"]) for p, v in row["phases"].items())))
            if key in expected_outputs and expected_outputs[key] != observed:
                raise RuntimeError(f"exact-output mismatch for {key}")
            expected_outputs[key] = observed
        verdict, report = summarize(rows, args.limit)
        (args.results / "report.tsv").write_text(report)
        (args.results / "verdict.txt").write_text(verdict + "\n")
        print(report, end="")
        print(f"VERDICT\t{verdict}\tlimit={args.limit:.3f}\tidentity={identity}")
        return 0 if verdict == "PASS" else 2
    finally:
        fcntl.flock(box, fcntl.LOCK_UN)
        box.close()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"eri-matrix: {error}", file=sys.stderr)
        sys.exit(1)
