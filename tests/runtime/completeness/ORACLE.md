# Completeness compatibility oracle audit

> **Ownership note:** The corpus contract remains current. References to
> `hl-execution` describe the deleted Rust replacement engine; production is
> `src/runtime/native/retained`.

This folder preserves the whole legacy `completeness` category as one selectable
compatibility domain: 184 cases backed by 184 registered byte-preserved C
sources and 185 byte-preserved stdout files.  The former category also contained
one unregistered source/golden pair, `x86_64/lddqu`; it remains byte-preserved as
migration evidence but is not invented as a selectable contract.  The manifest preserves each source, ISA, compiler
flags, exit value, and golden path. The migrated manifest uses
`-Itests/runtime/completeness`, the final folder's repository-relative include
root, for the cases that include `compat.h`; `compat.h` itself is byte-identical.

## Retained C implementation studied

The read-only implementation audit covered the complete dispatch owners, rather
than only the first failing fixture:

- `../engine/src/translator/guest/aarch64/translate.c` and `interp.c`, entered
  through the translator/interpreter dispatch tables, plus `dispatch.h`,
  `cache.c`, and `signal.c` (`hl_aarch64_signal_build`,
  `hl_aarch64_signal_restore`, `hl_aarch64_signal_capture`, and
  `hl_aarch64_signal_resume`).
- `../engine/src/translator/guest/x86_64/decode.c`, `translate.c`, `interp.c`,
  `legacy.c`, and the complete `lower/` family (`alu`, `crypto`, `mov`,
  `repstr`, `shift`, `sse4x`, and `x87`), together with `flags.c`, `avx.c`,
  `operand.c`, `cache.c`, and `signal.c` (`hl_x86_signal_build`,
  `hl_x86_signal_restore`, `hl_x86_signal_capture`, divide/trap raising, and
  fast-clock fault recovery).
- `../engine/src/linux_abi/syscall/dispatch.c` and every domain leaf used by
  this cohort: `aio.c`, `binding.c`, `event.c`, `fs.c`, `guest_copy.c`,
  `helpers.c`, `inotify.c`, `io.c`, `mem.c`, `misc.c`, `net.c`, `proc.c`,
  `ptrace.c`, `rare.c`, `signal.c`, `sysv.c`, and `time.c`; syscall-number
  normalization is owned by `../engine/src/linux_abi/number.c` at
  `hl_linux_syscall_number` and `hl_linux_syscall_guest_number`.

## Ownership and behavior

The retained translator owns CPU state for one guest task and advances it through
decoded/interpreted or published translated blocks.  Cache entries are keyed by
guest identity and configuration; publication and chaining occur only after code
and relocation state are complete.  AArch64 and x86-64 each own their register,
flags, vector, floating-point, and signal-frame layouts.  Signal capture restores
guest architectural state before returning to dispatch.  The Linux layer owns
canonical syscall numbering, bounded guest-copy import/export, descriptor and
process identity, blocking and wakeup state, and errno conversion.  Descriptor
table locks are not the lifetime of host calls; open-file descriptions retain
shared offsets while descriptor flags remain local.  Partial I/O is returned
before an interrupt error, blocking operations retain cancellation/wakeup order,
and guest pointer failures become `EFAULT` rather than a host fault.

Architecture branches are explicit: AArch64 AdvSIMD, crypto, LSE, FP16/BF16,
dot-product, and I8MM cases run only on ARM64; x86 flags, MMX/x87, SSE, AVX,
crypto, string, and trap cases run only on AMD64.  Syscall cases use canonical
numbers and run on both ISAs.  Host-specific filesystem/procfs/provider seams are
behind the Linux ABI adapters.  `getdents64` remains explicitly unsupported on
macOS because the synthetic procfs directory backend returns `open_ok=0`; the
case remains selectable and points here instead of disappearing from inventory.

## C-to-Rust capability matrix

| Retained capability | Rust owner | Migration state |
|---|---|---|
| AArch64 decode, scalar, AdvSIMD, crypto, atomics, FP state | `hl-execution::aarch64` and `src/runtime/native/exec` | implemented; cases retain byte-exact acceptance |
| x86 decode, flags, scalar, string, MMX/x87, SSE/AVX, crypto | `hl-execution::x86` and `src/runtime/native/exec` | implemented with compatibility cases covering edge flags and traps |
| translated-block publication, lookup, chaining, fault reconstruction | `src/runtime/native/exec` plus `hl-execution` | implemented; native diagnostics are required by this manifest |
| syscall normalization, errno, guest marshalling | `hl-linux` | implemented; rare syscall cases remain acceptance probes |
| descriptor, event, process, signal, memory, filesystem, procfs joins | owning runtime crates joined by `hl-runtime` | implemented or visibly divergent through a typed case status |
| macOS synthetic procfs directory enumeration | VFS/procfs host adapter | divergent; `getdents64` is typed unsupported with this evidence |

The QEMU oracle establishes architecture-visible output independently of the
Rust engine.  A passing folder run requires the declared exit value and exact
stdout for every selected active target; native execution must be enabled only
through the typed testing configuration and must emit its diagnostics proof.

## Migration evidence

The migrated tree was checked mechanically against `HEAD`: all C/header and
golden SHA-256 fingerprints match (zero mismatches), all 184 registered
source/golden references resolve, and all case IDs are unique.  An 18-worker
cross-build compiled all 275 case/target rows with zero failures.  An 18-worker,
120-second-bounded QEMU sweep produced 261 exact passes and the 14 divergent
rows below.  The 11 affected cases are conservatively typed `broken` case-wide;
this records evidence without debugging or normalizing an architecture result.
Hashes are the first twelve hexadecimal SHA-256 digits.

| Case | Target | Divergence | Actual | Golden |
|---|---|---|---|---|
| `arm64-bf16` | arm64 | stdout | `a89453580d32` | `71a89999c479` |
| `arm64-dc-zva-model` | arm64 | exit 134, expected 0 | `e3b0c44298fc` | `2409d5eeef50` |
| `process-vm` | arm64 | stdout | `5b05bd51aaff` | `062ce6f68bdf` |
| `process-vm` | amd64 | stdout | `5b05bd51aaff` | `062ce6f68bdf` |
| `cpu_discovery` | arm64 | stdout | `56bb6da1fea5` | `d79705203168` |
| `seccomp-filter` | arm64 | stdout | `591126306fe9` | `7bb239666a86` |
| `clone3` | arm64 | stdout | `7340df9c3525` | `67cdd1579832` |
| `clone3` | amd64 | stdout | `7340df9c3525` | `67cdd1579832` |
| `io-uring-enter` | arm64 | stdout | `d6ad57926e86` | `fad0e393a80c` |
| `io-uring-enter` | amd64 | stdout | `d6ad57926e86` | `fad0e393a80c` |
| `amd64-x87-stack-faults` | amd64 | stdout | `15271214fca0` | `93724fe75e13` |
| `amd64-x87-fprem-loop` | amd64 | stdout | `7f6f63fd99f6` | `a2b44c5d7c6a` |
| `amd64-x87-precision-rounding` | amd64 | stdout | `4aaabdc7529b` | `4f0f6807a094` |
| `amd64-denorm-flags` | amd64 | stdout | `99f391e9991f` | `7223d2f67a51` |

At validation time the shared testing binary did not reach fixture loading:
concurrent runner work derived `Debug` for `runtime::ledger::Row` while
`runtime::WorkKey` did not implement it.  This folder's YAML was therefore also
loaded independently with a strict YAML parser; it contains 184 cases, 275
target rows, 172 active cases, 11 broken cases, and one unsupported case.
