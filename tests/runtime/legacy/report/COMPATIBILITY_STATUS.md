# Compatibility status

> **Historical checkpoint:** The counts and Rust-parity denominator in this
> report predate the C-primary cutover. They are not current-tip completion
> evidence. A final C-primary corpus run must supersede this report or explicitly
> account for every remaining failure.

This file preserves the canonical accounting note for that retained-C/Rust
compatibility checkpoint. It
separates inventory, scheduler coverage, historical execution, and results from
the current source fingerprint. Those numbers answer different questions and
must not be substituted for one another.

## Retained C denominator

The retained C engine owns 1,633 manifest cases and 3,101 declared case/ISA
legs. Linux policy selects 3,073 active legs: 1,520 AArch64 and 1,553 x86-64.
That 3,073-leg selection is the required Rust parity denominator. The retained
C engine's latest local ordinary run passed 3,069/3,073 legs; the other four
are the two tmpfs-sensitive cases on both ISAs (`memfd-seals` and
`statx-btime`) run from APFS scratch rather than tmpfs.

## Rust inventory and scheduler coverage

The Rust import contains every retained C row plus 30 Rust-local inventory
rows. Its build inventory has 3,131 rows, of which 3,103 are buildable/pinned
and 28 are global known-bug exclusions. Ten bootstrap legs make the Linux
execution selection 3,113 rows; macOS policy selects 2,996.

All 3,073 active retained-C Linux legs are represented by the current Rust
inventory and have a typed fixture accepted by `tests/inventory.rs`. Therefore
the current supervisor can schedule all of them. This is scheduler coverage,
not evidence that every row starts successfully, finishes, or matches C.

## Historical broad execution

The following reports prove that broad Rust execution occurred, but they were
produced by older source fingerprints and are not current pass percentages:

- `results.tsv` attempted 2,285 legs (1,127 AArch64 and 1,158 x86-64): 8 pass,
  2,277 fail. This predates most current ISA/runtime work.
- `api-results--isa-aarch64.tsv` attempted the 1,127-row AArch64 selection:
  182 pass, 568 fail, and 377 skip. Its old fixture skips and failures have
  since been changed substantially.

These reports remain migration evidence. They must not be merged numerically
with newer reports because their source and runner fingerprints differ.

## Current full-corpus evidence

Commit `867734f7` is the last fully reconciled interpreter checkpoint. A clean detached
release build ran all 3,113 selected Linux rows with 18 workers and unchanged
case deadlines in 563.87 seconds. The saturated first pass produced 3,072 pass
and 41 fail. Every failed key was then rerun with four externally scheduled
workers and `HL_COMPAT_JOBS=1`, giving each guest approximately one host CPU.
Eight load-sensitive rows passed on retry. Exact-key reconciliation therefore
gives:

- 3,080 pass / 33 fail / zero skip = 98.940%;
- AArch64: 1,533 pass / 7 fail;
- x86-64: 1,547 pass / 26 fail;
- retained C: 3,044 pass / 29 fail = 99.056%;
- Rust-local rows: 26 pass / 4 fail;
- bootstrap: 10 pass / zero fail.

The 33 remaining failures comprise 27 reproducible CPU-active timeouts and six
semantic mismatches. The semantic rows are retained-C `mlockall-scope` on both
ISAs and the Rust-local `credential-mutation` and `seccomp-filter` rows on both
ISAs. The remaining suite counts are memory 10, soak 7, core/workload 5,
process 5, core/syscall 2, IPC 2, and signals 2.

The eight recovered rows are AArch64 `core/regress/go-cgo-sigurg`,
`process/reparent-topology`, `signals/restart-interrupted-io`, and
`soak/callgraph`, plus x86-64 `isa/x86_64/go-static-heapgc`,
`memory/anon-tracker-concurrent`, `memory/dbt-longjmp-reenter`, and
`signals/restart-interrupted-io`. This percentage is explicitly a
reconciliation of the full run and exact-key retries, not a claim that one
invocation produced 3,080 passes. The immutable full report SHA-256 is
`f83514d82bced88f2c5dd8dbad213970b086a4d459895f89886e345821692bdc`;
the exact 41-key reconciled retry report SHA-256 is
`2a89e3c9274f50dd1a0548ba156171908c81d440367299efa18dda98a5fde578`.
Both runs left zero workers and zero zombies. Compared with `14b31a2b`, this is
one net additional pass: AArch64 `go-cgo-sigurg` and both `pty-jobsig` legs are
fixed, while both `mlockall-scope` legs are new persistent regressions.

