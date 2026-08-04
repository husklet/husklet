# x86 CMPS/SCAS oracle audit

This audit covers the retained compare/scan string-instruction domain and the
Rust-owned native execution fast path.

## Retained implementation studied

- `../engine/src/translator/guest/x86_64/lower/repstr.c`,
  `hl_x86_lower_repstr`: decodes A6/A7/AE/AF, operand widths, F2/F3 repetition,
  and static or runtime DF into the `R_REPSTR` descriptor.
- `../engine/src/translator/guest/x86_64/rep.c`,
  `hl_x86_rep_compare`: owns the complete operation, including zero count, bare
  forms, forward byte `memcmp`/`memchr` acceleration, typed wider loops,
  backward traversal, final compare flags, and RCX/RSI/RDI writeback.
- `../engine/src/translator/guest/x86_64/interp.c`, compare/scan dispatch, and
  `interp_dispatch.h` / `dispatch.h`, `R_REPSTR` handling: the interpreter and
  translated dispatcher use the same helper and resume after its single
  synchronous call.
- `../engine/src/translator/guest/x86_64/cpu.h`, string state fields, and
  `decode.c`: register/DF ownership and opcode admission.
- `../engine/tests/unit/test_rep.c`, `tests/compat/completeness/x86_64/repstring.c`,
  and `tests/compat/core/regress/repcmps_nopie.c`: zero count, forward/backward
  termination, low-address rebasing, and compatibility coverage.

The retained CPU object owns registers, flags, DF, and the temporary descriptor
for one engine task. The synchronous helper borrows that object for one dispatch
and retains no pointer or allocation afterward. There are no locks, cancellation
points, teardown transitions, syscalls, or errno results in this domain. Host
memory is dereferenced directly; consequently retained partial host faults are
not resumable inside the helper. The translator is an AArch64-host implementation
of x86-64 guest semantics. Forward byte operations have libc acceleration;
DF and wider paths use bounded element loops. Non-PIE host bias is applied only
to dereferenced addresses while guest registers retain guest-visible values.

## Capability mapping

| Capability | Retained owner | Rust owner | Status |
|---|---|---|---|
| A6/A7/AE/AF and byte/word/dword/qword | `hl_x86_lower_repstr` | `run.c::rep_decode` | implemented |
| Bare, REPE, REPNE, zero count | `hl_x86_rep_compare` | `run.c::rep_execute` | implemented |
| DF forward/backward pointer updates | descriptor + helper | CPU RFLAGS DF + `rep_execute` | implemented |
| Final SUB flags | `hl_x86_sub_nzcv` | `rep_sub_flags` | implemented |
| Stop at middle, last, or exhaustion | helper loops | authenticated element loop | implemented |
| 32-bit address size | scalar lowering gap | fast path zero-extension | implemented in Rust |
| FS/GS CMPS source override | scalar lowering gap | checked segment-base address | implemented in Rust |
| Split projection boundary | direct host span | repeated authenticated chunks | implemented in Rust |
| Partial fault and resume | host-fault gap | operand resolver with committed progress | implemented in Rust |
| Forward libc byte acceleration | `memcmp` / `memchr` | bounded loop | divergent performance gap |
| Non-PIE low-link rebasing | global bias tuple | projection mapping | implemented by different ownership |

`test/x86_rep.c::compare_contract` is the focused differential matrix. It covers
all four widths and bare/F2/F3 forms, DF in both directions, termination at the
middle/last/none, zero count, split projection boundaries, addr32 with FS and GS,
and a partial fault followed by resolver-backed resume. Rust projection leases
replace retained global non-PIE bias and authenticate each dereference. The only
remaining domain gap is a vectorized/libc forward-byte search optimization; it
does not affect architectural state or acceptance semantics.
