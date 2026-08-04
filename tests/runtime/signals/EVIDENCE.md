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

On 2026-08-04, the expanded manifest parsed as 75 logical cases and 143
case/ISA rows. All 143 rows cross-compiled in parallel with their declared
compiler and exact case flags. Every one of the 75 golden files is referenced;
all 76 C files are either a declared build source or one of the three
`fd_main.c` leaves included for the separate signalfd entry points. The 71
migrated libc sources and goldens compare byte-for-byte with their retained
paths at the migration parent revision.

An integrated 18-worker run selected typed native execution and emitted both
`hl-native` diagnostic streams, then aborted with stack smashing before it
could write a result ledger. This is runtime compatibility evidence, not a
fixture parse or build failure. `rt-signal-order` remains typed broken for the
known x86-64 ordering defect. `sigurg-go-preempt` is also typed broken: its
retained golden requires executable-identity suppression that Husklet's engine
policy explicitly forbids. The generic `sigurg-preempt` case remains active.
