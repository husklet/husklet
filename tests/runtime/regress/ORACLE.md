# Retained regression-domain oracle

> **Historical ownership:** Deleted Rust-engine paths below remain as audit
> evidence. These regression cases now gate the selected C engine under
> `src/runtime/native/retained`.

The source programs and expected stdout in this folder are byte-for-byte copies
from `../engine/tests/compat/core/regress`. The retired engine itself is the
behavioral oracle; it was inspected read-only. This audit covers the complete
implementation mechanisms joined by this cohort rather than treating fifteen
historical failures as fifteen special cases.

## Retained implementation inspected

- `src/linux_abi/syscall/helpers.c`: the open-file-description offset and seek
  paths, including `poslk_resolve`; `src/linux_abi/syscall/io.c` and
  `src/linux_abi/syscall/fs.c`: syscall dispatch, read/pread ordering, overlay
  descriptor handling, and `lseek` mutation.
- `src/linux_abi/syscall/proc.c`: `sched_pid_live` and the
  `sched_getaffinity`/`sched_setaffinity` cases; `src/linux_abi/thread.c`:
  `thread_tid_alive`, live-thread registration, exit, and post-fork repair.
- `src/linux_abi/elf.c` and `src/linux_abi/x86.c`: `load_elf`, non-PIE image
  placement, stack reservation/guard construction, guest/storage bias state,
  fault entry, and `nonpie_fixup`.
- `src/translator/guest/aarch64/translate.c`: `translate_block`, conditional
  compare lowering, literal LDR/LDRSW/SIMD/PRFM rewriting, address folding, and
  block exits; `src/linux_abi/elf.c`: LDAPR and LDXP/STXP/CASP non-PIE fault
  repair.
- `src/translator/guest/x86_64/avx.c`: `hl_x86_avx_address`, `avx_ea`, and the
  guarded vector-memory interpreter; `rep.c`: `hl_x86_rep_compare`, partial
  iteration and flag/write-back rules; `interp.c` and the corresponding move
  lowering: non-PIE LEA/return-address and immediate materialization behavior.
- `src/linux_abi/signal.c`: `maybe_deliver_signal`, host-to-guest signal
  queuing, alternate-stack frame construction, and the retained `g_go_image`
  SIGURG suppression; `src/linux_abi/syscall/signal.c`: signal syscall ordering.
- `src/translator/cache.c`, both architecture dispatch headers, and
  `src/core/dispatch.c`: translation publication, block-return state, signal
  safe-points, cache identity, invalidation, and teardown.

The retained tree has no standalone `.S`, `.s`, or `.asm` implementation file.
The architecture entry assembly is inline in the naked `run_block` definitions
in `src/core/dispatch.c` and
`src/translator/guest/x86_64/translate.c`; those definitions and their spill,
callee-save, environment-register, return-reason, and unwind boundaries were
included in the audit.

## State, identity, lifetime, locking, and teardown

The retained descriptor table owns descriptor-local flags while an open file
description owns the shared seek position. A successful `lseek` mutates that
OFD before a later `read`; `pread` observes an explicit offset without changing
it. Duplicate descriptors therefore share one cursor and unrelated OFDs do not
serialize. Descriptor references keep the host object alive across the host
operation, and final close tears it down only after the table reference is
removed.

Guest task identity is the registered guest PID/TID, not an incidental host
PID. `sched_pid_live` resolves the caller, container init, and live guest thread
registry before any bare-host probe. The thread registry is joined before guest
execution, removed during exit, and repaired to the one surviving thread after
fork. Its lock protects identity/lifetime lookup; host scheduling calls are not
made while unrelated task-table state is held.

For executable images, guest identity remains the ELF link address. The loader
may reserve storage elsewhere and retains one `(guest interval, storage
interval)` projection for the image lifetime. Translation, syscall marshalling,
fault reconstruction, auxv, signal frames, and checkpoint state must all use
that same projection. Code-cache host addresses are private translation
identities. Completed blocks are published under the cache lock; translated
code runs without that lock. Invalidation retires generations before freeing
storage, and fork repair removes identities belonging to vanished host threads.

The main stack reservation owns an inaccessible lower guard followed by the
writable stack. Fault handling first distinguishes an internal projection or
code-cache fault from a guest fault. A guest-owned SIGSEGV is reconstructed at
the guest PC and delivered on the configured alternate stack; default-fatal
delivery terminates the guest process group. Teardown cannot unmap stack,
image, or code storage while a registered execution/fault context can still
refer to it.