Post-checkpoint commit `f0df68ee` fixes that `mlockall-scope` regression by
restoring the retained C distinction between strict range wiring and
best-effort whole-space wiring. A clean detached run of the two unchanged keys
passes 2/2; `/tmp/mlock-f0df68ee.tsv` has SHA-256
`f98b3d46347952e84a7c730264d9bbfdf78ecf2332cf6bc114dd9710148b2097`.
This focused result does not replace the 3,113-row checkpoint or get added to
its percentage; the next complete-selection run must establish the new global
count.

### Superseding default-native full run

A later complete-selection run from the `d2403776` source lineage enabled the
bounded native x86 adapter by default and attempted all 3,113 rows. Its
saturated first pass produced **3,046 pass and 67 fail**. The immutable report
SHA-256 is
`99e69741977f8e0d76b966befa3654fed79b0f1a90844889b8e68f23a0d19c56`.

Low-load exact-key retries immediately confirmed broad regressions in cases
that passed before default-native activation: `abi/ucontext_swap` produced no
output, `abi/corpus/x_strsearch` and `completeness/aesni` terminated with
SIGSEGV, and CPU-active failures remained in `abi/qsort`,
`abi/corpus/qsort_cb`, and `abi/corpus/x_wcs`. The retry campaign was stopped
once this was sufficient to disprove safe global activation; therefore 3,046
is a first-pass count, not a reconciled current compatibility percentage.

Main commit `8fa6fd46` reverted default native selection. The bounded native
adapter is again opt-in through `HL_NATIVE_EXECUTION=1` until generic
instruction-completeness, fallback, fault, and control-state correctness are
proved. No post-revert 3,113-row run has yet superseded the `867734f7`
interpreter checkpoint.

### Focused opt-in native-x86 evidence

Commit `ac65e26a` was checked against the exact 26 x86-64 keys that remained
failed after the `867734f7` full checkpoint and its low-load retries. The
unchanged cases ran with four external processes, `HL_COMPAT_JOBS=1`, and their
original 120- or 240-second deadlines. Exact-key reconciliation excludes an
extra `utimes-family` row selected by the `times` substring filter. The checked
`X86_NATIVE_AC65E26A.tsv` report contains 17 pass and nine fail rows and has
SHA-256 `2e9952724f3ff6ab9c0be6f408fdf3b8f9ce76e7a5f5397750c1f078cfb6c7f8`.

The recovered x86-64 keys are:

- memory: `dbt-codecache-churn`, `dbt-computed-goto`,
  `dbt-deepwide-recursion`, `dbt-flags-carry`, `dbt-ibtc-mega`,
  `dbt-soak-mix`, `dbt-vtable`, and `mlockall-scope`;
- process: `resource-usage`;
- soak: `bitchurn`, `callgraph`, `manyblocks`, and `vtable`;
- core/syscall: `times`;
- core/workload: `busyloop`, `codecache`, and `indirect`.

The nine remaining x86-64 failures are seven CPU-active timeouts—IPC
`tso-simd-mp` and `tso-unaligned`, memory `anonymous-mapping-reclamation`, soak
`divchurn`, `fpaccum`, and `longjmp`, and core/workload `allocchurn`—plus the
Rust-local `credential-mutation` and `seccomp-filter` launch-persona
mismatches. These results do not establish a global compatibility count; the
subsequent default-native full run demonstrated regressions outside this
26-key cohort. The valid complete-selection checkpoint therefore remains the
interpreter reconciliation of 3,080/3,113 at `867734f7` until a fresh
post-revert full run establishes a newer count on one source fingerprint.

These 17 recoveries are **opt-in performance evidence only**. They must not be
combined with the interpreter checkpoint and must not be presented as a
3,098/3,113 compatibility projection: the subsequent full run proved that
default native execution also regresses previously passing rows. The nine-row
failure list remains useful only for directing native performance work.

After complete x86 `DIV`/`IDIV` lowering landed at `fbcaa290`, the unchanged
x86-64 `soak/divchurn` row passed with explicit opt-in options
`HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`. The inventory run completed in
2.34 seconds; a direct typed-worker confirmation completed in 2.365 seconds
and printed the exact golden checksum. Native counters were 294,108 runs, 79
builds, 294,285 hits, two scheduler fallback sites, and 16 inline services.
The checked `DIVCHURN_NATIVE_FBCAA290.tsv` report has SHA-256
`fcd51b3245af370e2ac3316c723d0edcff8d7a433338d07c12ee1e745ecaad81`.
This remains focused opt-in performance evidence and is not added to the
global compatibility percentage.

