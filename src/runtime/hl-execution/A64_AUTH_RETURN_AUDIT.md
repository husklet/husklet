# AArch64 authenticated-return audit

## Retained oracle

The read-only oracle files studied were
`/Users/x/dd/engine/src/translator/guest/aarch64/translate.c`, entry function
`trans`, and `/Users/x/dd/engine/src/translator/guest/aarch64/interp.c`, entry
function `interp_a64`. The translator owns trace construction and terminates a
trace at RETAA/RETAB, lowering either encoding to the same shadow-stack or
indirect-return path as `ret x30`. It lowers PAC/AUT architectural hints to NOP
so `x30` remains unsigned. The interpreter owns one CPU for the duration of the
fallback call, performs branch-register validation before mutating it, publishes
the target and `R_BRANCH` together, and has no lock, blocking, cancellation,
partial-result, host, or architecture branch within this operation. Its older
branch-register validator rejects the authenticated forms as undefined; that is
a cold-path divergence from the translator rather than the intended policy.

The retained approximation does not model keys, signing, authentication failure,
or stack-pointer modifiers. That limitation is preserved explicitly: the engine
does not advertise pointer authentication, PAC/AUT hints are NOPs, and RETAA or
RETAB reads the unsigned `x30` value without changing registers or flags.

## Rust and native ownership

| Capability | Owner | Status |
|---|---|---|
| PAC/AUT hint admission | `hl-execution` AArch64 system decoder | implemented as NOP |
| RETAA/RETAB cold decode | `hl-execution` AArch64 integer decoder | implemented as `Return { source: 30 }` |
| Target alignment/fault staging | `hl-execution` AArch64 interpreter | shared with ordinary returns |
| Native target extraction and IBTC miss | `src/native/exec` AArch64 indirect emitter | implemented using guest `x30` |
| Native trace termination | `src/native/exec` AArch64 trace effects | implemented for both encodings |
| Key/SP authentication | no owner | intentionally absent oracle approximation |

The cold decoder maps only the two allocated encodings selected by
`word & 0xffff_fbff == 0xd65f_0bff`; other pointer-authenticated branch and call
forms remain reserved. Both cold and native paths therefore share ordinary
return target, IBTC, control-exit, and fault behavior without adding a distinct
authentication state or lifetime.
