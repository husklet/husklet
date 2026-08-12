# Exact ABI corpus oracle

The ABI cohorts preserve deterministic C programs and byte-exact goldens, but
their implementation oracle is the retained read-only engine in `../engine`.
The audit read these implementation owners and entry points, rather than only
the fixtures and manifests:

- `src/core/dispatch.c`: `run_guest` and the AArch64 and x86-64 `run_block`
  entry trampolines;
- `src/translator/cache.c`: `map_host`, `map_put`, `jit_hostpc_lookup`,
  `jit_flush_to_fresh`, `stw_flush`, `smc_inplace_drop`, and `jit_after_fork`;
- `src/translator/arena.c`: arena reservation, allocation, reset, and teardown;
- `src/translator/guest/aarch64/translate.c`: `translate_block`, its generated
  block-return path, and `smc_icflush`; `dispatch.h`, `abi.h`, `cpu.h`,
  `signal.c`, and `stubs.c` for return-reason, CPU-layout, signal-context, and
  helper-call contracts;
- `src/translator/guest/x86_64/translate.c`: `translate_block` and `run_block`;
  `decode.c`, `operand.c`, `flags.c`, `rep.c`, `x87state.c`, `signal.c`,
  `cpuid.c`, `dispatch.h`, `abi.h`, and `cpu.h` for variable-length decode,
  operands, flags, string partial progress, x87/SIMD state, signal frames, and
  advertised CPU identity; and `cache.c` for persistent-cache admission;
- `src/linux_abi/signal.c`: signal-frame construction, `sigreturn_frame`, and
  guest-context restoration used by setjmp/ucontext and asynchronous delivery;
- `tools/matrix_runner.c`: `suite_case_timeout_ms`, `case_timeout_ms`,
  `stall_timeout_ms`, both platform launch paths, and `main`; and
  `tools/remote_supervisor.c`: `terminate_group` and `main`, for bounded oracle
  execution and descendant teardown.

No retained file was edited during this study.

## State, identity, locking, and teardown

`run_guest` owns one live `struct cpu` association for the host thread. It joins
the translated-thread and signal registries before entering blocks and leaves
both before releasing the alternate signal stack, so an exited thread cannot
remain a cache-flush or signal target. Guest PC and registers remain the public
identity; RW/RX code addresses and cache generations are internal identities.
The block map owns guest-PC-to-body publication, the instruction map owns
host-PC-to-guest-PC fault reconstruction, and the W^X arena/code mapping owns
storage. `map_put` publishes only completed translations. Invalidation removes
or generations stale identities before retranslation.

Single-threaded execution may reset a cache in place. Once guest threads exist,
the dispatcher holds `g_jit_lock` across lookup, translation, publication, and
generation selection. It does not hold that lock while a translated block runs.
`stw_flush` parks registered peers at block-boundary safepoints, switches to a
fresh cache, retires the old generation, and frees it only when no thread's
published execution generation can enter it. `jit_after_fork` repairs inherited
registries and mutex state in the single surviving child. Persistent-cache
images are admitted only when their architecture, layout, guest-address, and
code assumptions match. Final teardown unmaps retired mappings and arena
storage only after execution and fault lookup can no longer reference them.

## Ordering, partial results, signals, and errors

Every translated block fully spills guest-visible state before returning a
reason. The dispatcher handles that reason before asynchronous delivery, then
calls `maybe_deliver_signal` at the next safe boundary. `rt_sigreturn` restores
the saved context before draining newly unblocked pending signals; multiple
unblocked signals therefore run back-to-back before ordinary guest progress.
Signal handlers that escape through `siglongjmp` are detected by the stack
unwind checks rather than requiring an executable-specific exception.

Syscall instructions terminate the block. AArch64 `svc` advances the guest PC
by four only according to the syscall-return contract; x86-64 preserves the
architectural syscall clobbers and next RIP. Linux service owners, not the ABI
translator, decide errno, `EINTR`, restart, blocking, and partial-I/O results.
The dispatcher preserves the returned registers and does not retry a partial or
interrupted operation itself. REP/string lowering records completed iterations
and fault position so a signal, fault, or short operation cannot be reported as
if the entire instruction completed. Decode, arithmetic, divide, memory, and
explicit trap exits reconstruct exact guest PC/state before signal conversion.

The matrix runner supplies a distinct deadline and bounded capture per launch.
On POSIX its child/process-group teardown is TERM, bounded grace, KILL, and
reap; the remote supervisor repeats group teardown on timeout or interruption.
Windows uses the corresponding launch/job ownership path. A timeout, output
overflow, signal termination, or nonzero exit remains a failure and cannot be
hidden by a later case.

## Architecture and host branches

