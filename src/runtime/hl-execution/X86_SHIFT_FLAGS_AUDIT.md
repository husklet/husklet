# x86 shift flag audit

Audited at Husklet `f93ff96850099f2c3d813ed80f3cfb632cb41870` against the
read-only retained engine `../engine` on 2026-08-04.

## Trigger

The `x86_strsearch_arithmetic_loop_matches_interpreter_at_each_boundary`
differential first diverged after guest instruction nine, ending at
`0x40273f`. The bytes are `48 c1 e1 11`, or `shl rcx, 17`. Native flags were
`0x005`; interpreter flags were `0x805`; the sole difference was OF (`0x800`).

OF is defined for SHL, SHR, and SAR only when the masked count is one. At a
masked count of 17 it is undefined, so the differential exposed inconsistent
materialization policy rather than a wrong defined architectural result.

## Retained C domain

The complete scalar group-two shift paths inspected were:

- `../engine/src/translator/guest/x86_64/lower/shift.c`, entry
  `hl_x86_lower_shift`: optimized translator for immediate, one, and CL counts,
  all operand widths, registers and memory, SHL/SHR/SAR and rotations;
- `../engine/src/translator/guest/x86_64/interp.c`, entry `interp_shift` and its
  group-two decode caller: fallback execution and flag materialization;
- `../engine/src/translator/guest/x86_64/translate.c`, group-two dispatch and
  NZCV helpers used by `hl_x86_lower_shift`;
- `../engine/src/translator/guest/x86_64/lower/trace.c`, shift flag liveness
  handling across translated-block edges.

The retained optimized translator owns live ARM NZCV plus the CPU membank NZCV
and parity lane. It eagerly materializes CF, SF, ZF, and PF, leaves AF
untouched, changes no flags for a zero masked count, and changes OF only for a
one-bit shift. Block exits spill canonical flag state; stitched and chained
edges reload or synchronize it. No lock or independent heap lifetime is
involved: flags are task-local CPU state and survive until the next flag writer,
signal/checkpoint capture, or task teardown.

The retained fallback `interp_shift` computes an OF value even for multi-bit
shifts despite its adjacent comment claiming undefined flags match the JIT.
That fallback detail diverges from the retained optimized translator and from
the architectural defined-mask contract. It is evidence to preserve, not the
policy to copy into the Rust domain.

## Rust capability matrix

| Capability | Rust interpreter | Rust native translator | Result |
|---|---|---|---|
| Mask count to 5/6 bits | implemented | implemented | aligned |
| Zero count preserves all flags | implemented | implemented | aligned |
| CF for nonzero count | implemented | implemented | aligned for supported widths/forms |
| SF/ZF/PF for nonzero shift | implemented | implemented | aligned |
| AF undefined and not overwritten | implemented | implemented | aligned |
| OF defined for count one | implemented | implemented | aligned |
| OF undefined for count greater than one | marked both defined and undefined, then overwritten | preserved | **divergent** |
| Register/memory and 8/16-bit forms | implemented | unsupported/fallback for part of the family | remaining native coverage gap, unrelated to this mismatch |

The Rust owner is `hl-execution::x86::flags::Arithmetic`. `FlagUpdate` already
separates `defined` from `undefined`, and `FlagState::apply` overwrites only the
defined mask. `Arithmetic::double_shift` implements the correct disjoint-mask
shape. Scalar `Arithmetic::shift`, however, included OF unconditionally in
`defined` while also including it in `undefined` for counts other than one.

## Correction and fail-first cohort

The correction makes scalar shift OF defined only for a masked count of one.
It does not choose a value for architecturally undefined output; it preserves
the task's prior OF, matching the native translator and the existing
`FlagUpdate` ownership contract.

The focused cohort covers SHL, SHR, and SAR across byte, word, dword, and qword
widths. For a count of two it requires disjoint masks and preservation of both
clear and set prior OF. For a count of one it requires OF to be defined and not
undefined. The existing strsearch boundary differential is the integration
acceptance case; it should proceed past `0x40273f` after this correction.

This audit does not claim the whole group-two native family complete. Native
memory and narrow forms still fall back by design and require a separate full
domain port before they can be called native-complete.