After the x86 floating-point environment bridge and legacy scalar-double
lowering landed at `bd7d4e7b`, the unchanged x86-64 `soak/fpaccum` row passed
with explicit opt-in native diagnostics. The inventory run completed in 0.45
seconds; a direct typed-worker confirmation completed in 0.417 seconds and
printed the exact golden checksum. Native counters were 117,183 runs, 79
builds, 117,360 hits, two scheduler fallback sites, and 16 inline services.
The checked `FPACCUM_NATIVE_BD7D4E7B.tsv` report has SHA-256
`ad6553f2e4e43144861266c42b71ae40817e2e5810745181bc16ce83006bcdb4`.
This is focused opt-in performance evidence, not a global compatibility count.

After CET-disabled `RDSSPD`/`RDSSPQ` reads were admitted as architectural
conditional no-ops at `025d3bed`, the unchanged x86-64 `soak/longjmp` row
passed with explicit opt-in native diagnostics. The inventory run completed in
7.82 seconds; a direct process-group-bounded confirmation completed in 6.474
seconds and printed the exact golden checksum. Native counters were 126,006
runs, 113 builds, 47,872,908 hits, two scheduler fallback sites, and 16 inline
services. The checked `LONGJMP_NATIVE_025D3BED.tsv` report has SHA-256
`cddb01d2eb53d872ac0474444e76dbb64891fb37dfb6f423aa3f266e53135db1`.
This is focused opt-in performance evidence, not a global compatibility count.

The focused run completed with approximately 15 GiB of RAM available, 5.8 GiB
free in `/tmp`, 286 GiB free on the workspace filesystem, zero remaining
workers, and zero zombie processes.

The following older checkpoint remains historical evidence and must not be
reported as current:

`api-results.tsv` is the complete current-source Linux inventory run from
2026-08-01. It executed all 3,113 selected rows with 18 compatibility workers
in 285.35 seconds:

- 1,862 pass;
- 1,251 fail;
- zero skip;
- AArch64: 952 pass / 588 fail;
- x86-64: 910 pass / 663 fail.

The ten bootstrap rows all pass. Joining this report to the checked
`C_INVENTORY_GAP.tsv` classification gives the exact remaining split:

- retained C: 1,829 pass / 1,244 fail = 3,073 (59.52% pass);
- Rust-local migration rows: 23 pass / 7 fail = 30;
- bootstrap: 10 pass / 0 fail = 10.

The raw complete-selection pass rate is 59.81%. Of the failures, 262 are row
timeouts, 340 include an exit-status mismatch, and 649 are output-only
mismatches. `api-results.summary.tsv` retains the full per-suite multi-class
counts. The run saturated the host's 18 logical CPUs while remaining bounded:
approximately 19 GiB of RAM and 594 GiB of disk remained available afterward,
and the supervisor left zero zombie processes.

`FULL_CORPUS_001.tsv` is the preceding complete-source checkpoint: 1,721 pass,
1,392 fail, zero skip in 3,256.11 serial seconds. It remains historical evidence
and must not be combined with the current report.

`API_BATCH_002.partial.tsv` is superseded as a coverage measurement. Its
x86-64 `abi/bswap` failure was fixed before the full run; the dedicated
`BSWAP_X86.tsv` row and the full report both pass that case. It remains useful
only as the pre-fix bounded ledger.

Focused reports such as `ATOMIC_ARM_LSE.tsv`, `ATOMIC_X86_FIXED.tsv`, the seven
`LSE_*.tsv` reports, `PASSCRED.tsv`, and `BIGARR_1T.tsv` prove their named
capabilities only. They are deliberately not added to the 18-row general batch
because overlapping rows and different fingerprints would double-count.

## Seccomp oracle boundary

The retained C engine's default launch persona includes the container baseline
filter: `PR_GET_SECCOMP` returns filter mode 2 and `/proc/self/status` reports
`Seccomp: 2` with one filter. The retained `procfs/pf-selfcaps` case checks this
cross-interface identity on both ISAs.

The Rust-local `process/seccomp-filter` row instead requires the same default
launch to begin in disabled mode 0. There is no corresponding retained-C case;
the retained completeness seccomp case installs and enforces a filter without
asserting an unfiltered initial mode. The Rust-local row therefore conflicts
with the C oracle and is not C-parity evidence. It remains unchanged as an
explicit future launch-profile test: it can pass only when the public launch
configuration can request an unconfined persona, rather than by inspecting the
guest, case, suite, or syscall history.

