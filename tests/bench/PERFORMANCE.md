# Engine performance checkpoint

## 2026-08-04: exact-head float-SIMD attribution

Clean detached commit `fbfe10df8dc21b1c4827580a624da54c67416585`
was built warning-strict in release mode and measured seven times on CPU 17 with
the same ARM64 guest (`--divisor 1000 --phase float_simd`). The guest SHA-256 was
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`;
the retained-C runner was
`0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`,
the Rust engine was
`8c7dd39e3cf3e776f6a22edfeb535b7a39e594f434007ed28b988de6493e8497`,
and the matrix runner was
`7e6a7f251a7358c6fa665e2353e63cac58499bcecc8397fea6d26aca4f8c2f2f`.
Host load at admission was 7.74/5.69/4.64 with 9.2 GiB available memory and
23.9 GiB free swap.

| provider | median us | range us | versus native | checksum |
|---|---:|---:|---:|---:|
| host native | 124 | 123--129 | 1.000x | 2,302,728,000 |
| retained C | 123 | 122--129 | 0.992x | 2,302,728,000 |
| Rust native-verified | 282,375 | 279,009--286,296 | 2,277.218x | 2,302,728,000 |

The retained oracle audit covered
`../engine/src/translator/guest/aarch64/translate.c` (`gpr_field_mask`,
`uses_x18`, ordinary same-ISA emission, optional I8MM/BF16 policy, memory
folding, and vector-dirty classification) and `stubs.c` (`emit_prologue`, full
and GPR-only spill, runtime vector currency, and block-return ownership).
Baseline vector-only arithmetic is emitted verbatim; cross-file GPR forms are
rewritten around stolen registers; optional host features are probed or lowered.
The CPU record owns all GPR/vector/FP state for the engine lifetime, translated
blocks borrow it synchronously, full exits republish it, and asynchronous faults
reconstruct pre-operation architectural state.

The corresponding Rust owners are `simd_float.c` and the other typed SIMD
families for opcode admission, `trace.c` for chained translation and provenance,
`single.c`/`pair.c` for transactional memory guards, and `stub.c`/`fault.c` for
CPU publication and fault reconstruction. Every Rust repeat reported identical
diagnostics: 12 runs, 21 builds, 59 hits, 12 fallbacks, 27 fallback exits, 9,133
completed instructions, **3,356 full guards, zero fast guards**, and 17 guard
fallbacks. Thus current float-SIMD time is no longer evidence that FMLA itself is
interpreted; it is dominated by guarded accesses to the benchmark's static
arrays. The previously rejected broad view/projection certificate cannot be
revived: it did not prove per-access mapping identity, permissions, invalidation,
or fault ordering. No production shortcut is justified until a bounded authority
mechanism preserves those contracts for every guarded address.

## 2026-08-04: current ARM64 compute boundary audit

Exact detached Husklet commit `fbfe10df8dc21b1c4827580a624da54c67416585`
was built in release mode and measured five times on inherited CPU 17.  The
same content-bound guest ran host-native, through the retained C engine, and
through Rust with native execution and diagnostics required.  The command was:

```sh
taskset -c 17 target/release/testing benchmark matrix \
  --arch arm64 \
  --binary target/testing/a64-ab/combined_bench_aarch64 \
  --c-engine /Users/x/dd/engine/build/unit-audit/bin/hl-engine-runner \
  --rust-engine target/release/hl-engine \
  --out target/testing/compute-before --repeats 5 --timeout 60 \
  -- --divisor 1000 --phase compute
```

All three rows produced checksum `9349119015121845085`.  Host-native was
46 us median (44--53), retained C was 46 us (46--47), and Rust was 2,111 us
(2,059--2,591): Rust remains 45.891 times host-native on this fixed work.
Every Rust repeat was `native-verified` and reported the same counters:
seven native runs, nine builds, 24 hits, two fallbacks, seven branch exits,
five yields, and 337,288 completed guest instructions.  The engine, runner,
guest, and retained engine SHA-256 values were respectively
`f96f044a4189dff40fa3d03c40f35511ba791427da8514ae508c6bbd826d1727`,
`33bdc527d552d1ee009c1be2a1c925301ad896cf075c3eb7ada48e563c62d930`,
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`,
and `0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`.
Host load at admission was 5.93/5.48/4.61, so this is focused causal evidence,
not a release performance claim.

