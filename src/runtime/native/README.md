# Native runtime

This directory is the in-repository source of Husklet's production C engine.
`hl-engine` compiles it directly; builds must not read or link `../engine` or any
other sibling checkout.

The `retained/` subtree is the integrated production closure behind Husklet's
container, direct-worker, and GUI APIs. Production selection is C-only and
fails closed; there is no Rust execution fallback. The compiled host closure
covers Linux/AArch64 and macOS/AArch64, and the product workers select both
AArch64 and x86-64 guests. The Rust boundary in
`src/containers/hl-engine/src/execution/` validates and serializes the launch
plan, supervises the worker, and calls `hl_c_backend_create`/
`hl_c_backend_run` through `execution/ffi/`; execution, Linux ABI service,
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

The Rust loader classifies `ET_EXEC` and `ET_DYN` images and sends a bounded
main-image placement plan to the retained loader. For displaced `ET_EXEC`, the
shared `address_projection` boundary maps canonical guest link addresses to
their storage interval and back. x86 translated control flow now keeps PCs and
return addresses canonical-low and performs projection only at execution or
memory access; it does not inspect Go metadata or V8 symbols. PIE and static PIE
continue through the ordinary `ET_DYN` path.

`src/apps/testing/tests/displaced_et_exec_linux.rs` forces displacement for
both guest ISAs, requires a reported nonzero bias, and verifies low PC/data
identity, static pointers, direct and indirect calls, and exact syscall output.
This proves the generic projection path under forced displacement; it does not
claim that every third-party non-PIE workload has completed compatibility
coverage.

Build workers from the repository root with `make engine`. On Linux,
`target/release/hl-aarch64 --backend-receipt` and
`target/release/hl-x86_64 --backend-receipt` emit a hash-bound JSON receipt only
after the production selector constructs the `retained-c` backend.

The C quality entry points are `make lint-c`, `make fmt-c-check`, and
`make fmt-c`. `make lint-c` checks the source manifest, builds every retained
compile group with strict warnings, runs linter contract tests, and invokes
clang-format, clang-tidy, cppcheck, and deterministic repository policies. It
is also part of `make gate`.

Current product-boundary measurements use `testing product-ab`: it stages and
hashes completed product workers, verifies exact output, alternates explicit-C
and default-C order, and writes a new ledger for every run. Direct comparisons
with preserved original/retained C artifacts use `testing ab` with a prior
same-binary null-arm ledger. The Rust-vs-C results in `exec/PERFORMANCE.md` and
related audit notes are historical investigations, not current product
baselines.
