# Daemon, Test, and Documentation Gaps

This file covers daemon architecture, Docker API mismatches, build/test false greens, and stale documentation.

## Fractional `--cpus` Loses Quota Precision

Priority: P1
Impact: cgroup CPU quota is too high for fractional limits
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-slot-e`.

Evidence:

- `nano_cpus_to_cpus` rounds `NanoCpus` up to whole CPUs: `dd-daemon/src/runtime/spawn/spec.rs:21`.
- Spawn config forwards only the rounded integer CPU count: `dd-daemon/src/runtime/spawn/mod.rs:53`.
- cgroup `cpu.max` renders `g_cpu_max * 100000`: `dd-jit-darwin/src/runtime/os/linux/container/vfs.c:3427`.

Why this is bad:

Docker `--cpus=0.5` should expose quota `50000 100000`. dd rounds it to one CPU, so runtimes sizing from cgroups see twice the requested CPU budget.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-slot-e-target cargo test -p dd-daemon fractional_nano_cpus_needs_fractional_cgroup_quota -- --nocapture
```

Result: failed as intended; left `1`, right `0`.

## Gap and Architecture Docs Are Not Auditable

Priority: P2
Impact: xfail rationale and architecture state drift
Confidence: High

Evidence:

- Source comments reference `docs/GAPS.md`, `docs/SYSCALLS.md`, `docs/IMAGE-MANIFEST.md`, `docs/TESTING.md`, and `docs/CHARTER.md`.
- These files are absent in the current `docs/` root (`test -e` returned nonzero for each during this audit).
- `docs/ENGINE_HOLES.md` says default NaN sign and runtime DF are fixed near the top, but later still lists DIVSS/DIVPS NaN sign and DF as open: `docs/ENGINE_HOLES.md:6`, `docs/ENGINE_HOLES.md:410`, `docs/ENGINE_HOLES.md:414`.
- Current translator code has `emit_dnan_pre/post` and runtime `cpu->df` handling: `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:872`, `dd-jit-darwin/src/runtime/translate/x86_64/translate.c:385`.

Why this is bad:

Fix agents and reviewers cannot reliably tell which gaps are accepted, fixed, stale, or still open. Xfail comments name a missing taxonomy, and current architecture docs contradict source state.

Suggested improvement:

Create one canonical gap registry with:

- stable id
- owner
- affected engines
- severity
- source evidence
- test or xfail case
- expected Linux/Docker behavior
- current dd behavior
- close condition

