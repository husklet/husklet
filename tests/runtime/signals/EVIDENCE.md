# Signals migration evidence

On 2026-08-03, the folder-owned sources were compiled as four static binaries
per ISA, eight artifacts total, with the YAML-declared AArch64 and x86-64
cross-compilers and flags. The delivery-frame source produces one artifact;
the byte-exact signalfd leaf payloads are linked through `fd_main.c` into three
separately named artifacts so each YAML case retains its argv contract.
`qemu-aarch64` and `qemu-x86_64` each ran `delivery-frame`, `edges`, `epoll`,
and `fork` with a 30-second bound. All eight executions exited successfully
and their stdout matched the checked-in golden files byte for byte. `file`
identified all eight artifacts as statically linked ELF executables for ARM
aarch64 or x86-64; the freestanding signal-frame binaries had no build ID.

Fresh integrated evidence from the current shared tree passed all eight rows:
`target/debug/testing runtime signals --jobs 2` reported 8 passed and 0 failed
across both guest ISAs. Every row used typed native execution with diagnostics;
the emitted `hl-native` and `hl-native-detail` records prove native activation.