## Required reporting language

- **Corpus size:** 3,073 active retained-C Linux legs.
- **Rust scheduler coverage:** all 3,073 retained-C legs are imported and
  schedulable.
- **Historical attempts:** at least 2,285 broad-run legs were executed by an
  older Rust source state.
- **Latest valid interpreter selection plus exact-key retry reconciliation:** 3,113
  attempted, 3,080 pass, 33 fail, zero skip (98.940%).
- **Exact retained-C selection:** 3,073 attempted, 3,044 pass, 29 fail
  (99.056%).

Never describe imported or schedulable rows as passed. Never describe the
18-row current batch as the only Rust execution that ever occurred. Never
publish a global Rust pass percentage by combining stale and focused reports.
The 98.940% figure may be used only with the explicit cross-run reconciliation
qualification above. The later default-native run is reported separately as
3,046/3,113 first-pass with 67 failures; it is regression evidence, not the
current default configuration and not a replacement reconciled percentage.

## Opt-in native x86 full-selection evidence

Commit `91ff7277` was run from a clean detached worktree with the typed option
`HL_COMPAT_ENGINE_OPTIONS=HL_NATIVE_EXECUTION=1`, x86-64 selection, 18 workers,
release mode, and the pinned retained corpus. All 1,573 selected rows completed
accounting in 401.92 seconds: **1,513 pass, 60 fail, zero skip**. The immutable
report SHA-256 is
`4f0954a12afb9238f1db1cca8f7ec28e4f6384ca6cc62cc6eefcad32cf97da37`.
The byte-identical 1,573-row report is retained as
[`NATIVE_X86_91FF7277.tsv`](NATIVE_X86_91FF7277.tsv). The inventory SHA-256 is
`3a3fbd3ca3acf5abef9f71a196685793e76634d7d34b58265ce7a79c9180f5b3`.

The 60 failures comprise 26 timeouts, 16 signal-11 exits, 16 semantic/output
mismatches, and two worker/resource errors. Exact low-load retries of all 16
signal-11 keys, using four external workers and one inventory worker per key,
reproduced all 16 failures. Their aggregate TSV digest is
`3ec82e9e411656cad1c3a5645347cc7a2182e2f825b30e09a7b614cef54110b5`.
That digest covers the original per-key reports and generated summaries. The
normalized exact 16-key ledger from that retry is retained as
[`NATIVE_X86_SIGSEGV_RETRY_91FF7277.tsv`](NATIVE_X86_SIGSEGV_RETRY_91FF7277.tsv),
SHA-256
`14bf1e9a9d85371da291ffb13ce186c21692122f71ddcfece59ac943478dc5eb`.
Typed-option A/B checks of `abi/corpus/x_strsearch` and `completeness/aesni` at
both `43c9fea5` and `8fa6fd46` also exit with signal 11. Earlier apparent native
passes at those revisions used an ambient option that the runner did not
forward and were interpreter runs, so they are not native evidence.

On 2026-08-03, the same exact 16-key cohort was rerun from clean detached commit
`032aec2f` with typed options
`HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`, unchanged inventory deadlines,
one inventory worker per key, and no more than four external keys at once. The
inventory test was built once and its compiled binary invoked directly for each
selector in a separate process group. Exact key normalization excludes the
adjacent substring matches `sha-kat`, `otmpfile`, and `exec-cloexec-signal`.
**All 16 prior signal-11 keys recovered: 16 pass and zero fail.** No process
group exceeded the 3 GiB safety limit, available RAM remained above 23 GiB
after every batch, and the zombie count remained zero. The normalized 16-row
recovery ledger is retained as
[`NATIVE_X86_SIGSEGV_RECOVERY_032AEC2F.tsv`](NATIVE_X86_SIGSEGV_RECOVERY_032AEC2F.tsv),
SHA-256
`5e5bce64bd1b2bfdfdcb765786f776177b13e8d04f7d17963059f17ada804ed0`.

At current main commit `7b19e7e3`, the exact 16 semantic/output-mismatch
keys from the `91ff7277` full-selection report were rerun with typed options
`HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`, unchanged inventory
deadlines, one inventory worker per selector, and at most four external
process groups concurrently. The inventory test was built once and its test
binary invoked directly. Exact tuple normalization excluded the substring
neighbor `memory/memfd-exec-alias-race`, which timed out but is not a member of
this cohort. The run left zero workers and zero zombies, and available RAM
remained above 22 GiB.

