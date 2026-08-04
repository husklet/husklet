# Workload migration evidence

The folder contains all 21 retained logical cases and all 32 registered
case/ISA rows. Every golden and all source content are copied from the retained
tree. One trailing space in `ibtc_dispatch.c` was removed to satisfy the
repository whitespace gate. That file is intentionally sourced from the retained
sibling ABI folder because the workload manifest declares that ownership. Leaf
renames otherwise leave the fixture bytes unchanged.

The YAML preserves each stable case name, target set, compiler/linker flags,
environment, exit status, 120-second deadline, and stdout contract. The
`dbserver` and `sqlite` rows remain AArch64-only and retain static SQLite
linkage. Multi-thread and multi-process behavior stays inside the guest binary;
no host fixture is split out.

On 2026-08-04 the folder was loaded by the repository runner and checked with
its 18-worker bounded oracle path. All 18 AArch64 rows and all 14 AMD64 rows
cross-compiled with their declared flags, exited with the declared status, and
matched stdout byte-for-byte: 32/32 oracle rows passed. The two SQLite-linked
AArch64 sources produced only the retained static-linker warnings.

A fixture oracle pass is not a production-runtime pass. Engine results must
separately prove typed native selection through `HL_COMPAT_ENGINE_OPTIONS`
diagnostics.
