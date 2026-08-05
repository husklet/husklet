# X86 scalar-single conversion and compare oracle audit

Audited revision: `269b800687d5af9ae5446d46022eae0327c7fe09`.

## Retained implementation studied

- `../engine/src/translator/guest/x86_64/interp.c`: `interp_sse_prefix`,
  `interp_sse_rm_get`, `interp_fp_is_double`, `interp_fp_is_scalar`,
  `interp_fp_source_bytes`, `interp_fp_comis_flags`, `INTERP_FP_COMIS`, and
  `interp_step_sse_fp` cases `0x2a`, `0x2c`, `0x2d`, `0x2e`, and `0x2f`.
- `../engine/src/translator/guest/x86_64/translate.c`: the `0x2a`, `0x2c`/`0x2d`,
  and `0x2e`/`0x2f` lowering arms in `x86_translate`, plus
  `emit_fpsr_to_mxcsr` and `emit_mxcsr_to_fpsr`.
- `../engine/src/translator/guest/x86_64/x87state.c`: `hl_x87_state_save`
  and `hl_x87_state_restore`, which preserve the shared host FP exception and
  rounding projection used by SSE.

The retained interpreter has no separately owned `cpu->mxcsr`: on x86-64 the
host MXCSR owns rounding, DAZ/FTZ, and sticky exceptions for the lifetime of the
guest CPU execution interval; AArch64 projects those controls onto FPCR/FPSR.
Operand state is CPU-owned, and memory operands are fully read before the
destination register, RIP, EFLAGS, or exception state is committed. There is no
lock in this instruction-local path and no separately allocated resource to tear
down. `REX.W` selects a signed 64-bit integer source/destination; without it the
integer width is 32 bits. F3 selects the single scalar forms. `CVTSI2SS` obeys
MXCSR.RC and reports precision, while `CVTTSS2SI` always truncates and produces
the signed integer-indefinite bit pattern on NaN or overflow. `UCOMISS` raises
invalid only for a signaling NaN; `COMISS` raises it for any NaN. Both set
ZF/PF/CF to `000`, `001`, `100`, or `111` for greater, less, equal, or unordered
and clear OF/SF/AF. DAZ suppresses denormal-operand reporting and compares the
flushed zero. Memory failure precedes every architectural commit.

## Rust capability mapping

| Retained capability | Rust owner | Status |
|---|---|---|
| F3 and REX decode, register identity | `x86/scalar/mod.rs`, `conversion.rs` | implemented |
| signed 32/64 integer to scalar single, upper-lane merge | `Conversion::from_integer_execute` | implemented |
| scalar single to signed 32/64, truncation and indefinite | `Conversion::to_integer_execute` | implemented |
| MXCSR.RC, DAZ/FTZ, sticky exceptions | `CpuState::mxcsr`, `Arithmetic::environment`, `Arithmetic::exceptions` | implemented |
| COMISS/UCOMISS NaN and EFLAGS behavior | `Comparison::execute` | implemented |
| exact-width memory access and fault-before-commit | `Conversion::memory`, `Comparison::execute` staged CPU clones | implemented |
| host-specific FP projection | replaced by host-independent `hl-softfloat` state | intentionally different ownership, equivalent guest contract |

No missing exception-model prerequisite was found. The focused Rust tests cover
both integer widths, all relevant rounding/exception outcomes, EFLAGS preservation
or production, mandatory-prefix forms, and atomic memory faults.