The retained-domain audit covered `../engine/src/core/dispatch.c`
(`run_block`, `block_return`, and `run_guest`) and
`../engine/src/translator/guest/aarch64/stubs.c` (`emit_prologue`,
`emit_spill`, and `emit_chain_exit_from`) together with
`translate.c` (`stitch_cond`, `translate_block`, the IRQ-slim entry anchors,
and conditional stitching).  Retained C keeps architectural registers live
across direct edges, stitches bounded conditional fall-through into one
superblock, and polls interruption at backward/indirect cycle edges.  Slow
exits spill full state before returning to the dispatcher; invalidation and
publication occur under the JIT lock, and signals are selected after a fully
published boundary.  The AArch64 host branch is the relevant same-ISA path;
other hosts do not execute this transliterator.

The Rust owners are `src/native/exec/src/arch/aarch64/trace.c`
(`trace_build` and `hl_a64_conditional_chain`), `stub.c`
(`hl_a64_stub_budget_begin` and `hl_a64_stub_exit`), `entry.S`, cache
`relocation.c`, and `src/native/exec/src/executor.c::run_aarch64`, with the
safe call/lifetime owner in `src/containers/hl-engine/src/native/executor.rs`.
Rust relocation correctly targets `body_offset`, skipping the full register
reload on a chained edge.  It nevertheless terminates every conditional basic
block.  Each destination body republishes its executing-cache identity, polls
both interrupt sources, loads and checks budget, and loads/stores both budget
and completed count.  The benchmark loop has two hot conditional block edges,
so this fixed admission is paid repeatedly even though only seven public
native calls occur.

That makes bounded conditional trace stitching, not another opcode fast path,
the largest generic compute gap.  The dormant loop/certificate structures are
not authority to bypass these checks: the rejected run-scoped view certificate
does not safely cover mapping-owner transitions, and simply chaining past the
entry guard would weaken interrupt and checkpoint latency.  No production
optimization was made in this lane.  A valid implementation must port the
retained bounded conditional-stitch domain as a unit, preserve a polling edge
on every cycle, retain exact instruction accounting, and invalidate every
stitched edge with its source epochs.  Acceptance needs branch-shape tests
(taken, fall-through, nested conditions, backward cycles), mid-cycle interrupt
and budget exhaustion, mapping/instruction epoch invalidation, fork/cache
retirement, and same-base pinned before/after evidence.

## 2026-08-04: retained C versus Rust native execution