Three keys recovered: `abi/ucontext_swap`,
`core/syscall/utimes-family`, and `filesystem/devfull`. Thirteen remain
persistent: the output mismatch `core/workload/smctableoverflow`; memory
`dbt-smc-crosspage`, `dbt-smc-grow`, `dbt-smc-manyslots`, `dbt-smc-minijit`,
`dbt-smc-rwx-rewrite`, `dbt-smc-trampoline`, and `memfd-exec-alias`; network
`netlink-edges`; POSIX `ifindex`; and process `credential-mutation`,
`proc-self-exe-comm`, and `seccomp-filter`. The normalized exact-key ledger is
[`NATIVE_X86_SEMANTIC_RETRY_7B19E7E3.tsv`](NATIVE_X86_SEMANTIC_RETRY_7B19E7E3.tsv),
SHA-256
`6467191821c97bd0b933c33b81dbecdbb01d79431a464df67d5635da655aebbf`.

At commit `a9a195d3`, the complete x86-64 selection was rerun from a clean
detached worktree after the native crash recovery, scalar byte-memory ALU,
atomic, floating-point, and division work. The release inventory binary was
invoked directly with 16 workers, unchanged row deadlines, and the typed
options `HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`. All 1,573 rows
completed accounting in 393.55 seconds: **1,533 pass, 40 fail, zero skip**
(97.46%). This is 20 net passes above the `91ff7277` full-native checkpoint;
most importantly, none of the 16 former signal-11 failures recurred.

The immutable inputs and outputs are identified by:

- source commit `a9a195d31f299fde3959eede25a562fd0c73a468`;
- inventory SHA-256
  `3a3fbd3ca3acf5abef9f71a196685793e76634d7d34b58265ce7a79c9180f5b3`;
- fixture-schema SHA-256
  `2aed4705cdb5a5cf03b16b456aa8d4c1c774b77a6f6f84ae4b5a527a5e02be22`;
- artifact-manifest SHA-256
  `8956e2b2999a0c6b4da2d8fa1fdc02f5a628d219116e8747db24a4c3037fedc0`;
- release inventory-binary SHA-256
  `4e265a9dd8f07f07d5ff402c69ee856bdc6eebd1cd2294035292ecfa6a8c3bd3`;
  and
- full report SHA-256
  `efbbebc603bd6bba375879c9decc3ebe28331ba279f9ea111d7d703e7b2dd2dc`.

The full ledger is
[`NATIVE_X86_A9A195D3.tsv`](NATIVE_X86_A9A195D3.tsv); its generated mismatch
summary is
[`NATIVE_X86_A9A195D3.summary.tsv`](NATIVE_X86_A9A195D3.summary.tsv), SHA-256
`50fdbeb39eddfb23c8fc5aa621be6b8e959ad30a59ad7f7f60e8b07d7bdfed71`.
The 40 failures comprise 23 CPU-active timeouts, 13 deterministic semantic or
output mismatches, two manually contained resource failures, one separate
output-limit failure, and one worker/cleanup failure. The semantic set is the
same 13-key set listed above. `memory/mprotect-enforcement` reached the capture
limit, while `process/nonpie-dladdr` exited without a worker result or clean
teardown.

`process/forkstorm` and `soak/forkpipe` crossed the 10 GiB per-process-group
safety threshold during the saturated run. Their exact groups were terminated
with TERM followed by KILL; the runner consequently labels their rows
`engine:output-limit`, but they are resource failures for reconciliation.
Observed group RSS was approximately 19.8 GiB for `forkstorm` and 25.0 GiB for
`forkpipe` before containment. The host recovered to 25 GiB available RAM,
240 GiB free workspace storage, zero compatibility workers, and zero zombies.

At current main `62810495954826fa285c1b3292e1a81cccb55a20`, the complete
x86-64 selection was rerun from a clean detached worktree with the unchanged
inventory, fixtures, row deadlines, workloads, and goldens. The release
inventory binary ran 16 workers with typed options
`HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`. All 1,573 rows completed in
369.84 seconds: **1,543 pass, 30 fail, zero skip** (98.09%). This is ten net
passes above `a9a195d3`'s 1,533/1,573 checkpoint, with no pass-to-fail
regressions. The exact recovered keys are `abi/corpus/x_wcs`,
`core/workload/smctableoverflow`, memory `anonymous-mapping-reclamation`,
`dbt-smc-crosspage`, `dbt-smc-manyslots`, `dbt-smc-rwx-rewrite`,
`dbt-smc-trampoline`, POSIX `pthcancel`, process `exit-teardown`, and threads
`cancel`.

