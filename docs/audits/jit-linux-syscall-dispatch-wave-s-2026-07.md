# Linux syscall dispatch/normalization audit — wave S (2026-07)

Scope: both guest number spaces, x86 legacy normalization, canonical-number mapping, the family dispatch chain, default returns, tests, and workload comments. A syscall is not dead merely because no tracked test invokes it. No code was changed.

## Dispatch model verified

The ARM guest uses asm-generic/aarch64 numbers directly. The x86 guest passes through two stages:

1. `x86_normalize()` rewrites legacy shapes whose arguments differ (`open`→`openat`, `stat/lstat`→`newfstatat`, legacy path/time operations, `poll`→`ppoll`, `fork/vfork`→`clone`, and x86-only register/TLS operations). It can also complete a syscall and return early.
2. `canon_x86()` maps the resulting x86 number by syscall name to the aarch64 canonical number. Unknown or genuinely x86-only numbers become `CANON_X86ONLY | raw_nr`.

`service_local()` then performs non-PIE pointer rebasing and filesystem-cache invalidation before asking each family in order: SysV IPC, memory, signal, time, I/O, AIO, filesystem, process, network, event, miscellaneous, and rare. A family returns false only when it does not own the number. The final default is silent `ENOSYS` unless JT/JTS tracing is enabled.

All compiled `svc_*` families are reachable from the chain. The default-return branches inside each family are required composition, not dead fallbacks. Merging them into one giant switch would reduce neither ABI surface nor normal cost and would make ownership harder to audit.

## Proven safe internal deletion

There is one duplicate unreachable handler:

- `x86_normalize()` case 201 implements x86 `time(time_t *)`, rebases the optional pointer, writes it, places seconds in `rax`, and returns `1`. `service_local()` immediately returns whenever `G_NORMALIZE(c)` returns true.
- `rare.c` also implements `case (CANON_X86ONLY | 201)`.
- That later case cannot be reached from an x86 syscall instruction because normalization always intercepts raw 201 first. ARM can never generate `CANON_X86ONLY`, because the macro and x86 mapper are absent from its composition.

Delete only the `rare.c` duplicate and its comment. Keep the normalization case and its behavioral tests/workload coverage. The rare comment claiming raw 201 otherwise reaches the unhandled logger is stale under the current normalize-first order.

This saves little code but removes two implementations that can drift on non-PIE pointer handling: the normalizer explicitly calls `x86_nonpie`, while the unreachable rare case directly dereferences `a0`.

## Intentional aliases that must remain

Legacy x86 aliases are public Linux ABI, even though they converge internally:

- `open`/`creat`→`openat`;
- `stat`/`lstat`→`newfstatat` with different flags;
- `mkdir`, `rmdir`, `unlink`, `readlink`, `chmod`, `chown`, `lchown`, `mknod`, `symlink`, `rename`, and `link`→their `*at` forms;
- `utime`, `utimes`, `futimesat`→`utimensat` with timeval/utimbuf conversion;
- `poll` and x86 `pause`→`ppoll` with distinct synthesized arguments;
- `fork`/`vfork`→`clone` with register restoration after service.

The normalization code is the deduplication. Deleting raw-number cases would break old binaries and libc fallbacks. It is safe to factor repeated register shuffles into helpers only if generated behavior and non-PIE rebasing remain identical.

`exit` and `exit_group`, `dup` variants, accept/accept4, pipe/pipe2, eventfd/eventfd2, signalfd/signalfd4, and older/newer stat/access calls are also not redundant: their flags, process scope, structure version, or observable semantics differ even when handlers share a helper.

## Number-table findings

`sysmap.h` describes itself as auto-generated, but it contains an explicit snapshot through syscall 471 and no checked-in generator was identified in this audit. Treat it as generated data only after locating or adding the authoritative generator and kernel header versions. New x86 numbers that are also asm-generic currently pass through unchanged only where explicit cases were added; an absent entry becomes biased x86-only and can miss an existing canonical handler.

