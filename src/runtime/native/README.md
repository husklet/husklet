# Native runtime

This directory is the in-repository source of Husklet's production C engine.
`hl-engine` compiles it directly; builds must not read or link `../engine` or any
other sibling checkout.

The current `retained/` subtree is the first, already-integrated C closure.  It
is intentionally kept behind Husklet's existing container and worker APIs while
the broader standalone engine is audited and imported subsystem by subsystem.
The transition is complete only when the in-tree C engine supports the required
architectures and compatibility corpus, satisfies the performance gate, and no
production selector can fall back to the retired Rust execution engine.

`../engine` is a read-only behavior and performance oracle during that work.  It
is never a source dependency.  Source inventories in `retained/` are part of the
build contract: adding or removing a translation unit requires updating the
inventory and its parity tests.

Rust packages elsewhere under `src/runtime/` continue to own product services
that have not moved into C.  This directory contains C only; Cargo explicitly
excludes it from the `src/runtime/*` workspace member glob.