An independent five-second process-group monitor enforced a 10 GiB ceiling.
It contained `process/forkstorm` at 16,239,404 KiB and `soak/forkpipe` at
12,589,044 KiB with TERM followed by KILL. Their report rows consequently show
signal-9 exits, but reconciliation classifies both as resource failures. No
other group crossed the ceiling. After completion the host had 24 GiB
available RAM, 27 GiB free swap, and 221 GiB free workspace storage, with zero
compatibility workers and zero zombies.

The immutable inputs and outputs are:

- inventory SHA-256
  `3a3fbd3ca3acf5abef9f71a196685793e76634d7d34b58265ce7a79c9180f5b3`;
- fixture-schema SHA-256
  `2aed4705cdb5a5cf03b16b456aa8d4c1c774b77a6f6f84ae4b5a527a5e02be22`;
- artifact-manifest SHA-256
  `8956e2b2999a0c6b4da2d8fa1fdc02f5a628d219116e8747db24a4c3037fedc0`;
- release inventory-binary SHA-256
  `96139a8af689da3fb510db4e014344169dd8c809b43111695bb4c59d9bfb1538`;
- full report [`NATIVE_X86_62810495.tsv`](NATIVE_X86_62810495.tsv), SHA-256
  `0e431a0d7b67382b36db4d7ca504ac4bad422bd512d2016b9c8b37785bfefdca`;
- mismatch summary
  [`NATIVE_X86_62810495.summary.tsv`](NATIVE_X86_62810495.summary.tsv),
  SHA-256
  `4767a3f01dc2077812e80cfec3cd8339112fc7e531f059f472faff33ed71ac4a`;
  and
- containment ledger
  [`NATIVE_X86_62810495.resources.tsv`](NATIVE_X86_62810495.resources.tsv),
  SHA-256
  `2aff25c83d03fb5cf14a7c6590fe52d27eff07ea4fa278dcfb1b162f05464980`.

The exact build and execution commands were:

```sh
cargo test --offline --locked --release \
  -p hl-engine --test inventory --no-run

HL_COMPAT_ISA=x86_64 HL_COMPAT_JOBS=16 \
HL_COMPAT_ENGINE_OPTIONS='HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1' \
HL_COMPAT_REPORT=/tmp/NATIVE_X86_62810495.tsv \
target/release/deps/inventory-d81ee358e63a049b \
  inventory_matrix --ignored --nocapture
```

A subsequent low-load retry used one inventory worker per selector and no more
than four external process groups. Twelve quick semantic keys plus
`mprotect-enforcement` and `nonpie-dladdr` all reproduced their full-run
failure classes. The normalized 14-row ledger is
[`NATIVE_X86_QUICK_RETRY_A9A195D3.tsv`](NATIVE_X86_QUICK_RETRY_A9A195D3.tsv),
SHA-256
`9ca205d661f5985ff6b4d43179cebd2488bde146549ad824a42a5e70a819c929`.
`memfd-exec-alias` was not repeated because its substring selector also admits
the 120-second `memfd-exec-alias-race` row; its current-tree full failure and
the preceding exact low-load semantic retry remain consistent. The 23 timeout
rows were not low-load-retried because their unchanged 120- or 240-second
deadlines cannot complete as a cohort inside the bounded corpus lane.

A mechanical tuple join against `NATIVE_X86_91FF7277.tsv` explains why this
full result is 1,533 rather than the earlier 1,535 focused projection. All 22
focused recoveries still pass: the 16-key crash cohort, `divchurn`, `fpaccum`,
`longjmp`, `ucontext_swap`, `utimes-family`, and `devfull`. One additional old
failure, `memory/dbt-longjmp-reenter`, also recovered. Three `91ff7277` passes
failed in the saturated run: `isa/x86_64/go-static-goro`,
`memory/mprotect-enforcement`, and `process/forkstorm`. Therefore the exact
arithmetic is 1,513 + 22 focused recoveries + one additional recovery - three
regressions = 1,533.

Only those three discrepancies were then rerun at low load from the same clean
commit and release binary, with one inventory worker per selector and unchanged
deadlines. `go-static-goro` passed in 37.79 seconds, proving its full-run timeout
was saturation-sensitive. `mprotect-enforcement` reproduced its output-limit
failure in 0.09 seconds. `forkstorm` again exceeded the 10 GiB group limit,
reaching approximately 24.5 GiB observed RSS before its exact process group was
contained; its ledger row again says `engine:output-limit`. The exact ledger is
[`NATIVE_X86_DISCREPANCY_RETRY_A9A195D3.tsv`](NATIVE_X86_DISCREPANCY_RETRY_A9A195D3.tsv),
SHA-256
`6906c3d2d4f1f7909bb30f002a788bd17b5c4ab05037dba0e41f22611ec67f54`.
This gives a low-load reconciliation of **1,534/1,573**, while the single-tree
saturated full-run result remains the authoritative **1,533/1,573** checkpoint.