This checkpoint measures the folder-owned `combined/main.c`, which is byte-for-byte
identical to `../engine/tests/perf/combined_bench.c`. The Rust binaries were built
from clean detached Husklet commit
`2b731a4337ddd7ad1d4d96b2a7ec4b124508e3e3`. The retained C source paths used by
the benchmark were clean at commit
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`; the executed C binaries are
content-bound below because their build directory is not a clean checkout.

| artifact | SHA-256 |
|---|---|
| Rust `hl-engine` | `ea0cd2d28f1e4a1eae2ee06b064ca7ed4760794b051be16e2535dca1e9007fe6` |
| Rust `testing` | `5dd399743d9513c33685872163e273778971330d4309298403684265388a4980` |
| ARM64 guest | `b39066b2ae7468d8a57359f963515729f92cbfd7bc53fb7af68980909e3be08f` |
| AMD64 guest | `d3be5c156c27b3069a0a90d3788d4eef6a77e1cf67a06ea55d79f9a64953755c` |
| C ARM64 engine | `9c2701b36a46050909b12498eb0b47673f301bcb35d57e552d343b029cf3a67a` |
| C AMD64 engine | `d25cc62b03b9baf68f7edea8ec8f87146ca5f57ffc380fa30555020dbedf30a2` |

### Method

- Linux ARM64 host, performance governor, one cell at a time pinned to CPU 17.
- Five samples per cell, `--divisor 20`, 120-second per-sample timeout, and a
  1 MiB output bound. With five samples, the observed maximum is nearest-rank
  p90.
- Rust used `HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1`; every measured Rust
  cell reported `native-verified` with nonzero native diagnostics.
- Host start: 18 logical CPUs, 15 GiB available RAM, 9.9 GiB swap used, 192 GiB
  disk free, and 7.7 GiB `/tmp` free. Concurrent repository builds later raised
  load average to 26 and swap use to 14 GiB. In-guest phase time is therefore
  the comparison metric; wall time is retained only as diagnostic evidence.

Times are microseconds. Ratios are Rust divided by retained C.

| ISA | phase | C median | C p90 | Rust median | Rust p90 | median ratio | p90 ratio |
|---|---:|---:|---:|---:|---:|---:|---:|
| ARM64 | compute | 3,596 | 6,539 | 16,052 | 20,147 | 4.464x | 3.081x |
| ARM64 | float SIMD | 10,540 | 11,748 | 15,526,078 | 19,704,347 | **1,473.062x** | **1,677.251x** |
| ARM64 | memory | 7,044 | 7,766 | 2,203,672 | 2,824,412 | 312.844x | 363.689x |
| ARM64 | syscall | 26,495 | 27,288 | 238,371 | 270,094 | 8.997x | 9.898x |
| ARM64 | signal | 23,969 | 25,187 | 3,166,848 | 3,340,777 | 132.123x | 132.639x |
| ARM64 | pipe | 72,562 | 73,899 | 291,401 | 307,204 | 4.016x | 4.157x |
| AMD64 | compute | 5,003 | 5,158 | 972,729 | 994,547 | 194.429x | 192.816x |
| AMD64 | float SIMD | 18,712 | 19,530 | 20,095,445 | 21,035,868 | 1,073.934x | 1,077.105x |
| AMD64 | memory | 6,531 | 7,477 | 303,537 | 305,902 | 46.476x | 40.912x |
| AMD64 | syscall | 26,004 | 30,696 | 490,431 | 520,288 | 18.860x | 16.950x |
| AMD64 | signal | 15,261 | 16,235 | 2,701,050 | 3,324,126 | 176.990x | 204.751x |
| AMD64 | pipe | 78,726 | 79,765 | 1,010,559 | 1,060,462 | 12.836x | 13.295x |

Checksums matched for every comparable row. The syscall checksum contains the
guest TID and is intentionally not compared. The file phase is excluded from
performance ratios because it exposed a correctness failure: Rust returned zero
successful operations while C returned 20,000. The complete ARM64 C sample ran
all phases, while the complete Rust sample reached the fixed 120-second bound;
the table uses individually selected phases with identical internal warmups.

The largest measured hot-path gap is ARM64 float SIMD. Root-cause work must first
identify its generic translation or fallback mechanism; this checkpoint does not
justify an application-specific fast path.

## Candidate evidence after the checkpoint

### Syscall and descriptor hot-path audit

The retained implementation audit for the syscall/descriptor lane covered
`../engine/src/linux_abi/syscall/dispatch.c` (`service`, `service_local`, and
`svc_done`), `syscall/proc.c` (`svc_proc`, including canonical `getpid` and
`gettid` identity), `syscall/io.c` (`svc_io`, scalar/vector I/O, descriptor
duplication, close, seek, pipe, splice, and synchronization), and
`syscall/helpers.c` (descriptor/OFD identity, close teardown, locking, and
restart/error completion). The dispatch boundary owns signal-entry ordering,
seccomp-before-service, ptrace routing, original-register preservation, errno
projection, restart, and descriptor publication. Descriptor aliases share OFD
state; descriptor-local flags remain per descriptor; close tears down emulated
state only after alias and lock bookkeeping; blocking and partial-result paths
retain Linux `EINTR` and progress semantics. AArch64 uses canonical syscall
numbers and advances past `svc`; x86 translates its historical number table and
must preserve the number in `rax` across restart.

The Rust mapping is `hl-linux/src/syscall/{table,frame,mod,ports}.rs`
for ABI admission, `hl-runtime/src/syscall_router.rs` for seccomp/ptrace/signal
ordering and family dispatch, `hl-runtime/src/process/{dispatch,syscalls}.rs`
for task identity and lifecycle, `hl-runtime/src/filesystem/` for descriptor
syscall composition, and `hl-descriptor` for table, descriptor, OFD, operation
lease, alias, and teardown ownership. Engine construction in
`hl-engine/src/ffi/linux/execution/routing/mod.rs` creates one router per Linux
thread. The measured identity loop exposed an avoidable generic divergence:
after seccomp admission Rust dynamically redispatched immutable `getpid` and
`gettid` through the mutable process port, whereas retained C reads identity
directly from stable process/CPU state. The candidate publishes the typed
`ProcessId`/`ThreadId` into the per-thread router and answers only those two
operations there. Seccomp, tracing, result publication, restart, and signal
boundaries remain on the common path; all mutable task operations continue
through the process port.

On exact base `0f3927e8b`, seven release-mode ARM64 native-verified repeats of
the combined benchmark's syscall phase (`--divisor 20 --phase syscall`) had a
241,851 us median and 239,284--249,135 us range. The candidate, built in the
same isolated worktree and measured with the same guest and command, had a
209,289 us median and 208,173--212,419 us range: 13.46% lower median time.
Every repeat on both trees reported 24,999 syscall exits, two fallbacks, zero
yields, and checksum/count 1,000,000. The candidate engine SHA-256 was
`850079ad9d29ffb89cb58732f4576c9522b929a886febdbbbc1fb18dbaeda94d`;
the testing runner was
`0a338fde24f3e88453a5cf0eab075adca4fa8bd8957c435016d5ee373b564ac0`
and the ARM64 guest was
`b5f1df4463a41c926a7ce2819edd0e0363a0fd9b69cb84182607770e06d5ef6e`.

The results below come from the shared dirty tree and are diagnostic candidate
evidence, not evidence for `HEAD` or a stable revision.

### ARM64 baseline SIMD admission

Disassembly of `phase_float_simd` identified vector MOV/ORR and FMLA in the hot
loop and vector SCVTF during setup. All three instruction families terminated
the native trace. The scheduler then suppressed the failed entry and repeatedly
executed the loop through lane-by-lane SoftFloat. Current diagnostics confirmed
13 native fallbacks across 13 builds before the candidate.

The candidate admits the complete baseline AdvSIMD logical family and the S/D
vector FMLA/FMLS and SCVTF/UCVTF families. Optional FP16, BF16, and I8MM remain
excluded pending explicit host/guest feature policy. A warning-strict trace test
covering AND, MOV, FMLA 4S, FMLS 2D, SCVTF 4S, UCVTF 2D, and SVC failed before
the change and passes afterward.

One shared dirty-tree `--divisor 20 --phase float_simd` row completed
`native-verified` with the same checksum at 14,251,204 us, 8.2% below the clean
checkpoint median. That tree contained unrelated companion native changes, so
the delta cannot be attributed to SIMD admission. An exact clean detached run
of accepted combined commit `31cefa656` took 19,484,700 us under higher host
load, also with the same checksum and native proof. No end-to-end speed claim is
accepted from these samples. Vector memory guards and remaining fallback
boundaries are still the next measured owners.

### AMD64 REP bulk accounting

The retained domain audit covered `lower/repstr.c` and `rep_runtime.c` for MOVS
and STOS decoding, largest pinned-span selection, direct and indirect mappings,
forward-overlap smear, backward DF order, partial faults, register publication,
store observation, and pin release on every path. It also covered
`guest_memory.c` for pin lifetime and `dispatch.c` for helper return ordering.
The current Rust-owned comparison covered x86 `run.c`, `projection.c`, the Rust
projection lease, and scheduler slice selection. The lease holds mapping
identity for the synchronous run; native code owns only borrowed bounded views,
and post-success dirty publication precedes any executable-write epoch exit.

The checkpoint's AMD64 memory row was 46.476 times the retained-C median.
Diagnostics isolated a deterministic amplification: one memory iteration made
1,537 public exits and four made 2,305, exactly 256 additional exits for each
additional 1 MiB copy. Projection already supplies a bounded contiguous 1 MiB
view, but native REP charged every copied byte against the 4,096-instruction
scheduler budget.

A candidate charged one authenticated, at-most-1-MiB REP chunk as one guest
instruction. Its five-repeat native-verified row had checksum 36,526 throughout
and a 24,599-us median, 12.34 times faster than the checkpoint Rust median.
Independent review rejected and reverted it: the established interpreter and
native contract charges every REP element against the instruction budget. The
experiment let budget one copy up to 1 MiB, changed `executed` semantics, and
weakened interrupt/checkpoint latency by up to 1,048,576 times. Its timing is
useful causal evidence for the exit amplification, not an acceptable product
result.

The accepted follow-up must either preserve per-element accounting while
eliminating public exits, or explicitly redesign the common interpreter/native
string-budget contract. It requires differential coverage for a greater-than-
1-MiB partial chunk and resume, the next-view fault, interrupt/checkpoint
latency, DF and overlap order, exact RCX/RSI/RDI, dirty publication, and
executable-write epoch behavior before another benchmark.

The accepted bounded correction preserves that contract and removes an
unrelated scheduler asymmetry. The scheduler already grants its sole runnable
native task 65,536 element/instruction credits, but the x86 path discarded that
selected budget and hardcoded 4,096. It now receives the same selected budget
as AArch64. Shared-runnable execution remains at 4,096, while sole-runnable REP
falls from 256 to 16 yield exits per MiB. Sixteen exits are intentional: making
the complete MiB one accounting unit would again weaken the established
checkpoint and scheduling bound. A greater-than-1-MiB native regression checks
every 65,536-element partial state and resume, exact RCX/RSI/RDI, the final
syscall boundary, and byte equality. Existing focused string tests retain DF,
overlap, fault, dirty-range, executable-write, zero-count, and address-size
coverage.

Exact clean detached commit `5b0d27e85` confirms the bounded correction. Five
AMD64 memory repeats were all `native-verified`, held checksum 36,526, and
reported a 119,295-us median with a 118,386--138,524-us range. This is 2.54
times faster than the 303,537-us checkpoint Rust median, while remaining 18.27
times slower than the retained-C median of 6,531 us. Native diagnostics were
identical across all five repeats (`x86_public_exits=7131`, 238 builds, 397,962
hits, and five fallbacks). The exact `hl-engine` SHA-256 was
`f363cc376f433def6740fd941d6c292bc85e70b4f9b2e65bffac9a82f9ffe2ff`; the
exact `testing` SHA-256 was
`071120b66176b44e0f456a2df4207ad66c000ef8a5f417d82c3c39d98c013824`.

### AMD64 scalar SSE2 audit

The compute loop's hot instruction sequence is MULSD, integer XOR/IMUL/ROL,
ADDSD, COMISD, conditional SUBSD, and CVTTSD2SI. Current native lowering admits
only scalar square root, addition, multiplication, and division. Retained C
owns the coherent scalar SSE/SSE2 arithmetic, comparison, conversion, MXCSR,
NaN, EFLAGS, and indefinite-result semantics. The missing family, rather than
one benchmark opcode, was assigned to a separate implementation lane.

That candidate implements SUBSD, COMISD/UCOMISD, and 32/64-bit CVTTSD2SI for
register and guarded-memory forms, including ordered/unordered flags, signaling
versus quiet NaNs, integer-indefinite overflow results, and preservation of the
upper XMM lane. Its warning-strict focused structural and differential test
passes. Five native-verified compute repeats produced a 616,183-us median and
539,149--663,124-us range with checksum 7,097,455,804,780,747,230 throughout.
That is 1.58 times faster than the checkpoint Rust median but still 123 times
the retained-C median. Structural tests prove the scalar family removes those
fallbacks, while the timing only suggests their cost; packed and scalar-single
SSE remain the larger gap. This is shared dirty-tree evidence. The observed post-build `HEAD` was
`2114daca34f3790f0ce2c887dcaf3ad6551567eb`, and the candidate engine SHA-256
was `a9a544aadfb9b7ec69c8c4c59036f24eed6fb60d60d8240396b37e7e8b2dbf92`.

The exact clean detached `31cefa656` verification passed both warning-strict
native tests. Its five-repeat compute median was 846,456 us with a wide
655,170--1,420,562-us range and the same checksum. That is only 1.15 times below
the checkpoint Rust median and remains about 169 times retained C; the spread
is too large for an accepted timing claim. The exact engine SHA-256 was
`1baf1ab38e73bfe91c4514b2793d963671f18b368d805efb9f30456641c6c3e8` and
the exact testing binary was
`91e12bbb0dc3e4488d69d35dab5656696a2662b32cf14d83c94793f09a30b6c6`.

### Benchmark rootfs startup audit

The retained benchmark oracle was inspected at
`../engine/tools/matrix_runner.c`: `stage_rootfs`, `open_case_workspace`,
`run_case`, and `remove_rootfs`. It creates one private root per case, stages
the guest and optional loader/libc once, runs it, then removes exactly those
owned paths. It has no image-store snapshot, cross-process lease, or durable
publication contract comparable to `hl-images`.

The Rust path was inspected from `TestImage::materialize` through
`Images::rootfs`, `Roots::fork`, `Snapshots::prepare`, `Tree::copy_to`, and
`Draft::commit_with`. The image-store operation lock spans a full root fork.
The fork recursively traverses the immutable parent, temporarily makes each
directory traversable and each file readable, reflinks or copies regular
files, preserves hard-link identity, forks ownership/name sidecars, recursively
syncs the new tree, atomically renames it, syncs committed directories, and
publishes the snapshot before creating its lease. Release owns lease deletion
and private snapshot removal. These durability and isolation transitions must
not be weakened for benchmark speed.

Before this correction, provenance preparation called `materialize` only to
read the immutable manifest digest, then released that complete private root;
execution immediately repeated the same durable fork. One uncontended ARM64
syscall row at base `fbfe10df8` measured that discarded provenance root at
284,747 us plus 28,498 us release, while the execution root cost 122,545 us
plus 45,515 us release. The correction resolves and platform-validates the
same image metadata for provenance without creating a writable root. Execution
still performs exactly one isolated durable fork and release.
