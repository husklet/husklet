# Bounded backedge lifecycle evidence

`backedge-signal-bound` deliberately stays in a generated conditional backedge
until a one-second `SIGALRM`; its handler exits 42 so diagnostics settle normally.
Completion under the three-second
case timeout proves that direct in-body chaining reaches the dispatcher signal
safepoint; changing or removing the generated budget escape makes this fixture
time out.  QEMU is excluded only because it cannot referee Husklet's container
stop API.

The existing `runtime/memory/aarch64-smc-targeted` fixture supplies the SMC
half of the contract. Its generated `subs; b.ne` backedge is executed, rewritten
from `add #1` to `add #2`, explicitly published with `ic ivau`/`isb`, and must
change its result from 2000 to 4000. The existing
`runtime/checkpoint-translated/two-container-cycles` fixture supplies the
checkpoint half: three product generations must reach their marker and preserve
the guest state protocol across two stop/restore cycles.

## AArch64-on-x86 three-source family selection

Measured 2026-09-07 on the physical x86_64 Linux host at `870556641`, using
QEMU's per-instruction plugin against the exact static `int_a64`, `stress_a64`,
and `vec_a64` guests. All three stdout hashes matched the prior native/QEMU and
engine runs. The `instruction >> 21 == 0x4d8` three-source prefix retired
601,648 + 106,521 + 294,922 = 1,003,091 times. This is larger than the next
unsupported coherent candidates (direct calls plus returns, and atomics), so
the count selected the family before implementation. The immutable receipts
are under `/var/tmp/a64-next-census-870556641`; their form-record SHA-256 values
are `e7152b1ab67bab586db09e4618ca7e8ced114423329911fde1a47c959d260681`,
`b647c78148998c285bd465c84cda9b15e302be0d2e8c4b1e27da6f1f4e2ade6d`,
and `5bdc17e6f12974ab27ebe6c48833508cbb2ee9ed4ec2e8cd88ac715c0f640520`.

The production ownership audit covered the complete integrated
`interp/integer/register.c::interp_exec_dp_register_arithmetic` three-source
decoder, `dbt_x86_64.c::translate_block` and its canonical CPU-record emitters,
and `host/x86_asm.h`'s bounds-checked byte emitter. The decoder owns allocated
`op31/o0/sf` combinations and 32/64-bit, signed/unsigned, high-half, ZR, and Ra
semantics. Generated code owns no independent architectural state: x0..x30 are
loaded from and committed to the canonical `struct cpu`, NZCV is unchanged,
and every signal, fault, syscall, and checkpoint boundary therefore observes
the same record. Translation cache publication remains the existing `map_put`
transaction; overflow or an unallocated encoding abandons the whole prefix,
and fork/checkpoint generation invalidation remains owned by that cache.
There is no family-local heap object, lock, teardown, blocking operation,
partial result, errno, or host-specific path beyond the Linux/x86_64 DBT
selection itself. The retired `../engine` oracle is absent on this host, so no
claim in this lane relies on an unavailable historical checkout.

`three-source-multiply` checks every allocated form before continuing, including
overflow, signed and unsigned widening, both high halves, Ra, and ZR. The
backedge signal fixture executes MADD inside the bounded translated loop, and
the existing two-cycle daily-development fixture supplies checkpoint/rebuild
coverage across naturally generated multiply-heavy code.
