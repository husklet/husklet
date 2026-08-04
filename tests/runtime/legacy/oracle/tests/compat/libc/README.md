# libc compatibility

This suite is the byte-preserving C transfer of every guest in the legacy
`ext_libc` group. Each source is built as a static Linux binary for AArch64
and x86-64, run through the matching production engine, and checked against
the exact output in `expected/shared`. Portable cases must also produce
byte-identical output across both engines.

`manifest.tsv` is the authoritative inventory and runtime contract. The
suite has no native-oracle path and does not depend on the legacy Rust test
registration.
