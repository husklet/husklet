# Retained core ABI oracle audit

> **Historical ownership:** Rows naming `hl-execution` describe the deleted
> Rust replacement engine. Production is `src/runtime/native/retained`.

This flat category ports all 35 cases from
`../engine/tests/compat/core/abi/manifest.tsv`. The retained tree was used
read-only; no retained file was edited. The fixtures remain independent even
where a transformed or differently scoped test with a similar name exists in
another Husklet category.

## Retained implementation studied

The audit followed the complete execution boundary beneath these probes:

- `../engine/src/core/dispatch.c`: `run_guest`, the AArch64 `run_block` and
  `block_return` entry trampolines, block lookup/translation/publication, return
  reason handling, signal-boundary polling, registry join/leave, and teardown;
- `../engine/src/core/target/aarch64.c` and
  `../engine/src/core/target/x86_64.c`: target construction, CPU initialization,
  ELF entry, architecture dispatch composition, and process exit;
- `../engine/src/translator/cache.c` and `arena.c`: `map_host`, `map_put`,
  `jit_hostpc_lookup`, W^X arena allocation/publication, invalidation,
  stop-the-world cache rotation, fork repair, and final unmapping;
- `../engine/src/translator/guest/aarch64/{abi.h,cpu.h,dispatch.h,translate.c,signal.c,stubs.c}`:
  generated CPU layout, block lowering, helper-call ABI, indirect dispatch,
  architectural flags/FP state, signal reconstruction, and syscall PC advance;
- `../engine/src/translator/guest/x86_64/{abi.h,cpu.h,dispatch.h,decode.c,operand.c,flags.c,rep.c,x87state.c,signal.c,translate.c}`:
  variable-length decode, integer/FP/x87 state, `moffs`, BT/SHLD/SHRD,
  REP partial progress, indirect dispatch, signal reconstruction, and syscall
  return state;
- `../engine/src/linux_abi/{elf.c,signal.c,thread.c,fork.c}`: ELF mappings and
  initial state, signal queue/frame/return, task registration, fork inheritance,
  cancellation, joins, and teardown;
- `../engine/src/linux_abi/syscall/{dispatch.c,helpers.c,io.c,mem.c,proc.c,signal.c,time.c}`:
  syscall admission, guest pointer validation, vector I/O, anonymous mappings
  and partial unmaps, process/exit behavior, restart decisions, and time ABI;
- `../engine/tools/matrix_runner.c`: `suite_case_timeout_ms`,
  `case_timeout_ms`, `stall_timeout_ms`, launch/wait/termination on POSIX and
  Windows, and result comparison; `remote_supervisor.c`: `terminate_group` and
  `main` for descendant-bounded oracle execution.

## State, identity, lifetime, locks, and teardown

Each `run_guest` invocation owns one live CPU association for its host thread.
It registers the CPU in both the stop-the-world and thread-directed-signal
registries before entering guest blocks, installs the architecture-required
alternate signal stack, and unregisters before releasing that stack. Guest PC,
registers, flags, FP/vector state, stack, and Linux virtual addresses are the
guest-visible identity. Host RW/RX pointers, arena offsets, translation bodies,
and cache generations remain internal.

The block map owns guest-PC-to-body publication; the instruction map owns
host-PC-to-guest-PC fault reconstruction; the current and retired arenas own
code storage. `map_put` publishes only a complete translation. The dispatcher
holds `g_jit_lock` for lookup, translation, publication, chaining, and generation
selection once threads exist, but never while executing a translated block.
Stop-the-world cache rotation parks registered peers at block boundaries,
publishes a fresh generation, and frees a retired generation only after no CPU
can enter it. `jit_after_fork` repairs inherited locks and registries for the
single surviving child. Final task and process teardown waits for execution to
leave before unmapping guest mappings, code arenas, and signal storage.

Guest libc owns heap, stdio, regex/glob, environment, conversion, qsort, and
atexit state inside guest mappings. Engine descriptor entries own descriptor
identity; open-file descriptions own offsets and lifetime. Mapping ledgers own
guest ranges and backing identity. Signal queues and masks are task/process
state, not global ABI-test state.

## Ordering, partial results, blocking, cancellation, signals, and errno

Every block exit spills guest-visible state before the dispatcher interprets
its reason. AArch64 advances PC by four after a completed `svc` unless exec or
signal return redirected it; x86-64 preserves its architectural syscall
clobbers and emitter-selected next RIP. Linux syscall owners decide errno and
restart semantics. Guest pointers and counts are validated before host access;
vector reads/writes preserve completed prefixes, shared OFD offsets, `EINTR`,
and retry ordering. Blocking calls wake through task/signal mechanisms rather
than spinning, and process cancellation joins live guest execution before
releasing mappings or descriptors.

Signal delivery occurs only at a consistent block/syscall boundary.
`sigreturn_frame` restores the saved architecture-specific context before newly
unblocked pending signals are considered. `syscall_should_restart` distinguishes
transparent `SA_RESTART` replay from guest-visible `EINTR`; `sigsetjmp` escapes
restore guest stack/register/mask state without unwinding through host FFI.
`munmap` validates Linux alignment/length ordering, removes mapped intersections,
and treats holes according to Linux semantics. Recoverable guest failures become
Linux errno or a guest signal; they do not panic the host runtime.

## Architecture and host branches