A bounded diagnostic experiment reduced the x86 native slice budget from
4,096 instructions to one. `x_strsearch` then changed from a quick signal-11
exit into a CPU-active worker that did not terminate after four minutes. The
interrupted parent test left the supervised worker orphaned; its exact process
group was terminated and the host returned to zero zombies. This proves that
the crash manifestation depends on native boundary frequency, but a one-step
native policy is neither correct nor a viable fallback. The remaining blocker
is locating the first corrupt x86 boundary state: current diagnostics report
only the eventual guest signal and do not retain the preceding native
branch/syscall CPU snapshot.

During the saturated full pass, `soak/forkpipe` grew to approximately 18 GiB
RSS and was terminated by its exact process group before host exhaustion. It
was not retried. Native execution remains strictly opt-in: deterministic guest
state failures and unbounded fork resource growth make default activation
unsafe.

### Native x86 allocator endurance

At `17060303`, an exact low-load retry of `core/workload/allocchurn` with the
typed options `HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`, one inventory
worker, and the unchanged 120-second deadline remained a CPU-active timeout:
`wall_ms=120002`, `tree_ticks=11956`, host busy 15%, four runnable tasks on 18
CPUs. The worker occupied approximately one complete CPU and 96 MiB RSS, so
this is execution throughput rather than memory pressure or orchestration
stall. The byte-identical row is retained as
[`ALLOCCHURN_NATIVE_17060303.tsv`](ALLOCCHURN_NATIVE_17060303.tsv), SHA-256
`8f5ce0c2251035672f58e38a27ab8a94ee0bac619c7c3808edfdee96c8fe4597`.

A structurally identical ten-iteration probe completed and reported
`runs=2 builds=66 hits=126 fallbacks=2 sites=2 services=29`; executor detail was
`branch=39 fallback=87 operand_callbacks=16 operand_cache_hits=69`, with zero
completed blocks. The production case performs six million allocator cycles.
The native frontend suppresses both fallback entry sites after observation,
leaving the allocator-heavy body on the interpreter. Static comparison also
shows that the Rust native frontend rejects the `LOCK`-prefixed `cmpxchg`
family and has no `xchg` lowering, while retained C routes `0F B0/B1` through
its x86 translation/interpreter closure. Glibc's allocator contains both
implicitly atomic memory `xchg` and `lock cmpxchg`; this atomic read-modify-write
closure is the highest-confidence coherent missing hot family. It must be
ported with exact width, flags, atomicity, fault, and memory-boundary semantics;
special-casing `malloc` or this loop would be invalid.

