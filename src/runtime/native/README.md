# Native runtime

This directory is the in-repository source of Husklet's production C engine.
`hl-engine` compiles it directly; builds must not read or link `../engine` or any
other sibling checkout.

The `retained/` subtree is the integrated production closure behind Husklet's
container, direct-worker, and GUI APIs. Production selection is C-only and
fails closed; there is no Rust execution fallback. The compiled host closure
covers Linux/AArch64 and macOS/AArch64. Both AArch64 and x86-64 guest translator
sources are inventoried, but only the AArch64 guest target is production-selected
until the x86 boundary passes its independent compatibility and performance gates.

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