AArch64 uses fixed-width decode, guest GPR/vector layout, NZCV state, AAPCS64
argument/return rules, explicit SVC PC advance, and AArch64 signal frames. The
`stolen_regs` case specifically protects the translator's reserved-register
save/restore contract. x86-64 uses variable-width decode, partial-register and
lazy flag rules, SysV argument/return rules, SSE/x87 state, direction-sensitive
REP operations, `moffs`, BT width preservation, SHLD/SHRD flags, and x86 signal
frames. Consequently `mov-moffs`, `fpedge`, `fpdnan`, `repmovsdf`, `x87m80`,
`shldflags`, and `btwidth` remain x86-only, while `stolen-regs` remains
AArch64-only.

The retained cache and fault adapters branch for Linux/macOS/Windows W^X and
signal/exception mechanisms, but expose the same guest Linux addresses and
register state. The test oracle uses Linux-user QEMU for both guest ISAs so host
locale, filesystem paths, and CPU identity do not become expected output.

## Retained-to-Rust capability matrix

| Retained capability | Rust owner | Audit state |
|---|---|---|
| AArch64 decode, integer/FP/atomic execution and CPU state | `hl-execution::aarch64` | implemented; cohort evidence required |
| x86 decode, scalar/vector/string/x87 execution and flags | `hl-execution::x86` | implemented; x86-only cohort evidence required |
| stable CPU layouts and native entry/cache ABI | generated CPU schema plus `src/runtime/native/exec` | implemented; exact-layout/native evidence remains a wider gate |
| ELF load, initial stack and guest-visible entry addresses | `hl-loader` | implemented |
| guest ranges, anonymous mappings, protection and unmap publication | `hl-memory`; syscall join in `hl-runtime` | implemented; partial-unmap behavior is directly exercised |
| syscall numbers, argument codecs, signal frames and errno values | `hl-linux` | implemented |
| descriptor I/O, OFD offsets, filesystem and pipe joins | `hl-descriptor`, `hl-vfs`, `hl-ipc`, and `hl-runtime` | implemented; cohort evidence required |
| process/thread registration, fork, exit and signal queues | `hl-task`; joins in `hl-runtime` | implemented; cohort evidence required |
| block-boundary signal delivery and `rt_sigreturn` | `hl-runtime::signal` with `hl-linux` frame codecs | implemented; both-ISA evidence required |
| libc heap, math, strings, regex/glob, qsort and atexit algorithms | guest static libc | outside engine ownership; preserved as acceptance evidence |
| retained host-specific W^X/fault adapters | `src/runtime/native/exec` platform adapters | migration is platform-gated outside this fixture-only lane |

No case licenses application-, runtime-, or vendor-specific production behavior.
A failure must be assigned to the generic execution, loader, memory, descriptor,
VFS, task, signal, time, or Linux ABI invariant above.

## Preserved fixture contract

`test.yaml` contains exactly 35 cases and 62 target rows. IDs are stable
`runtime/abi-core/<legacy-case>` values. Every source and golden is copied from
the corresponding retained manifest row; target membership, optimized
static-PIE/pthread/math flags, empty argv/environment, exit code, 120-second
deadline, and dependency intent are preserved. The dependency labels are
recorded below because the typed runtime YAML intentionally has no free-form
dependency field:

| Cases | Retained dependencies |
|---|---|
| `hello`, `longjmp` | `linux-libc` |
| `recursion` | `codegen,stack` |
| `pipe` | `linux-libc,pipe` |
| `regex` | `linux-libc,regex` |
| `strings` | `linux-libc` |
| `math` | `linux-libc,libm` |
| `bitops` | `codegen` |
| `mov-moffs` | `x86-moffs` |
| `varargs` | `linux-libc,varargs` |
| `fnptr` | `ibtc` |
| `ibtc-dispatch` | `ibtc,computed-goto` |
| `jumptable` | `indirect-branch` |
| `floatmath` | `libm,fp` |
| `fpedge`, `fpdnan` | `x86-sse,fp` |
| `repmovsdf` | `x86-df,rep-movs` |
| `x87m80` | `x87,m80` |
| `shldflags` | `x86-shld-shrd,flags` |
| `btwidth` | `x86-bt-bts-btr-btc` |
| `stolen-regs` | `aarch64-register-mangling` |
| `heap` | `linux-libc,heap` |
| `files`, `statfile` | `linux-libc,filesystem` |
| `mmapanon`, `munmap-partial` | `linux-libc,mmap` |
| `qsort`, `sortbig` | `linux-libc,qsort` (`sortbig` also `heavy`) |
| `glob` | `linux-libc,glob` |
| `strtod` | `linux-libc,fp` |
| `timefmt` | `linux-libc,time` |
| `environ` | `linux-libc,environment` |
| `atexit` | `linux-libc,atexit` |
| `sigaction` | `linux-libc,signals,fork/wait` |
| `sigjmp` | `linux-libc,signals` |

There are no argv, environment, or auxiliary fixture dependencies in this
cohort. All declared outputs are category-local goldens.

Source-name collisions that make merging this category into a different owner unsafe include
`fnptr.c`, `qsort.c`, `mmapanon` versus the transformed `mmap_anon.c`, the
cross-category `ibtc_dispatch.c`, and `hello.c` in the ISA category. The
complete category keeps the retained bytes and IDs unambiguous without
overwriting those owners. There are no prebuilt binaries or result captures.
