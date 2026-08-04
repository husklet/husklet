# Run-option compatibility oracle

The retired repository scenario implementation was inspected before its
deletion, including the former `Case::run`, `Case::command`, `Case::detached`,
and `Case::name` paths. The process-option cohort owns no engine
state: each case constructs an independent `ContainerSpec`, `Containers` owns
the resulting container identity and lifetime, and every migrated test removes
its container before returning.

The exact preserved contracts are:

| Legacy ID | Public Rust owner and assertion |
|---|---|
| `runflags/detached-d` | `Containers::start`; inspect reports an active state before forced teardown |
| `runflags/env-e` | `Process::env`; `printenv FOO` writes exactly `barbaz\n` |
| `runflags/workdir-w` | `Process::working_dir`; `pwd` writes exactly `/var/spool\n` |
| `runflags/name` | `ContainerSpec::name`; inspect preserves `rf-named` |
| `runflags/entrypoint` | `Process::new`; replacing the program writes exactly `ENTRYOVERRIDE\n` |
| `runflags/user-uidgid` | `Process::user`; `id` reports exactly uid 1000, gid 1000, and group 1000 |
| `runflags/exit-code` | `Containers::wait`; guest exit 42 remains `ExitStatus::Code(42)` |

Ordering follows the former cases: create, start where execution is required,
wait for terminating processes, inspect/log, then remove. No partial-result,
blocking, cancellation, signal, errno, architecture-specific, or host-specific
branch is part of these option contracts. Guest architecture selection remains
explicit through `HL_SCENARIO_TARGET`; the pinned Alpine archive supplies the
matching guest userspace. No C-engine implementation was changed, so a C runtime
domain audit is not applicable to this test-ownership migration.
