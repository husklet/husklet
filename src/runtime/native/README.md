# Native runtime

This directory is the in-repository source of Husklet's production C engine.
`hl-engine` compiles it directly; builds must not read or link `../engine` or any
other sibling checkout.

The `retained/` subtree is the integrated production closure behind Husklet's
container, direct-worker, and GUI APIs. Production selection is C-only and
fails closed; there is no Rust execution fallback. The compiled host closure
covers Linux/AArch64 and macOS/AArch64, and the product workers select both
AArch64 and x86-64 guests. The Rust boundary in
`src/containers/hl-engine/src/c_execution.rs` validates the launch plan and
calls `hl_c_backend_create`/`hl_c_backend_run`; execution, Linux ABI service,
translation, and guest scheduling remain inside this C closure.

The pinned, source-by-source expansion ledger is
[`ORACLE_IMPORT_MANIFEST.tsv`](ORACLE_IMPORT_MANIFEST.tsv); sequencing,
ownership boundaries, conflicts, and the completed import tranches are in
[`ORACLE_IMPORT_PLAN.md`](ORACLE_IMPORT_PLAN.md).

`../engine` is a read-only behavior and performance oracle during that work.  It
is never a source dependency.  Source inventories in `retained/` are part of the
build contract: adding or removing a translation unit requires updating the
inventory and its parity tests.

Rust packages elsewhere under `src/runtime/` continue to own product services
that have not moved into C.  This directory contains C only; Cargo explicitly
excludes it from the `src/runtime/*` workspace member glob.

Current product-boundary measurements use `testing product-ab`: it stages and
hashes completed product workers, verifies exact output, alternates explicit-C
and default-C order, and writes a new ledger for every run. Direct comparisons
with preserved original/retained C artifacts use `testing ab` with a prior
same-binary null-arm ledger. The Rust-vs-C results in `exec/PERFORMANCE.md` and
related audit notes are historical investigations, not current product
baselines.