ABI-visible integer, floating-point, vector, stack, condition-code, TLS,
setjmp/ucontext, and indirect-call behavior is architecture-specific at decode,
lowering, register layout, and assembly entry. It is not application-specific.
The retained AArch64 path advances the PC according to fixed-width instruction
and syscall rules and performs explicit instruction-cache invalidation. The
x86-64 path preserves variable-length decode, flags, x87/SIMD state, and
guest-address identity even when translated storage moves internally.

The retained host branches are mechanism branches: POSIX signals/ucontext and
process groups, Windows VEH/CONTEXT and job/process ownership, and macOS W^X /
`MAP_JIT` constraints. Guest branches are AArch64 fixed-width decode, AAPCS64
aggregate/HFA/TLS and FP/SIMD rules versus x86-64 variable-width decode,
SysV register/stack returns, RFLAGS, x87, SSE/AVX, CPUID, and TLS. The retained
macOS `swapcontext` exclusion and generic QEMU CPUID difference remain typed
evidence; neither permits vendor- or executable-name branching.

## Capability mapping

| Retained C capability | Rust owner | State |
|---|---|---|
| CPU layouts used by entry code, signal frames, integer/FP/vector state | generated schema in `src/schema/cpu` and native layouts in `src/native/cpu` | implemented; ABI assertions remain mandatory |
| Block entry/return, full spill, return reasons, fault context | `src/runtime/native/exec` plus `hl-execution` runner | implemented; target gates cover both ISAs |
| AArch64 decode, ALU, FP/SIMD, atomics/LSE, branches and fixed PC advance | `src/runtime/hl-execution/src/aarch64` | implemented for this cohort; corpus remains the broader completeness gate |
| x86 decode, operands, RFLAGS, REP progress, x87/SSE/AVX, CPUID and IRETQ | `src/runtime/hl-execution/src/x86` | implemented for active rows; CPUID oracle mismatch remains typed broken evidence |
| Guest memory loads/stores, unaligned access, atomics and executable-write invalidation | `hl-memory`, `hl-execution`, and the `hl-runtime` execution adapter | implemented for active rows; exact invalidation concurrency remains a full-corpus gate |
| W^X cache mapping, publication, lookup, chaining, generations and retirement | `src/runtime/native/exec` code-cache boundary | implemented; clean-tree native and stress gates required for parity |
| Syscall boundary and exact errno/partial/EINTR behavior | Linux personality in `hl-runtime` and domain owners | implemented for exercised calls; ABI fixtures alone do not prove every syscall domain |
| Signals, signal frames, setjmp/longjmp and ucontext restoration | `hl-task`, Linux signal adapter, and native fault entry | setjmp/longjmp active; retained macOS `swapcontext` path remains an explicit gap |
| TLS models and static/non-PIE guest addresses | loader, memory, task TLS, and execution address translation | implemented for active TLS rows; guest-visible addresses must never expose host storage |
| Fork repair and cache/thread registry lifetime | task fork composition plus native fork-critical repair | implemented boundary; nested/fork stress gates remain required |
| Oracle deadline, capture, cancellation, descendant cleanup | `src/apps/testing/src/runtime` runner and container lifecycle | bounded runner and cleanup implemented |

Each ABI case is acceptance evidence for one exact contract; none licenses a
runtime- or executable-name branch. Any new failure must map to a generic
decode, register, flag, memory, signal/context, syscall, cache, or lifecycle
invariant from this table.

The migration preserves source bytes and golden fingerprints mechanically.
Cases retain their original target set, compiler flags, exit status, and
checked stdout. Host-specific exclusions remain explicit status evidence rather
than being silently omitted.


## Retained manifest rows

