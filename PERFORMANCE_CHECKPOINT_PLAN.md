# Performance and checkpoint acceptance plan

This file is the durable definition of the performance and end-to-end checkpoint
goal. A green unit suite is not completion. Completion requires the production
binaries, GUI, containers, terminal state, and extension lifecycle to satisfy the
journeys and measurements below on the exact revision being shipped.

## Product outcome

A developer can use Husklet for ordinary work at same-ISA native speed. Closing a
workspace checkpoints the complete workspace quickly. Opening it later resumes the
same running work quickly, without relaunching commands or reconstructing a merely
similar UI.

Translated execution is not exempt from performance work. Cross-ISA execution must
be profiled in both directions and optimized from measured instruction costs. The
current 20x-plus instruction overhead and 30x-plus wall overhead are defects, not an
acceptable final baseline.

## Required execution matrix

Every row uses settled, hashed production workers and native-engine libraries. A
backend receipt must prove which backend actually executed; host/guest architecture
labels or a successful exit are not sufficient.

| Host | Guest | Ordinary execution | Checkpoint execution | Required evidence |
| --- | --- | --- | --- | --- |
| Linux x86_64 | x86_64 | Native supervised | Native and translated coverage | Native receipt, semantic hash, timing and counters |
| Linux AArch64 | AArch64 | Native supervised | Translated until native ARM capture is implemented | Native receipt on physical/real ARM; translated checkpoint journey |
| Linux x86_64 | AArch64 | Translated | Translated | Production run, two checkpoint cycles, counters |
| Linux AArch64 | x86_64 | Translated | Translated | Production run, two checkpoint cycles, counters |

QEMU/TCG may prove cross-ISA and ARM functional behavior. It must never be used as
evidence for native ARM performance. Native ARM performance requires an actual ARM
runner or host.

## Real developer workload

The production `testing benchmark-developer` journey is the baseline workload. It
must exercise, from identical cloned roots and an explicit closed environment:

- prompt/shell startup;
- Git operations and recursive search;
- a full multi-file compilation and link;
- an edit followed by an incremental build;
- tests;
- archive and package-metadata operations;
- repeated process spawning.

The arm order is balanced. Every row retains durable stdout and semantic artifacts,
and resume/final validation reopens and hashes them. Wall time stops when the child
and output reader finish, before perf parsing or evidence publication. Measurements
hold the exclusive box lock through quiet qualification and all measured arms.

Record wall time, task-clock, instructions, cycles, page faults, backend receipts,
worker/library/fixture hashes, and per-phase wall time. Any phase over 1.2x same-host
native gets a focused hardware-counter profile. A performance change is accepted
only when semantic outputs match and balanced A/B measurements improve the relevant
mechanism without moving cost into another phase.

Baseline captured on x86_64 Linux from the corrected 18-row campaign at
`/var/tmp/devbench13-results-corrected-b6c73b53e`:

- native-supervised / host: 0.995x wall, 1.0004x instructions, 1.007x cycles;
- translated / host: 33.31x wall, 22.61x instructions, 32.89x cycles;
- translated phase ratios: full build 33.13x, incremental build 56.99x,
  process spawn 45.59x, Git 15.85x, search 25.02x, archive 23.93x,
  package metadata 20.34x, and tests 20.91x.

These are a starting point, not a target. Same-ISA native must remain near native.
Translated instruction overhead must be driven down by profiles and exact output
checks; reducing only wall time through host contention or caching is not enough.

## GUI whole-workspace checkpoint journey

The authoritative journey starts the real GTK application under Xvfb using a
scratch HOME and real workspace configuration. Test hooks may drive the GUI, but
the daemon, workers, native engine, checkpoint store, terminals, and extension
containers are production builds.

Before closing the workspace, create observable state that cannot be reproduced by
simply launching a fresh shell:

