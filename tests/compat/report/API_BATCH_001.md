# Compatibility batch 001

Run date: 2026-08-01. The retained ledger is `API_BATCH_001.partial.tsv`.

The bounded invocation selected the first 12 deterministic Linux inventory
rows with `HL_COMPAT_BATCH=12`, `HL_COMPAT_JOBS=1`,
`HL_COMPAT_STALL_MS=5000`, and resume enabled. It recorded 10 passes, two
failures, and no skips. Both failures are `abi/atomic-builtins`; all AArch64 and
x86-64 legs of `ackermann`, `alloca`, `atomicops`, `atomics`, and `bitfield`
passed. AArch64 `atomic-builtins` exceeded its ten-second row deadline. The
x86-64 leg exited on guest SIGSEGV before output. No compatibility worker or
zombie remained after teardown.

The pre-run failure was harness-only. Whole-selection fingerprinting reached
the absent `pclib_blob_arm.bin` declared by the later `filesystem/pc-libmap`
row. The retained C matrix gives every case a fresh private `/tmp`; that guest
opens its blob with `O_CREAT` and therefore makes initial absence legal. Resume
fingerprinting now records the declared path and explicit present/absent state
for that optional side input. Guests, goldens, runtime resources, and runner
binaries remain required and fail preflight when absent. Focused tests cover
legal absence, required absence, and present-byte changes.

This fixes resume admission only. `compat-worker` still models `side-file` as a
required host `Input::File`; that does not match the retained runner's empty
persistent scratch semantics and will fail when `pc-libmap` itself is selected.
Its typed fixture must become an owned writable scratch path whose first run may
create the file and whose intended cold/warm lifetime is explicit. Do not add a
committed seed blob merely to bypass that contract.

## Atomic failure audit

The immutable fixture is `../engine/tests/compat/abi/atomics_builtins.c`: four
pthreads each execute 250,000 `__sync_fetch_and_add` operations against one
`long`; the golden is `atomic v=1000000`.

The AArch64 artifact calls `__aarch64_ldadd8_sync`. Its advertised-LSE branch is
one `ldaddal`; its fallback is an `ldxr` / add / `stlxr` retry loop. The retained
C engine advertises `HWCAP_ATOMICS`, recognizes and inlines outline atomic
helpers in `translator/guest/aarch64/translate.c`, and uses host atomic CAS for
the interpreter monitor in `translator/guest/aarch64/interp.c`. Rust currently
interprets the loop and `hl-memory::store_exclusive` rejects a reservation when
the address-space-wide write epoch changes, not only when the reserved bytes
conflict. The observed timeout is therefore a genuine engine performance and
reservation-granularity gap; this batch does not prove the eventual value
incorrect. It must not be hidden by increasing the timeout.

The x86-64 artifact executes `lock addq $1,[rip+v]`. The retained C engine keeps
the LOCK bit and routes ALU-to-memory through `interp_locked_rmw` in
`translator/guest/x86_64/interp.c`, using host CAS for aligned operands and a
hashed lock for permitted split locks. Its lowering routes the same operation
through `lock_rmw` in `translator/guest/x86_64/lower/alu.c` and
`translator/guest/x86_64/translate.c`. Rust validated the prefix but discarded
it when constructing `ScalarInstruction::Alu`, then performed an ordinary
read/reserve/write. That violates the generic x86 invariant and can corrupt
glibc synchronization before this fixture reaches its own counter.

The source correction retains `locked` in the owned ALU IR and routes every
legal memory ADD, OR, ADC, SBB, AND, SUB, and XOR through one sequentially
consistent `ExclusiveMemory::fetch_update`. The current memory owner serializes
these operations transactionally, including unaligned operands; it is coarser
than the retained C hashed split-lock policy. LOCK on a register destination or
read-only TEST/CMP is rejected before execution. Focused source tests cover
decode retention and rejection, all seven operations on an unaligned qword,
a cache-line-crossing qword, four-thread contention, atomic-fault CPU rollback,
and noncanonical-address precedence. The serialized warnings-denied gate passed.

The first focused x86 rerun exposed an independent 64 MiB address-space defect,
recorded in checkpoint 65. After its generic correction,
`ATOMIC_X86_FIXED.tsv` records the retained x86-64 `abi/atomic-builtins` row
passing with exit 0. `ATOMIC_ARM.tsv` still records the AArch64 row timing out;
that monitor/performance work remains a separate lane. Do not start another
batch until that lane is settled.
