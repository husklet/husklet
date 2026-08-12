#!/usr/bin/env python3
"""Run one rootfs-aware engine sample and emit canonical ERI PHASE output."""

import argparse
from pathlib import Path
import re
import subprocess
import sys
import time


PHASE = re.compile(r"^PHASE\s+(\S+)\s+.*?us=(\d+)\s+.*?ok=(\S+)(?:\s|$)")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provider", required=True, choices=("external", "product"))
    parser.add_argument("--arch", default="arm64")
    parser.add_argument("--engine", required=True)
    parser.add_argument("--rootfs", required=True)
    parser.add_argument("--engine-option", action="append", default=[])
    parser.add_argument("--wall-phase", action="append", default=[])
    parser.add_argument("guest", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    guest = args.guest
    if guest and guest[0] == "--":
        guest = guest[1:]
    if not guest:
        parser.error("a guest executable is required after --")
    try:
        provider_guest = "/" + str(Path(guest[0]).resolve().relative_to(Path(args.rootfs).resolve()))
    except ValueError:
        parser.error("guest executable must be inside --rootfs")

    if args.provider == "external":
        if args.engine_option:
            parser.error("--engine-option is valid only for the product provider")
        command = [args.engine, "--rootfs", args.rootfs, provider_guest]
    else:
        command = [args.engine, "--guest-isa", args.arch]
        for option in args.engine_option:
            command.extend(("--engine-option", option))
        command.extend(("--rootfs", args.rootfs, guest[0]))
    command.extend(guest[1:])
    started = time.monotonic_ns()
    completed = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    wall = max(1, (time.monotonic_ns() - started) // 1000)
    if completed.returncode:
        sys.stderr.buffer.write(completed.stderr)
        return completed.returncode
    try:
        rows = []
        for line in completed.stdout.decode("utf-8", "strict").splitlines():
            match = PHASE.match(line)
            if match:
                rows.append(match.groups())
        if not rows:
            raise ValueError("engine returned no benchmark rows")
        phases = set()
        for phase, micros, checksum in rows:
            if phase in phases:
                raise ValueError(f"engine returned duplicate phase {phase}")
            phases.add(phase)
            micros = wall if phase in args.wall_phase else int(micros)
            print(f"PHASE {phase} us={micros} ok={checksum}")
    except (KeyError, UnicodeDecodeError, ValueError) as error:
        sys.stderr.write(f"eri-adapter: invalid engine output: {error}\n")
        sys.stderr.buffer.write(completed.stdout)
        sys.stderr.buffer.write(completed.stderr)
        return 65
    return 0


if __name__ == "__main__":
    sys.exit(main())
