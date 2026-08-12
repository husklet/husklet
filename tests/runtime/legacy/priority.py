#!/usr/bin/env python3
"""Rank retained C-pass cases by explicit production syscall evidence."""

from __future__ import annotations

import csv
import re
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent
TOKEN = re.compile(r"\b(?:SYS_|__NR_)([a-zA-Z0-9_]+)\b")


def table(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as source:
        return list(csv.DictReader(source, delimiter="\t"))


def family(name: str) -> str:
    groups = {
        "descriptor": ("close", "dup", "fcntl", "ioctl", "pipe", "eventfd", "epoll", "pselect"),
        "filesystem": ("open", "stat", "fstat", "faccess", "chmod", "fchmod", "chown", "link", "readlink", "rename", "unlink", "mkdir", "getdents", "xattr", "mount", "fallocate", "flock", "truncate", "ftruncate", "copy_file", "name_to_handle", "utimens", "fdatasync", "sync", "getcwd", "inotify", "fanotify", "cachestat", "pwrite"),
        "memory": ("mmap", "munmap", "mprotect", "mremap", "madvise", "mincore", "mlock", "memfd", "brk", "process_vm", "get_mempolicy"),
        "network": ("socket", "bind", "listen", "accept", "connect", "send", "recv", "shutdown", "getsock", "setsock"),
        "signal": ("rt_sig", "sigalt", "signalfd", "kill", "tgkill"),
        "time": ("clock", "timer", "getitimer", "nanosleep", "gettimeofday", "adjtimex", "times"),
        "process": ("clone", "fork", "exec", "wait", "exit", "pidfd", "set_tid", "getpid", "gettid", "prctl", "seccomp", "unshare", "setns", "kcmp"),
        "scheduling": ("sched", "getcpu", "getpriority", "ioprio", "getrusage", "prlimit"),
        "ipc": ("sem", "shm", "msg", "mq", "splice", "tee", "vmsplice"),
        "synchronization": ("futex", "membarrier", "rseq", "get_robust"),
        "identity": ("uid", "gid", "getresgid", "groups", "getgroups", "setfsuid", "cap", "uname", "sysinfo"),
        "asynchronous-io": ("io_",),
        "security": ("landlock",),
    }
    for group, prefixes in groups.items():
        if name.startswith(prefixes):
            return group
    return "other"


def evidence(source: str, known: dict[str, str]) -> list[str]:
    return sorted({name for name in TOKEN.findall(source) if name in known})


def select(names: list[str], known: dict[str, str]) -> tuple[str, str]:
    rank = {"missing": 0, "router-domain-only": 1, "supported": 2}
    gaps = sorted(names, key=lambda name: (rank[known[name]], name))
    return (gaps[0], known[gaps[0]]) if gaps else ("-", "no-explicit-syscall")


def analyze(root: Path = ROOT) -> list[dict[str, str]]:
    plan = {(row["suite"], row["case"], row["isa"]): row for row in table(root / "build-plan.tsv") if row["disposition"] == "active"}
    passes = [row for row in table(root / "report/c_results.tsv") if row["status"] == "pass"]
    known = {
        row["name"]: row["status"]
        for row in table(root.parents[2] / "src/apps/testing/syscall-audit/syscall-inventory.tsv")
    }
    with (root / "aliases.tsv").open(newline="") as source:
        aliases = {
            (row[0], row[1]): row[3]
            for row in csv.reader(source, delimiter="\t")
            if row and not row[0].startswith("#")
        }
    output = []
    cache: dict[str, list[str]] = {}
    for result in passes:
        key = (result["suite"], result["case"], result["isa"])
        record = plan.get(key)
        if record is None or result["suite"] == "bootstrap":
            continue
        path = root / "oracle" / record["source"]
        if not path.is_file() and (key[0], key[1]) in aliases:
            path = root / "oracle" / aliases[(key[0], key[1])]
        names = cache.setdefault(str(path), evidence(path.read_text(errors="ignore"), known))
        syscall, status = select(names, known)
        subsystem = family(syscall) if syscall != "-" else record["suite"]
        output.append({
            "suite": key[0], "case": key[1], "isa": key[2],
            "subsystem": subsystem, "primary_syscall": syscall,
            "production_status": status, "explicit_syscalls": ",".join(names) or "-",
        })
    return sorted(output, key=lambda row: (row["suite"], row["case"], row["isa"]))


def render(rows: list[dict[str, str]]) -> tuple[str, str]:
    fields = list(rows[0])
    lines = ["\t".join(fields), *("\t".join(row[field] for field in fields) for row in rows)]
    gaps = [row for row in rows if row["production_status"] in {"missing", "router-domain-only"}]
    families = Counter(row["subsystem"] for row in gaps)
    calls = Counter((row["primary_syscall"], row["production_status"]) for row in gaps)
    report = [
        "# Compatibility production priorities", "",
        "Generated from active retained-C passes and explicit `SYS_*`/`__NR_*` source evidence.",
        "It deliberately excludes inferred libc internals and loader/execution failures.", "",
        f"C-pass active rows analyzed: {len(rows)}", f"Rows with an explicit production gap: {len(gaps)}", "",
        "## Subsystem volume", "", "| Subsystem | Rows |", "|---|---:|",
        *(f"| {name} | {count} |" for name, count in families.most_common()), "",
        "## Primary syscall gaps", "", "| Syscall | Status | Rows |", "|---|---|---:|",
        *(f"| `{name}` | {status} | {count} |" for (name, status), count in calls.most_common()), "",
    ]
    return "\n".join(lines) + "\n", "\n".join(report)


def main() -> None:
    rows = analyze()
    manifest, report = render(rows)
    outputs = {ROOT / "priority.tsv": manifest, ROOT / "COMPAT_PRIORITY.md": report}
    if "--check" in sys.argv[1:]:
        for path, expected in outputs.items():
            if not path.is_file() or path.read_text() != expected:
                raise SystemExit(f"stale priority output: {path}; run priority.py")
    else:
        for path, contents in outputs.items():
            path.write_text(contents)
    print(f"analyzed {len(rows)} retained C-pass rows")


if __name__ == "__main__":
    main()
