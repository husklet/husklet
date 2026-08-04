# AArch64 instruction-lowering regression corpus

`isa_regress.c` pins the translator divergences found by the differential AArch64 ISA fuzzer
(`tests/fuzz/isa/aarch64`), which runs the same static binary natively on the ARM64 host and under
the engine. The golden output is the native host's, so any regression is an exact stdout mismatch.

Built `-static -no-pie` deliberately: a non-PIE ET_EXEC arms `g_nonpie_lo`, and both pinned bugs
live on the paths that only exist then -- the ldxr/stxr -> LSE atomic upgrade and the low-address
bias fold.

Run with `make compat-isa-aarch64`.
