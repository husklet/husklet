#!/usr/bin/env python3
"""Generate a concrete rootfs-aware E/R/I campaign configuration."""

import argparse
import hashlib
import json
import os
from pathlib import Path


PYTHON_PROGRAM = """import time
start=time.monotonic_ns()
import json,os,re,sys
d={}
for i in range(200000): d[i%1000]=d.get(i%1000,0)+i
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
        return {
            "command": command,
            "artifacts": {
                "adapter": {"path": str(adapter), "sha256": digest(adapter)},
                "engine": {"path": str(engines[label]), "sha256": digest(engines[label])},
            },
        }

    config = {
        "schema": "husklet-eri-v1",
        "rounds": args.rounds,
        "rootfs": {"path": str(rootfs), "sha256": tree_digest(rootfs)},
        "arms": {
            "E": arm("E", "external"),
            "R": arm("R", "product", ("--engine-option", "HL_EXECUTION_BACKEND=c")),
            "I": arm("I", "product"),
        },
        "workloads": {
            "python": {"argv": [str(python), "-c", PYTHON_PROGRAM], "phases": ["python"]},
            "sqlite": {"argv": [str(sqlite), "--divisor", "20", "--phase", "sqlite"], "phases": ["sqlite"]},
            "malloc": {"argv": [str(combined), "--divisor", "20", "--phase", "malloc"], "phases": ["malloc"]},
        },
    }
    output = Path(args.output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(config, indent=2, sort_keys=True) + "\n")
    print(output)


if __name__ == "__main__":
    main()
