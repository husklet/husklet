# Completeness compatibility suite

This directory contains pure-C Linux guests ported from the former Rust-driven
engine matrix. `manifest.tsv` is the authoritative case registry. There are 160
registered cases backed by 149 unique C sources; repeated execution modes never
duplicate a source.

Linux ABI cases are compiled for both guest ISAs. The AArch64 executable runs
through the AArch64 engine, the x86-64 executable through the x86-64 engine, and
the gate requires the declared exit status, a byte-exact checked-in stdout under
`expected/shared/`, and byte-identical output between engines. This is an engine
compatibility contract; it does not use a native executable as an oracle.
Architecture-specific opcode cases run only through their matching engine and
compare stdout with the checked-in ISA-specific file under `expected/`.

Sources are grouped by what they test, not by the host implementation:

- `aarch64/`: AArch64 instruction behavior.
- `x86_64/`: x86-64 instruction behavior.
- `syscall/`: Linux ABI behavior, normally compiled for both guest ISAs.

The byte-preserved `compat.h` is shared support material, not a standalone guest. Every syscall source
includes it through the Make dependency and the registered matrix therefore compiles and exercises the
header transitively; no synthetic header-only runtime case is introduced.

One source may back several manifest cases. Eleven environment-driven engine
mechanism variants are retained in the manifest as `excluded-replaced`; the
portable engine does not expose those private switches. Their observable
instruction or durability behavior remains covered by the active default case.
No source-text inspection test was ported or introduced.

Every source builds as a static GNU C11 Linux executable with the flags recorded
in the manifest. The syscall programs depend only on Linux libc, except that
the exec tests self-exec and the `pf-*` tests require procfs. Cross compilers may
need the paths used by the project toolchain instead of bare compiler names.