```text
alloca	exact-abi-corpus	alloca.c	ext_abi/alloca.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/alloca.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
atomics_st	exact-abi-corpus	atomics_st.c	ext_abi/atomics_st.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/atomics_st.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
bigswitch	exact-abi-corpus	bigswitch.c	ext_abi/bigswitch.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/bigswitch.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
boolconv	exact-abi-corpus	boolconv.c	ext_abi/boolconv.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/boolconv.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
bswap	exact-abi-corpus	bswap.c	ext_abi/bswap.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/bswap.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
charmath	exact-abi-corpus	charmath.c	ext_abi/charmath.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/charmath.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
clmul	exact-abi-corpus	clmul.c	ext_abi/clmul.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/clmul.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
cmpchain	exact-abi-corpus	cmpchain.c	ext_abi/cmpchain.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/cmpchain.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
computed_goto	exact-abi-corpus	computed_goto.c	ext_abi/computed_goto.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/computed_goto.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
deep_recursion	exact-abi-corpus	deep_recursion.c	ext_abi/deep_recursion.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/deep_recursion.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
divmod	exact-abi-corpus	divmod.c	ext_abi/divmod.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/divmod.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
endian	exact-abi-corpus	endian.c	ext_abi/endian.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/endian.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
float32	exact-abi-corpus	float32.c	ext_abi/float32.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/float32.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
fnptr_many	exact-abi-corpus	fnptr_many.c	ext_abi/fnptr_many.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fnptr_many.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
fp_classify	exact-abi-corpus	fp_classify.c	ext_abi/fp_classify.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fp_classify.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
fp_cmp	exact-abi-corpus	fp_cmp.c	ext_abi/fp_cmp.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fp_cmp.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
fp_conv	exact-abi-corpus	fp_conv.c	ext_abi/fp_conv.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fp_conv.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
fp_fma	exact-abi-corpus	fp_fma.c	ext_abi/fp_fma.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fp_fma.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
fp_minmax	exact-abi-corpus	fp_minmax.c	ext_abi/fp_minmax.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fp_minmax.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
fp_round	exact-abi-corpus	fp_round.c	ext_abi/fp_round.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fp_round.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
fp_special	exact-abi-corpus	fp_special.c	ext_abi/fp_special.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fp_special.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
globalinit	exact-abi-corpus	globalinit.c	ext_abi/globalinit.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/globalinit.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
hash	exact-abi-corpus	hash.c	ext_abi/hash.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/hash.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
int128	exact-abi-corpus	int128.c	ext_abi/int128.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/int128.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
intwrap	exact-abi-corpus	intwrap.c	ext_abi/intwrap.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/intwrap.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
longdouble	exact-abi-corpus	longdouble.c	ext_abi/longdouble.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/longdouble.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
manyargs	exact-abi-corpus	manyargs.c	ext_abi/manyargs.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/manyargs.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
memops	exact-abi-corpus	memops.c	ext_abi/memops.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/memops.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
mixedargs	exact-abi-corpus	mixedargs.c	ext_abi/mixedargs.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/mixedargs.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
mul64	exact-abi-corpus	mul64.c	ext_abi/mul64.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/mul64.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
nested_loops	exact-abi-corpus	nested_loops.c	ext_abi/nested_loops.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/nested_loops.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
overflow_builtin	exact-abi-corpus	overflow_builtin.c	ext_abi/overflow_builtin.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/overflow_builtin.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
popcnt	exact-abi-corpus	popcnt.c	ext_abi/popcnt.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/popcnt.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
printf_formats	exact-abi-corpus	printf_formats.c	ext_abi/printf_formats.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/printf_formats.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
ptr_arith	exact-abi-corpus	ptr_arith.c	ext_abi/ptr_arith.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/ptr_arith.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
qsort_cb	exact-abi-corpus	qsort_cb.c	ext_abi/qsort_cb.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/qsort_cb.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
rettypes	exact-abi-corpus	rettypes.c	ext_abi/rettypes.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/rettypes.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
rotate	exact-abi-corpus	rotate.c	ext_abi/rotate.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/rotate.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
setjmp_longjmp	exact-abi-corpus	setjmp_longjmp.c	ext_abi/setjmp_longjmp.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/setjmp_longjmp.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
shifts	exact-abi-corpus	shifts.c	ext_abi/shifts.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/shifts.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
signext	exact-abi-corpus	signext.c	ext_abi/signext.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/signext.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
simd_syscall	exact-abi-corpus	simd_syscall.c	ext_abi/simd_syscall.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/simd_syscall.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
syscall_crypto	exact-abi-corpus	syscall_crypto.c	ext_abi/syscall_crypto.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/syscall_crypto.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
struct_hfa	exact-abi-corpus	struct_hfa.c	ext_abi/struct_hfa.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/struct_hfa.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
struct_large	exact-abi-corpus	struct_large.c	ext_abi/struct_large.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/struct_large.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
struct_mixed	exact-abi-corpus	struct_mixed.c	ext_abi/struct_mixed.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/struct_mixed.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
struct_ret	exact-abi-corpus	struct_ret.c	ext_abi/struct_ret.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/struct_ret.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
struct_small	exact-abi-corpus	struct_small.c	ext_abi/struct_small.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/struct_small.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
tailcall	exact-abi-corpus	tailcall.c	ext_abi/tailcall.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/tailcall.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
union_pun	exact-abi-corpus	union_pun.c	ext_abi/union_pun.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/union_pun.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
varargs_float	exact-abi-corpus	varargs_float.c	ext_abi/varargs_float.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/varargs_float.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
varargs_mixed	exact-abi-corpus	varargs_mixed.c	ext_abi/varargs_mixed.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/varargs_mixed.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
varargs_pairs	mariadb-abi	varargs_pairs.c	focused-mariadb-regression	aarch64,x86_64	-static -O2 -std=gnu11	-	-	0	expected/varargs_pairs.out	linux-libc,abi,varargs	active	alternating pointer and unsigned varargs across register and stack save areas
vla	exact-abi-corpus	vla.c	ext_abi/vla.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vla.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
vtable	exact-abi-corpus	vtable.c	ext_abi/vtable.c	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vtable.out	linux-libc,abi	active	exact source provenance; checked deterministic golden; cross-ISA byte equality
x_dshift	exact-abi-corpus	x_dshift.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_dshift.out	linux-libc,abi	active	x86 SHLD/SHRD funnel double-shift; native-aarch64 golden, qemu-x86 cross-verified
x_adcsbb	exact-abi-corpus	x_adcsbb.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_adcsbb.out	linux-libc,abi	active	x86 ADC/SBB multi-word carry/borrow chains; native-aarch64 golden, qemu-x86 cross-verified
x_bitscan	exact-abi-corpus	x_bitscan.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_bitscan.out	linux-libc,abi	active	x86 BSF/BSR/TZCNT/LZCNT/POPCNT scans; native-aarch64 golden, qemu-x86 cross-verified
x_bittest	exact-abi-corpus	x_bittest.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_bittest.out	linux-libc,abi	active	x86 BT/BTS/BTR/BTC bit test-and-modify; native-aarch64 golden, qemu-x86 cross-verified
x_setcc	exact-abi-corpus	x_setcc.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_setcc.out	linux-libc,abi	active	x86 SETcc all 16 flag conditions; native-aarch64 golden, qemu-x86 cross-verified
x_cmov	exact-abi-corpus	x_cmov.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_cmov.out	linux-libc,abi	active	x86 CMOVcc branchless selects all conditions; native-aarch64 golden, qemu-x86 cross-verified
x_div128	exact-abi-corpus	x_div128.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_div128.out	linux-libc,abi	active	x86 DIV/IDIV 128-bit full-width divide/mod; native-aarch64 golden, qemu-x86 cross-verified
x_mulhi	exact-abi-corpus	x_mulhi.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_mulhi.out	linux-libc,abi	active	x86 MUL/IMUL/MULX high-half products; native-aarch64 golden, qemu-x86 cross-verified
x_rotate	exact-abi-corpus	x_rotate.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_rotate.out	linux-libc,abi	active	x86 ROL/ROR variable rotate 8/16/32/64; native-aarch64 golden, qemu-x86 cross-verified
x_rclrcr	exact-abi-corpus	x_rclrcr.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_rclrcr.out	linux-libc,abi	active	x86 RCL/RCR rotate-through-carry model; native-aarch64 golden, qemu-x86 cross-verified
x_shiftedge	exact-abi-corpus	x_shiftedge.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_shiftedge.out	linux-libc,abi	active	x86 shift-count masking + arithmetic/logical; native-aarch64 golden, qemu-x86 cross-verified
x_bswapmov	exact-abi-corpus	x_bswapmov.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_bswapmov.out	linux-libc,abi	active	x86 BSWAP/MOVBE byte reversal 16/32/64; native-aarch64 golden, qemu-x86 cross-verified
x_pextpdep	exact-abi-corpus	x_pextpdep.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_pextpdep.out	linux-libc,abi	active	x86 BMI2 PEXT/PDEP gather/scatter reference; native-aarch64 golden, qemu-x86 cross-verified
x_strops	exact-abi-corpus	x_strops.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_strops.out	linux-libc,abi	active	x86 REP MOVS/STOS/SCAS/CMPS + direction flag; native-aarch64 golden, qemu-x86 cross-verified
x_atomicrmw	exact-abi-corpus	x_atomicrmw.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_atomicrmw.out	linux-libc,abi	active	x86 LOCK xadd/cmpxchg/or/xor + XCHG RMW; native-aarch64 golden, qemu-x86 cross-verified
x_leaidx	exact-abi-corpus	x_leaidx.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_leaidx.out	linux-libc,abi	active	x86 LEA/SIB base+index*scale addressing; native-aarch64 golden, qemu-x86 cross-verified
flags_pfaf	exact-abi-corpus	flags_pfaf.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/flags_pfaf.out	linux-libc,abi	active	x86 exact PF/AF/CF/OF/SF/ZF after add/sub; native-aarch64 golden, qemu-x86 cross-verified
sse2_packint	exact-abi-corpus	sse2_packint.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/sse2_packint.out	linux-libc,abi	active	SSE2 packed-int add/sub/mul/sat/min/max + PMOVMSKB; native-aarch64 golden, qemu-x86 cross-verified
sse_movmsk	exact-abi-corpus	sse_movmsk.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/sse_movmsk.out	linux-libc,abi	active	SSE MOVMSKPS/MOVMSKPD sign-bit extraction; native-aarch64 golden, qemu-x86 cross-verified
sse_shuffle	exact-abi-corpus	sse_shuffle.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/sse_shuffle.out	linux-libc,abi	active	SSE2 PSHUFD/PSHUFLW/PSHUFHW/UNPCK + mask blend; native-aarch64 golden, qemu-x86 cross-verified
sse_fmath	exact-abi-corpus	sse_fmath.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/sse_fmath.out	linux-libc,abi	active	SSE ADDPS/MULPS/DIVPS/MINPS/MAXPS/SQRTPS/CVTTPS; native-aarch64 golden, qemu-x86 cross-verified
x_lahf	exact-abi-corpus	x_lahf.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_lahf.out	linux-libc,abi	active	x86 LAHF flag-byte load after ADD; native-aarch64 golden, qemu-x86 cross-verified
x_xlat	exact-abi-corpus	x_xlat.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_xlat.out	linux-libc,abi	active	XLATB (opcode 0xD7 single-byte): AL = [RBX + zero-extended AL]
x87_exact	exact-abi-corpus	x87_exact.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x87_exact.out	linux-libc,abi	active	x87 FRNDINT honors the x87 control-word rounding mode (fldcw RC), so floorl/ceill/truncl round correctly
x_signext	exact-abi-corpus	x_signext.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_signext.out	linux-libc,abi	active	x86 MOVSX/MOVZX mixed-width promotion; native-aarch64 golden, qemu-x86 cross-verified
x_sahf	exact-abi-corpus	x_sahf.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_sahf.out	linux-libc,abi	active	x86 SAHF flag round-trip; native-aarch64 golden, qemu-x86 cross-verified
arith_ext	exact-abi-corpus	arith_ext.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/arith_ext.out	linux-libc,abi	active	CQO sign-extend + MOVSXD + IDIVQ/DIVL exact + IMUL overflow flag (CWD split out to x_cwd); native-aarch64 golden, qemu-x86 cross-verified
avx2_gather	exact-abi-corpus	avx2_gather.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/avx2_gather.out	linux-libc,abi	active	AVX2 gather VPGATHERDD/VGATHERDPS/VPGATHERDQ (0F38 90/92, VSIB masked per-lane load + mask-clear) + VPMULUDQ/VCVTDQ2PS/VCMPPS support; native-aarch64 golden, qemu-x86 cross-verified
avx2_int	exact-abi-corpus	avx2_int.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/avx2_int.out	linux-libc,abi	active	AVX2 256-bit VPERM2I128 (0F3A 46), VPUNPCKL/H bw-wd-dq-qdq (0F 60-6D); also fixed VPADDB/W/D mis-decoded as subtract; native-aarch64 golden, qemu-x86 cross-verified
avx2_shuf	exact-abi-corpus	avx2_shuf.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/avx2_shuf.out	linux-libc,abi	active	AVX2 VPERM2I128, VPERMQ (0F3A 00), VPBLENDD/VPBLENDW/VPBLENDVB (0F3A 02/0E/4C), VPACKSSWB (0F 63), VPACKUSDW (0F38 2B); native-aarch64 golden, qemu-x86 cross-verified
avx2_varshift	exact-abi-corpus	avx2_varshift.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/avx2_varshift.out	linux-libc,abi	active	AVX2 VPERM2I128 lane permute + VPUNPCK/pack support; native-aarch64 golden, qemu-x86 cross-verified
avx_fma	exact-abi-corpus	avx_fma.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/avx_fma.out	linux-libc,abi	active	FMA3 VFMADD/VFMSUB/VFNMADD 132/213/231 ps&pd single-rounding; native-aarch64 golden, qemu-x86 cross-verified
avx_fp	exact-abi-corpus	avx_fp.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/avx_fp.out	linux-libc,abi	active	AVX 256-bit YMM VADDPS/VMULPS/VDIVPS/VADDPD/VBROADCASTSS/VPERM2F128/VEXTRACTF128; native-aarch64 golden, qemu-x86 cross-verified
x_bmi	exact-abi-corpus	x_bmi.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_bmi.out	linux-libc,abi	active	BMI1/BMI2/ABM LZCNT/TZCNT/ANDN/BLSI/BLSR/BLSMSK/BZHI/RORX/MULX; native-aarch64 golden, qemu-x86 cross-verified
bt_mem	exact-abi-corpus	bt_mem.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/bt_mem.out	linux-libc,abi	active	BT/BTS/BTC memory operand with bit offset > operand size (wide-offset addressing); native-aarch64 golden, qemu-x86 cross-verified
x_mmx	exact-abi-corpus	x_mmx.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_mmx.out	linux-libc,abi	active	MMX 64-bit paddq/pmaddwd/psadbw/packsswb/punpcklbw/paddusb (already lowered) + EMMS (0F 77) no-op state reset; native-aarch64 golden, qemu-x86 cross-verified
x_movbe	exact-abi-corpus	x_movbe.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_movbe.out	linux-libc,abi	active	MOVBE 16/32/64 byte-swapping load/store; native-aarch64 golden, qemu-x86 cross-verified
partial_reg	exact-abi-corpus	partial_reg.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/partial_reg.out	linux-libc,abi	active	8-bit low-reg write preserves upper bits, 16-bit ADD wrap+flags, MOVSX/MOVZX (16-bit MOV split to x_movpart16); native-aarch64 golden, qemu-x86 cross-verified
x_shiftrot	exact-abi-corpus	x_shiftrot.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_shiftrot.out	linux-libc,abi	active	SHL by 0 (flags unchanged), count mask mod 32/64, ROL/ROR CF+count-1 OF (defined bits only); native-aarch64 golden, qemu-x86 cross-verified
x_sse3	exact-abi-corpus	x_sse3.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_sse3.out	linux-libc,abi	active	SSE3 HADDPS/HSUBPS (FADDP + UZP1/UZP2 deinterleave), ADDSUBPS/ADDSUBPD (sign-mask EOR + FADD), MOVSLDUP/MOVSHDUP (TRN1/TRN2); native-aarch64 golden, qemu-x86 cross-verified
sse41_int	exact-abi-corpus	sse41_int.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/sse41_int.out	linux-libc,abi	active	SSE4.1 PMULLD/PMOVSX/PMOVZX/PMINSD/PMAXUD/PACKUSDW/PBLENDW; native-aarch64 golden, qemu-x86 cross-verified
sse41_misc	exact-abi-corpus	sse41_misc.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/sse41_misc.out	linux-libc,abi	active	SSE4.1 PTEST(ZF/CF)/BLENDVPS/INSERTPS/EXTRACTPS/DPPS; native-aarch64 golden, qemu-x86 cross-verified
sse41_round	exact-abi-corpus	sse41_round.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/sse41_round.out	linux-libc,abi	active	SSE4.1 ROUNDPS all 4 rounding modes (ROUNDPD split to x_roundpd); native-aarch64 golden, qemu-x86 cross-verified
sse42_crc	exact-abi-corpus	sse42_crc.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/sse42_crc.out	linux-libc,abi	active	SSE4.2 CRC32 b/w/d/q + POPCNT; native-aarch64 golden, qemu-x86 cross-verified
x_ssse3	exact-abi-corpus	x_ssse3.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_ssse3.out	linux-libc,abi	active	SSSE3 PMADDUBSW (0F38 04) unsigned*signed byte multiply-add pairs -> signed-saturated words; native-aarch64 golden, qemu-x86 cross-verified
x87_cmp	exact-abi-corpus	x87_cmp.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x87_cmp.out	linux-libc,abi	active	x87 FUCOMIP->EFLAGS (ordered/unordered/NaN) + FNSTCW/FLDCW round-trip; native-aarch64 golden, qemu-x86 cross-verified
x87_prem	exact-abi-corpus	x87_prem.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x87_prem.out	linux-libc,abi	active	x87 FPREM/FPREM1/FSCALE exact partial-remainder+scale; native-aarch64 golden, qemu-x86 cross-verified
x_xadd	exact-abi-corpus	x_xadd.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_xadd.out	linux-libc,abi	active	XADD + 32-bit CMPXCHG read-modify + ZF (CMPXCHG8B split to x_cmpxchg8b); native-aarch64 golden, qemu-x86 cross-verified
x_cwd	exact-abi-corpus	x_cwd.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_cwd.out	linux-libc,abi	active	CANDIDATE BUG: CWD (0x66 0x99) 16-bit AX->DX sign-extend mistranslated on x86 engine; aarch64 engine + qemu-x86 match golden
x_movpart16	exact-abi-corpus	x_movpart16.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_movpart16.out	linux-libc,abi	active	CANDIDATE BUG: 16-bit MOV r16,r16 (0x66) does not preserve the upper 48 bits of the 64-bit dest on x86 engine; aarch64 engine + qemu-x86 match golden
x_roundpd	exact-abi-corpus	x_roundpd.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_roundpd.out	linux-libc,abi	active	SSE4.1 ROUNDPD packed-double lane parity; root cause was MOVDDUP (F2 0F 12) broadcasting only lane 0 (zeroing lane 1) so the vectorized 1/16 and -512 constants lost their high lane; native-aarch64 golden, qemu-x86 cross-verified
x_cmpxchg8b	exact-abi-corpus	x_cmpxchg8b.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_cmpxchg8b.out	linux-libc,abi	active	CANDIDATE BUG: CMPXCHG8B (0F C7 /1) never updates the 64-bit memory operand on x86 engine; aarch64 engine + qemu-x86 match golden
x_tls	exact-abi-corpus	x_tls.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_tls.out	linux-libc,abi	active	thread-local storage FS-relative access (mov %fs:off) initial-exec+global-dynamic across pthreads; native-aarch64 golden, qemu-x86 cross-verified
x_strsearch	exact-abi-corpus	x_strsearch.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_strsearch.out	linux-libc,abi	active	glibc SSE4.2 string search (strstr/strchr/strrchr/strspn/strcspn/strpbrk PCMPISTRI); native-aarch64 golden, qemu-x86 cross-verified
x_denorm	exact-abi-corpus	x_denorm.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_denorm.out	linux-libc,abi	active	SSE subnormal float/double arithmetic preserves denormals under default MXCSR (FTZ=0/DAZ=0); native-aarch64 golden, qemu-x86 cross-verified
x_wcs	exact-abi-corpus	x_wcs.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_wcs.out	linux-libc,abi	active	wide-char block ops wmemset/wmemcpy/wmemmove/wcsnlen/wmemchr/wmemcmp (REP STOSD/CMPSD); native-aarch64 golden, qemu-x86 cross-verified
x_pcmpistr	exact-abi-corpus	x_pcmpistr.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_pcmpistr.out	linux-libc,abi	active	SSE4.2 PCMPISTRI implicit-length string compare all four aggregation modes + polarity; native-aarch64 golden, qemu-x86 cross-verified
x_aesni	exact-abi-corpus	x_aesni.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_aesni.out	linux-libc,abi	active	AES-NI AESENC/AESENCLAST/AESDEC/AESDECLAST/AESIMC/AESKEYGENASSIST vs FIPS-197 reference; native-aarch64 golden, qemu-x86 cross-verified
sse41_extra	exact-abi-corpus	sse41_extra.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/sse41_extra.out	linux-libc,abi	active	SSE4.1 MPSADBW (0F3A 42) + PHMINPOSUW (0F38 41) + DPPD; MPSADBW/PHMINPOSUW were unimplemented (abort) and are now added to do_sse3b; native-aarch64 golden, qemu-x86 cross-verified
x_f16c	exact-abi-corpus	x_f16c.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x_f16c.out	linux-libc,abi	active	F16C VCVTPS2PH/VCVTPH2PS half-precision round-trip (also exercises VEX VSHUFPS/VPEXTRW/VMOVMSK that were unimplemented); native-aarch64 golden, qemu-x86 cross-verified
avx_lanes	exact-abi-corpus	avx_lanes.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/avx_lanes.out	linux-libc,abi	active	VEX 128/256 VSHUFPS/PD VUNPCKL/H VPERMILPS/PD VBLENDPS/PD VMOVMSKPS/PD VMOVLHPS/HLPS VPEXTRD/B VPINSRD/B VINSERTPS (all were unimplemented in do_avx and are now added); native-aarch64 golden, qemu-x86 cross-verified
avx_fpops	exact-abi-corpus	avx_fpops.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/avx_fpops.out	linux-libc,abi	active	VEX 256 VROUNDPS/PD + VDPPS (were unimplemented in do_avx); also exposed and fixed DPPS/DPPD FMA-contraction rounding bug on the aarch64 host; native-aarch64 golden, qemu-x86 cross-verified
vex_sse2int	exact-abi-corpus	vex_sse2int.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vex_sse2int.out	linux-libc,abi	active	VEX SSE2/AVX2 packed-integer ALU (saturating add/sub, min/max, avg, mullw/mulhw/mulhuw, pmaddwd, psadbw, scalar-count shifts) block-exited to do_avx; were UNIMPLEMENTED on x86 engine; native-aarch64 golden, qemu-x86 cross-verified
vex_ssse3	exact-abi-corpus	vex_ssse3.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vex_ssse3.out	linux-libc,abi	active	VEX SSSE3 integer (vphadd/sub w/d/sw, vpmaddubsw, vpsign b/w/d, vpmulhrsw, vpabs b/w/d) block-exited to do_avx; were UNIMPLEMENTED on x86 engine; native-aarch64 golden, qemu-x86 cross-verified
vex_sse41int	exact-abi-corpus	vex_sse41int.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vex_sse41int.out	linux-libc,abi	active	VEX SSE4.1/AVX2 integer (vpmuldq, vpcmpeqq, vpcmpgtq, vpmin/max sd/ud/sb/uw, vmovntdqa, vphminposuw, vptest, vtestps/pd) block-exited to do_avx; were UNIMPLEMENTED on x86 engine; native-aarch64 golden, qemu-x86 cross-verified
vex_fp	exact-abi-corpus	vex_fp.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vex_fp.out	linux-libc,abi	active	VEX vhaddps/pd vhsubps/pd vaddsubps/pd vsqrtps/pd vbroadcastf128/i128 vpermps vpermilps/pd-var block-exited to do_avx; were UNIMPLEMENTED on x86 engine; native-aarch64 golden, qemu-x86 cross-verified
vex_crypto	exact-abi-corpus	vex_crypto.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vex_crypto.out	linux-libc,abi	active	VEX vaesenc/enclast/dec/declast/imc, vaeskeygenassist, vpclmulqdq, vmpsadbw block-exited to do_avx; were UNIMPLEMENTED on x86 engine; FIPS-197 reference, qemu-x86 cross-verified
vex_maskmov	exact-abi-corpus	vex_maskmov.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vex_maskmov.out	linux-libc,abi	active	VEX vmaskmovps/pd + vpmaskmovd/q masked memory load/store block-exited to do_avx; were UNIMPLEMENTED on x86 engine; native-aarch64 golden, qemu-x86 cross-verified
vex_cvt	exact-abi-corpus	vex_cvt.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vex_cvt.out	linux-libc,abi	active	VEX vcvtpd2dq/vcvttpd2dq/vcvtdq2pd + vrcpps/vrsqrtps block-exited to do_avx; were UNIMPLEMENTED on x86 engine; rcp/rsqrt modeled at full precision to match qemu oracle + native aarch64
vex_movdp	exact-abi-corpus	vex_movdp.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vex_movdp.out	linux-libc,abi	active	VEX vmovlps/lpd/hps/hpd 64-bit stores (0F 13/17), vdppd (0F3A 41), scalar vsqrtss/vrcpss/vrsqrtss block-exited to do_avx; were UNIMPLEMENTED on x86 engine; qemu-x86 cross-verified
vex_fma2	exact-abi-corpus	vex_fma2.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/vex_fma2.out	linux-libc,abi	active	VEX vfmaddsub/vfmsubadd 132/213/231 ps/pd, vroundss/sd (0F3A 0A/0B), vlddqu (0F F0) block-exited to do_avx; were UNIMPLEMENTED on x86 engine; single-rounded FMA, qemu-x86 cross-verified
lse_rs	exact-abi-corpus	lse_rs.c	-	aarch64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/lse_rs.out	linux-libc,abi	active	aarch64-guest LSE atomic memory ops (SWP/LDADD/LDSET/LDCLR/LDEOR) with value operand Rs[20:16] = stolen reg x16/x17 on the generic decode path (SP base): gpr_field_mask did not flag Rs so a stolen Rs was emitted verbatim reading engine-private host x16/x17; native aarch64 golden
casp_stolen	exact-abi-corpus	casp_stolen.c	-	aarch64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/casp_stolen.out	linux-libc,abi	active	aarch64-guest CASP/CASPA/CASPL/CASPAL paired compare-and-swap (DWCAS) with Rs/Rt pair or base Xn = stolen reg x16/x17: the mangle only substituted the named field not the implicit pair partner Xs+1/Xt+1 so the high half read engine-private host regs and the swap spuriously failed; native aarch64 golden
ldst_structrm	exact-abi-corpus	ldst_structrm.c	-	aarch64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/ldst_structrm.out	linux-libc,abi	active	aarch64-guest AdvSIMD load/store structure (ld1/st1/ld2) with register post-index increment operand Rm[20:16] = stolen reg x16/x17 on the generic decode path (SP base skips emit_fold_advsimd_struct): gpr_field_mask did not flag Rm (Rt is a vector list) so a stolen stride advanced the base by the engine-private host x16/x17; fixed by flagging Rm for the 0x0C800000 post-index box; native aarch64 golden
stxp_status	exact-abi-corpus	stxp_status.c	-	aarch64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/stxp_status.out	linux-libc,abi	active	aarch64-guest store-exclusive-pair status register witness: stxp Ws,Xt1,Xt2,[Xn] writes success/fail into Ws[20:16] (a third GPR field in the exclusive box) which gpr_field_mask flags via the exclusive-group mask; stolen status reg x16 locks that coverage; native aarch64 golden
madd_ra	exact-abi-corpus	madd_ra.c	-	aarch64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/madd_ra.out	linux-libc,abi	active	aarch64-guest three-source multiply accumulator witness: MADD/MSUB/SMADDL/UMADDL name a fourth GPR field Ra[14:10] which gpr_field_mask flags via the 3-source mask; stolen accumulator x16/x17 locks that coverage; native aarch64 golden
fp_ctlflush	exact-abi-corpus	fp_ctlflush.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fp_ctlflush.out	linux-libc,abi	active	guest FP control register: x86 MXCSR FTZ/DAZ + aarch64 FPCR.FZ actually flush subnormals and survive control-word save/restore; COMISS/FCMPE signaling compare raises Invalid on qNaN where UCOMISS/FCMP does not; native-aarch64 golden, qemu-x86 cross-verified
fma_dnan	exact-abi-corpus	fma_dnan.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/fma_dnan.out	linux-libc,abi	active	FMA generated NaN (inf*0, inf-inf) carries the arch default-NaN sign (x86 indefinite sign-set 0xFFF8..) while an operand NaN propagates verbatim; regresses the FMA3 default-NaN sign fixup; native-aarch64 golden, qemu-x86 cross-verified
x87_fsw	exact-abi-corpus	x87_fsw.c	-	aarch64,x86_64	-static -O2 -std=gnu11 -pthread -lm	-	-	0	expected/x87_fsw.out	linux-libc,abi	active	x87 FSW sticky exception flags now project the host FP status into IE/ZE/PE + ES (fnstsw/fnstenv/fxsave); fnclex/fninit clear them; masked-vs-unmasked ES honored per FCW; OE/UE not asserted (binary64 ST carrier, H11); native-aarch64 golden, qemu-x86 cross-verified
```
