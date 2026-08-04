# Memory compatibility corpus

This directory contains the complete `ext_mem` and `ext_mm` Linux guest corpus: twelve executable C cases
and the shared `memrss.h` support header. Sources are byte-identical to their legacy originals. The manifest
records the former Rust registrations from `memory.rs` and `memx.rs`; cases that formerly depended on a
native Linux oracle now use reviewed, checked-in normalized output.

The matrix compiles every case as a static Linux binary for AArch64 and x86_64, launches both production
engines through the ABI4 configuration protocol, compares stdout and exit status to the exact golden, and
requires cross-ISA stdout equality. RSS diagnostics intentionally remain on stderr; the stable behavioral
contract is the bounded verdict on stdout.
