#!/usr/bin/env python3
"""Generate E/R/I config only from engines that positively receipt their backend."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


BACKEND_RECEIPT_SCHEMA = "husklet-engine-backend-v1"


PYTHON_PROGRAM = """import time
start=time.monotonic_ns()
import json,os,re,sys
d={}
for i in range(2000000): d[i%1000]=d.get(i%1000,0)+i
ok=sum(d.values())
elapsed=(time.monotonic_ns()-start)//1000
print(f'PHASE python us={max(1,elapsed)} ok={ok}')
"""


def digest(path):
    value = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def tree_digest(root):
    value = hashlib.sha256()
    for path in sorted(root.rglob("*"), key=lambda item: os.fsencode(item.relative_to(root))):
        relative = os.fsencode(path.relative_to(root))
        value.update(len(relative).to_bytes(8, "big"))
        value.update(relative)
        if path.is_symlink():
            value.update(b"L" + os.fsencode(os.readlink(path)))
        elif path.is_file():
            value.update(b"F" + bytes.fromhex(digest(path)))
        elif path.is_dir():
            value.update(b"D")
        else:
            raise ValueError(f"unsupported rootfs entry: {path}")
    return value.hexdigest()


def executable(value, label):
    path = Path(value).resolve()
    if not path.is_file() or not os.access(path, os.X_OK):
        raise ValueError(f"{label} is not an executable file: {path}")
    return path


def backend_receipt(engine, backend, options=()):
    command = [str(engine), "--backend-receipt"]
    for option in options:
        command.extend(("--engine-option", option))
    completed = subprocess.run(command, capture_output=True, check=False)
    if completed.returncode or completed.stderr:
        raise ValueError(
            f"{backend} engine does not provide a quiet executable backend receipt; "
            "direct Program execution cannot prove ProductionFactory selection"
        )
    try:
        observed = json.loads(completed.stdout.decode("utf-8", "strict"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{backend} engine backend receipt is not strict JSON") from error
    expected = {
        "schema": BACKEND_RECEIPT_SCHEMA,
        "backend": backend,
        "engine_sha256": digest(engine),
    }
    if observed != expected:
        raise ValueError(f"{backend} engine backend receipt does not match the executable")
    return {"command": command, **expected}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--external", required=True)
    parser.add_argument("--retained", required=True)
    parser.add_argument("--integrated", required=True)
    parser.add_argument("--rootfs", required=True)
    parser.add_argument("--python", required=True)
    parser.add_argument("--combined", required=True)
    parser.add_argument("--combined-sqlite", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--rounds", type=int, default=8)
    args = parser.parse_args(argv)

    adapter = executable(Path(__file__).with_name("eri_adapter.py"), "ERI adapter")
    engines = {
        "E": executable(args.external, "external engine"),
        "R": executable(args.retained, "retained engine"),
        "I": executable(args.integrated, "integrated engine"),
    }
    receipts = {
        "R": backend_receipt(engines["R"], "retained-c", ("HL_EXECUTION_BACKEND=c",)),
        # The current integrated product deliberately embeds the retained-C backend. R/I are a
        # selector/default no-op control until a distinct integrated backend receipts its own name.
        "I": backend_receipt(engines["I"], "retained-c"),
    }
    rootfs = Path(args.rootfs).resolve()
    if not rootfs.is_dir():
        raise ValueError(f"rootfs is not a directory: {rootfs}")
    python = executable(args.python, "rootfs Python")
    try:
        python.relative_to(rootfs)
    except ValueError as error:
        raise ValueError("--python must be inside --rootfs") from error
    combined = executable(args.combined, "combined guest")
    sqlite = executable(args.combined_sqlite, "sqlite guest")
    for path, label in ((combined, "--combined"), (sqlite, "--combined-sqlite")):
        try:
            path.relative_to(rootfs)
        except ValueError as error:
            raise ValueError(f"{label} must be inside --rootfs") from error

    def arm(label, provider, extra=()):
        command = [
            str(adapter), "--provider", provider,
            "--engine", str(engines[label]), "--rootfs", str(rootfs),
            "--wall-phase", "python",
        ]
        command.extend(extra)
        command.append("--")
        result = {
            "command": command,
            "artifacts": {
                "adapter": {"path": str(adapter), "sha256": digest(adapter)},
                "engine": {"path": str(engines[label]), "sha256": digest(engines[label])},
            },
        }
        if label in receipts:
            result["backend_receipt"] = receipts[label]
        return result

    config = {
        "schema": "husklet-eri-v2",
        "rounds": args.rounds,
        "samples_per_row": 3,
        "rootfs": {"path": str(rootfs), "sha256": tree_digest(rootfs)},
        "arms": {
            "E": arm("E", "external"),
            "R": arm("R", "product", ("--engine-option", "HL_EXECUTION_BACKEND=c")),
            "I": arm("I", "product"),
        },
        "workloads": {
            "python": {"argv": [str(python), "-c", PYTHON_PROGRAM], "phases": ["python"]},
            "sqlite": {"argv": [str(sqlite), "--divisor", "2", "--phase", "sqlite"], "phases": ["sqlite"]},
            "malloc": {"argv": [str(combined), "--divisor", "2", "--phase", "malloc"], "phases": ["malloc"]},
        },
    }
    output = Path(args.output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(config, indent=2, sort_keys=True) + "\n")
    print(output)


if __name__ == "__main__":
    try:
        main()
    except ValueError as error:
        print(f"eri-config: {error}", file=sys.stderr)
        sys.exit(1)