1. Open at least three tabs.
2. Create both horizontal and vertical splits with stable pane identities.
3. In separate panes, set distinct working directories and environment state.
4. Produce enough terminal output to require scrollback/history restoration.
5. Start a long-running `sleep` and a process tree or pipeline; record guest PIDs and
   progress/state that distinguish continuation from relaunch.
6. Start every normal container belonging to the workspace.
7. Start installed extension containers, place at least one extension surface in a
   pane, and exercise an extension request that produces durable observable state.
8. Record the selected tab, focused pane, pane sizes, split orientations, tab order,
   terminal dimensions, cwd, visible grid, scrollback tail, container inventory,
   extension inventory, and process identities/progress.

Drive the same close action a user invokes. Closing must checkpoint all workspace
containers as one coordinated workspace operation, including extension containers;
it must not hang up processes first, silently relaunch them, or publish a partial
workspace image. Measure click-to-window-closed and checkpoint publication latency.

Reopen the workspace through the real manager UI. Measure click-to-window-visible,
click-to-terminal-usable, and total restore completion. Then prove:

- the original `sleep` and process tree continued rather than restarted;
- every pre-close container and extension container returned exactly once;
- tab order, selected tab, focused pane, split tree, pane identities and geometry
  match the captured state;
- terminal cwd, grid contents, scrollback/history, dimensions, and pending process
  output match and continue;
- extension surfaces return to their prior panes and their container/session state
  continues;
- no duplicate shell, container, extension host, or restored member was launched;
- closing/reopening repeatedly remains correct for at least two checkpoint cycles;
- a forced member failure produces a visible refusal and leaves no partial image.

Screenshots are supporting evidence only. Assertions use the terminal text/grid
artifact, layout/state receipts, process/container inventories, checkpoint manifest
and extension receipts. A passing GUI test count without a display is not evidence.

## Latency acceptance

Always publish measured distributions and machine/artifact identities. Until a
stable product budget is derived from measurements, use these regression rules:

- checkpoint close and restore are measured separately;
- report median and p95 over at least 10 warm cycles plus a cold cycle;
- no accepted change may regress either median by more than 5%;
- UI input must remain responsive during capture and restore;
- a workload-size sweep must show whether latency scales with processes, dirty
  memory, terminal history, panes, containers, and extensions;
- optimize the dominant measured component rather than hiding it behind animation
  or background completion.

After the first reliable end-to-end campaign, replace this provisional section with
explicit user-facing close and reopen budgets justified by the observed floor.

## Translator profiling and acceptance

Profile both x86_64-to-AArch64 and AArch64-to-x86_64, as well as forced same-ISA
translation, with the same production workload. Attribute generated code with
timestamped JIT records across exec/generation reuse. Report generated guest bodies,
branch helpers, decode/build, syscall handling, cache load/build, libc, and unknown
user-space samples separately.

Optimization order is determined by measured avoidable instructions. Every change
requires:

- a mechanism counter that reconciles exactly and does not govern the policy being
  evaluated;
- a mutation proving the test observes the changed path;
- identical semantic/output hashes;
- balanced instructions/cycles/task-clock/wall A/B measurements;
- fork, exec, SMC/authority invalidation, and two checkpoint/restore cycles;
- both supported ISAs when the shared architecture is touched.

## Delivery gate

Before pushing:

1. Merge the current `origin/main` into the integration branch.
2. Re-run focused performance and checkpoint journeys on the merged exact SHA.
3. Run `cargo check --workspace --all-targets`, the required workspace/native test
   commands, feature-gated application Clippy, and native-test-hook arms inside the
   pinned Nix shell.
4. Run the GTK application and GUI suites under Xvfb, including the whole-workspace
   close/reopen journey.
5. Run the x86 and ARM production-binary matrix; state explicitly which performance
   claims require a physical ARM host.
6. Push only the exact gated SHA (`git push origin <sha>:main`).
7. Remove clean merged worktrees and retain benchmark ledgers/hashes needed to audit
   the result.

This plan remains open while any execution-matrix row, GUI state assertion,
cross-ISA profile, latency measurement, exact merged-tree gate, or push is missing.