At `ac63f620`, a fresh profile of a temporary 1,000-iteration copy, compiled
with the retained row's exact `x86_64-linux-gnu-gcc -static-pie -O2 -pthread
-lm` shape, completed in approximately one wall second with the expected
checksum `252697`. Typed diagnostics reported `runs=14 builds=347 hits=6869
fallbacks=3 sites=3 services=87` and executor detail `branch=350 fallback=6511
operand_callbacks=4216 operand_cache_hits=2274`; there were still zero
completed native blocks. A 100-iteration control retained the old startup-only
shape (`runs=2 builds=66 hits=126 fallbacks=2 sites=2 services=80`,
`branch=39 fallback=87 operand_callbacks=16 operand_cache_hits=69`). Thus the
new atomic lowering is admitted, but the allocator body still repeatedly ends
native traces at unsupported instructions and pays substantial projection work.

Temporary diagnostic-only site logging, removed immediately after the run,
located the three fallback PCs in the PIE image (load bias `0x1000000`):

- `0x10225b5`, `_dl_relocate_static_pie+0x445`: `66 0f 73 d9 08`, SSE2
  `psrldq xmm1,8` (`0F 73 /3 ib` packed shift-immediate family);
- `0x1025310`, `getenv+0x50`: `41 38 45 00`, byte `cmp [r13],al` (`38 /r`
  memory-destination byte ALU/compare family); and
- `0x1010881`, `tcache_free_init+0x81`: `66 0f c4 0d ... 00`, SSE2
  `pinsrw xmm1,[rip+...],0` (`0F C4 /r ib` insert-word family).

`CMPXCHG` and `XCHG` are therefore genuinely admitted rather than merely
present in the interpreter. The native frontend accepts naturally aligned
memory `0F B0/B1` with optional `LOCK` and memory `86/87` when AArch64 LSE is
available, and emits `CASAL`/`SWPAL` width variants with bounds, permissions,
fault provenance, flags or exchanged-register state, and post-success dirty/SMC
publication in `frontend.c:decode_block` and `frontend/memory.c`'s
`hl_x86_emit_cmpxchg`/`hl_x86_emit_xchg`. Their focused frontend tests pass.
The retained engine's complete closure is broader: `translate.c` handles
register and memory XCHG plus `0F B0/B1`; `interp.c:interp_locked_rmw` also
handles unaligned split locks and locked ADD/ADC/SUB/SBB/AND/OR/XOR/BTC/BTS/BTR,
XADD, and CMPXCHG; `cmpxchg.c` supplies hashed-lock 128-bit CMPXCHG16B.
Rust intentionally falls back for unaligned memory atomics and still lacks that
locked arithmetic/XADD/wide-CAS closure.

The next coherent throughput work is ranked by this measurement:

1. Complete byte memory ALU/compare lowering (at least the symmetric `38/3A`
   forms together with the already modeled flag semantics). It is the first
   ordinary scalar blocker on a repeatedly entered libc path and avoids
   suppressing that native entry site.
2. Port the complete SSE2 packed-integer shift/unpack/shuffle/insert closure
   containing `0F 71-73`, `0F 60-62`, `0F 70`, and `0F C4`, including register
   and memory operands and exact lane/immediate behavior. Adding only the two
   observed opcodes would strand adjacent glibc vector sequences at the next
   instruction.
3. Port the complete locked scalar RMW closure, especially `0F C0/C1` XADD and
   lock-prefixed arithmetic/bit operations, with a bounded unaligned fallback
   equivalent to the retained hashed-lock path. This matters for allocator and
   thread contention but is not the first blocker in this single-thread probe.
4. After traces survive these decoder gaps, reduce the 4,216 resolver callbacks
   through trace-level authenticated range certificates; optimizing callbacks
   before restoring trace continuity cannot address the 6,511 native fallback
   exits.

No family was implemented in this profiling lane: each observed vector site is
one member of a larger stateful SSE2 family, and the retained atomic closure's
split-lock and wide-CAS semantics exceed a safely bounded change. The temporary
source, binary, captures, and instrumented engine build were removed; retained
fixtures and deadlines were unchanged.

At `ac63f620`, complete native memory `XCHG` and `CMPXCHG` lowering was present
for byte, word, dword, and qword operands, with fail-closed host-LSE gating,
unaligned pre-mutation fallback, exact flags and partial-register semantics,
and success-only dirty/executable publication. The unchanged typed
`core/workload/allocchurn` row nevertheless remained a CPU-active timeout at
its original 120-second deadline: `wall_ms=120004`, `tree_ticks=12019`, host
busy 20%, two runnable tasks on 18 CPUs. The retained row is
[`ALLOCCHURN_NATIVE_AC63F620.tsv`](ALLOCCHURN_NATIVE_AC63F620.tsv), SHA-256
`de31edab87ab025e63e54a632e9a4644171a9829277ffa03898cd41275922aac`. This proves
that atomic admission was a real missing native domain but is not the dominant
remaining end-to-end allocator bottleneck; a reduced-workload native profile
must identify the next complete hot family before another opcode patch.

At `9de5e37785899a8395cf1f4fd27177e2dcd7bdc5`, after the generic x86 REP
optimization merged through `c2d6e4bd732d125ea77bded0733e1428bd5aaedc`, the
unchanged `core/workload/allocchurn` case still failed at the unchanged
120-second deadline. It used
`HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`, `--jobs 1`, and reported a
CPU-active timeout: `wall_ms=120004`,
`tree_ticks=12006`, host busy 6%, two runnable tasks on 18 CPUs. Therefore the
REP speedup did not resolve allocator endurance; this remains a compatibility
and throughput blocker rather than a passed case. The immutable normalized row
is [`ALLOCCHURN_NATIVE_9DE5E377.tsv`](ALLOCCHURN_NATIVE_9DE5E377.tsv), SHA-256
`840411fee1cd8567fd3b0952b8e1837cb44ef2b4e6a014d91bcd022a0a1f4b30`.
