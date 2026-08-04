# POSIX compatibility

This suite is the exact C-source transfer of the legacy `ext_posix` guest group.
`manifest.tsv` is authoritative: it records every Rust-registered case, original
path, ISA scope, build contract, expected exit, dependency, and checked-in
byte-exact stdout. Both applicable Linux guest engines must match the golden;
cases marked for both ISAs must also match each other byte-for-byte.

The suite never runs a native oracle. Its expected files encode deterministic
behavioral requirements, not host-specific observations. PTY/devpts cases test
Linux guest semantics; `pty-devpts-ls` retains its legacy AArch64/rootfs scope.