## Ordering, partial results, blocking, signals, and errno

Seek validation and offset arithmetic occur before cursor mutation. Failure
leaves the OFD position unchanged and reports the Linux errno; EOF reads return
the available prefix. The offset tests deliberately distinguish shared cursor
state from positional I/O and retain full signed 64-bit `off_t` values.

Affinity validates mask width, target existence, and guest output memory in
Linux order. A live nonleader TID succeeds; an absent task returns `ESRCH`, an
invalid mask size returns `EINVAL`, and inaccessible output returns `EFAULT`.
No host-PID lookup may replace guest identity.

Translated instructions commit architectural state only after required operand
accesses succeed. REP compare/scans retain completed-iteration count, RSI/RDI,
RCX, direction, and arithmetic flags across yield or fault. Pair atomics commit
both words as one operation; failed compare returns the observed pair without a
store. Acquire loads preserve their ordering edge. Literal loads compute from
the guest PC, not the host code-cache PC. Conditional compare either performs
the comparison or installs the encoded literal flags exactly.

Host faults are reconstructed before queuing a guest signal. Signal delivery
occurs at a block boundary after complete guest state is spilled. Alternate
signal-stack entry must remain possible when the main stack is exhausted, and
the handler's `_exit(42)` must not be rewritten into a generic engine crash.
Blocking syscalls retain Linux interruption and partial-result behavior; no
translator path retries an operation after guest-visible progress.

## Architecture and host branches

AArch64-specific coverage is CCMP/CCMN flag selection, the complete
PC-relative literal family, LDAPR acquire loads, and LDXP/STXP/CASP pair
atomics. x86-64-specific coverage is non-PIE AVX/SSSE3 memory operands, REP
CMPS/SCAS state, immediate/return-address materialization, and the fixed-image
pointer model. SHA-512 and seek/affinity/stack cases execute on both guest ISAs.

The host split is mechanism-based: Linux may map an `ET_EXEC` at its link
address, while macOS address reservations can require displaced storage;
POSIX signal/ucontext entry differs from Windows VEH/CONTEXT, and macOS W^X
publication differs from Linux executable mappings. These differences must not
change guest addresses or Linux-visible results.

Two retained mechanisms violate the current architecture rule and are evidence,
not designs to copy: `g_go_image` suppresses SIGURG based on a Go image, and the
V8 symbol path rebases one named embedded-blob constant. Rust must instead make
arbitrary-PC signal delivery and general guest/storage projections correct. No
Go-, V8-, executable-, or vendor-specific production branch is accepted.

## Capability mapping to Rust

| Retained capability | Rust owner | State |
|---|---|---|
| OFD seek/read ordering and 64-bit offsets | `hl-runtime/src/filesystem/position.rs`, `hl-descriptor::OperationLease` | implemented; cohort is acceptance evidence |
| Guest PID/TID affinity lookup | `hl-runtime/src/process/schedule.rs`, `hl-task` task identities | implemented; cohort is acceptance evidence |
| ELF guest/storage separation and auxv pointers | `hl-loader::{projection,transaction,stack}`, `hl-memory` mapping projection | implemented model; full non-PIE cohort remains acceptance evidence |
| Lower stack guard and SIGSEGV delivery | `hl-loader/src/transaction.rs`, `hl-runtime` signal state, `hl-engine` execution fault adapter | implemented model; end-to-end handler result remains acceptance evidence |
| AArch64 CCMP and literal loads | `hl-execution/src/aarch64/{integer_decode,interpreter,memory}` | implemented |
| AArch64 LDAPR and pair atomics | `hl-execution` AArch64 memory/atomic decode and interpreter/native lowering | implemented in the portable execution model; native parity remains evidence-driven |
| x86 REP compare partial state | `hl-execution/src/x86/string.rs` | implemented |
| x86 vector memory through guest projection | `hl-execution` vector decode plus `GuestOperandMemory`; `hl-memory` projection | partially evidenced; no application-specific rebase is allowed |
| Arbitrary-PC async signal delivery | `hl-runtime` signal domain plus `hl-engine` execution adapter | remaining compatibility risk; no Go suppression ported |
| V8 named-symbol immediate rewrite | no Rust owner by design | retained hack rejected; general pointer invariant must satisfy the case |

The focused QEMU evidence is recorded in `EVIDENCE.md`. A passing native oracle
does not by itself prove the Rust engine; the category must subsequently run
through the committed Husklet engine with typed native options and diagnostics.