Safe maintenance is to generate a table of `{x86 raw, name, canonical, normalization kind, owner family}` from pinned Linux UAPI headers and fail on:

- two names mapping unexpectedly to one canonical number;
- a canonical number claimed by multiple families;
- a mapper result with no owner (reported as missing behavior, not deleted);
- an owner case unreachable from ARM numbers and all x86 mapper/normalizer outputs;
- a raw x86 shape mapped by name even though its ABI needs normalization.

This is a structural consistency test. Behavioral tests must still issue raw syscalls in Rust/C and compare Linux/QEMU results.

## Default returns and compatibility shims

The final `ENOSYS` is correct for unknown numbers and intentionally unsupported facilities. Keep it silent by default. Several owned cases deliberately return probe-specific results:

- `rseq` returns `ENOSYS` so libc falls back;
- io_uring setup/register/enter report absence rather than partial support;
- selected presence probes return `EPERM`, `EINVAL`, or `EOPNOTSUPP` because applications distinguish “kernel lacks feature” from “feature present but denied.”

These are behavior contracts, not dead stubs. Audit them against Linux and real callers before changing. In particular, a handler that merely returns a constant can still be essential to Chrome, libc, databases, or language runtimes.

The x86 `arch_prctl` normalizer, legacy time conversions, pause conversion, and fork-register restoration are live compatibility shims. They are architecture-only by definition, not unreachable.

## Missing behavior, separate from deletion

The family-chain comment says “Every Linux syscall is now owned,” but the final path explicitly handles unowned numbers with `ENOSYS`; the statement overclaims completeness. Modern numbers in `sysmap.h` through 471 are not thereby implemented. Replace the comment with “known implemented syscalls are partitioned among families; unclaimed numbers return ENOSYS.”

Likewise, a family `case` does not prove full Linux semantics. Constant/probe returns, ignored arguments, fixed limits, host passthrough and partial emulation must remain visible in the syscall ledger. Missing tests should create coverage work, never deletion work.

Potential missing behavior should be generated as three lists:

1. ARM UAPI numbers with no family owner;
2. x86 mapped/normalized numbers with no family owner;
3. owned cases documented as partial/constant/unsupported.

Prioritize real workload needs (glibc/musl, Chrome, JVM/V8, databases, Go/Rust toolchains) and Linux ABI probes. Do not compile out untested syscall families.

## Hot-path and consolidation notes

Every syscall currently pays sequential family ownership checks until its owner. That is up to twelve switch/default returns, plus fs-generation polling and optional routing/ptrace gates. A generated direct canonical dispatch table or top-level owner switch could reduce branches while preserving family functions, but it is a performance refactor, not dead-code cleanup. Measure before changing: compilers often turn small switches into efficient range checks/jump tables, and one large sparse table can hurt instruction cache.

The non-PIE pointer switch duplicates the syscall-number ledger and can silently omit a new pointer-taking handler. Move pointer-position metadata into the generated manifest and produce specialized rebasing code/table. The filesystem mutation switch similarly duplicates ownership knowledge but has behavior-specific conditions (`openat/openat2`), so retain explicit hooks rather than a simplistic “all fs calls” flag.

## Maximal safe group and gates

**S1 (safe now):** remove the unreachable `CANON_X86ONLY|201` rare case and stale narrative; preserve x86 normalization case 201. Add/retain a raw x86 `time` test covering NULL and writable pointer, including non-PIE if supported.

**S2 (behavior-neutral tooling/docs):** correct the completeness comment and introduce a pinned generated cross-architecture ownership manifest. Do not remove any syscall implementation based on the manifest’s lack of test producer.

**S3 (performance-only, later):** if profiles show family-chain cost, generate a direct owner router while retaining handler bodies and final `ENOSYS`. Gate with every raw syscall test on both architectures, seccomp seeing the original raw number, ptrace entry/exit numbers, sentry normalization, errno translation, non-PIE pointer rebasing, filesystem invalidation, and unknown-number behavior. Benchmark syscall-heavy open/read/write/futex/clock/epoll workloads and report branch/instruction counts, not only wall time.
