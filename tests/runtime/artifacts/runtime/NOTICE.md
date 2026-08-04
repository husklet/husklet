# Pinned dynamic-rootfs closure

These four unmodified GNU C Library runtime binaries are the minimal closure
needed by the retained `nonpie-dladdr` guests. The AArch64 files are from Ubuntu
`glibc` source version `2.43-2ubuntu2`; the x86-64 cross files are from
`cross-toolchain-base` version `80ubuntu3` (`libc6-amd64-cross`
`2.43-2ubuntu2cross1`). They are redistributed under LGPL-2.1-or-later; the full
license is retained as `COPYING.LIB`. Corresponding source packages are
available from Ubuntu's package archive, and recipients may replace these
shared libraries with compatible modified builds. `manifest.tsv` records exact
origin, license, mode, size, and SHA-256 identity. No host sysroot is consulted
during test execution.

Total retained binary size is 4,423,312 bytes.
