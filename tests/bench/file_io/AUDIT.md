# File I/O benchmark evidence

The fixture keeps setup outside each typed phase and reports a stable checksum.
The three cases use one warning-strict static PIE artifact and differ only by
the selected operation.

Evidence recorded on 2026-08-04:

- Husklet source: `2ad572f494673ff0b0cf1fd4814612b89a820729`
- retained C source: `7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`
- ARM64 artifact SHA-256: `85b31cc1fac3a324494a39daac4f4221b4bd5e54593e0e02827b24f10d47e212`
- testing runner SHA-256: `d22905e239e8bb0da6e7b2dd5395e8853b3d84efa000d509cfe8be55eca7e303`

Host-native Linux, QEMU ARM64, and the retained C engine each returned status
zero for every independent case with checksums `450000` (scalar), `147000`
(vector), and `9792000` (mapping). The current Rust engine returned status one
before guest output for all three cases. Therefore the earlier combined-run
status 135 is not evidence of a mapping-specific fault: the Rust failure occurs
before the operation-specific phase and remains a provider/startup blocker.

The mapping control truncates the file to one MiB before `mmap`, touches only
the first byte of each 4096-byte page, synchronizes before unmapping, and closes
the descriptor last. No fixture out-of-bounds access or lifetime violation was
found.
