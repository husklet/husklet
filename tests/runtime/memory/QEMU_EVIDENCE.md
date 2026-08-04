# QEMU oracle evidence

All eight declared cross-builds succeeded. `vector-pipe` matched its golden on
both ISAs. QEMU diverged for `mlockall-scope` (exit 0 but 161 different stdout
bytes), `vector-limits` (exit 1, 29 bytes), and `vector-order` (exit 1, 41
bytes) on both ISAs. Those host-policy cases remain typed broken.
