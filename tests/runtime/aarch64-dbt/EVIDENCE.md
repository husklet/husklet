# Bounded backedge lifecycle evidence

`backedge-signal-bound` deliberately stays in a generated conditional backedge
until the container lifecycle sends `SIGTERM`; its handler exits 42 so diagnostics
settle normally. Completion under the three-second
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
