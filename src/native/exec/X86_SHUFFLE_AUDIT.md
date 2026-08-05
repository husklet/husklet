# AMD64 legacy SSE shuffle audit

The retained implementation was inspected read-only at
`../engine/src/translator/guest/x86_64/translate.c` (`translate_one`, legacy
`0F 70` and `0F C6` cases), `interp.c` (the complete `PSHUFD`, `PSHUFHW`,
`PSHUFLW`, `SHUFPS`, and `SHUFPD` definitions), `avx.c` (the corresponding
non-destructive VEX ownership), `emit.c` (`e_ins_s`, `e_ins_d`, vector-copy
and broadcast emitters), and `lower/trace.c` (immediate-bearing instruction
length admission). The benchmark instruction sequence was checked against
`tests/perf/x86_flag_sse_diff.c` and the Rust-owned benchmark inventory in
`X86_PERFORMANCE.md`.

These operations own no allocation, locks, blocking, cancellation, signals,
errno, or teardown. The dispatcher owns the CPU and translated-block
lifetime. The instruction owns one immediate permutation and commits one XMM
destination only after any memory operand has passed the complete guarded read.
Register source/destination overlap is destructive but must read every selected
input lane before architectural commit. None of the family changes integer
flags or MXCSR. REX.R/REX.B extend XMM identities; address-size and segment
prefixes remain owned by the generic effective-address path. The retained
AArch64 path uses scratch vectors for overlap and general immediates, while the
x86 interpreter performs the same byte-exact permutation. There are no
host-specific branches beyond those two execution owners.

| Retained capability | Rust native owner | State |
|---|---|---|
| `66 0F 70 /r ib` PSHUFD, every immediate | frontend decode + vector emitter | implemented |
| `F2/F3 0F 70 /r ib` low/high word shuffle | frontend decode + vector emitter | implemented |
| `0F C6 /r ib` SHUFPS | frontend decode + vector emitter | implemented |
| `66 0F C6 /r ib` SHUFPD | frontend decode + vector emitter | implemented |
| register, destructive alias, REX XMM identity | scratch-first vector emitter | implemented |
| unaligned memory read and fault-before-commit | generic vector memory guard | implemented |
| truncated ModRM/address/immediate | frontend bounded decoder | implemented |
| VEX 128/256 non-destructive forms | no Rust native owner | remaining gap |

Focused tests admit every legacy prefix and width, register and displaced
memory forms, reject truncated immediates without advancing the guest PC, and
execute a general self-independent PSHUFD permutation on AArch64 while checking
source, flags, and MXCSR preservation. AMD64 quiet comparison is unavailable on
this AArch64 host and is not represented as equivalent evidence.
