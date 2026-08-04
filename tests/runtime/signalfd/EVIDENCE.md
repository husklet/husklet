# Signalfd migration evidence

On 2026-08-03, all three folder-owned sources were compiled as one static
binary with the YAML-declared AArch64 and x86-64 cross-compilers and flags.
`qemu-aarch64` and `qemu-x86_64` each ran `edges`, `epoll`, and `fork` with a
30-second bound. All six executions exited successfully and their stdout
matched the checked-in golden files byte for byte. `file` identified the two
artifacts as statically linked ELF executables for ARM aarch64 and x86-64.

The integrated runtime runner is temporarily blocked outside this folder:
`cargo check -p testing` fails in `src/apps/testing/src/benchmark.rs` because
the names `repeats` and `timeout` are unresolved. The in-progress shared
runtime builder also passes its one declared source path twice to the compiler.
Until the shared runner owners finish those changes, these cases remain typed
`broken` rather than claiming engine evidence from QEMU oracle results alone.
