# Filesystem compatibility corpus

The corpus includes the byte-preserved legacy `ext_fs`, `ext_fsx`, `dentry`, and `pcachex` guests. `manifest.tsv` is
the authoritative ABI4 registration and records each original path, ISA scope, build flags, rootfs
dependency, exit status, and deterministic checked-in golden. The four storage cases retain their
Alpine-overlay execution contract; the remaining Linux and portable verdict cases run bare on both
production guest ISAs. Stateful shared-memory, FIFO, xattr, and temporary-path cases clean up their
resources and are safe to soak repeatedly.

`dentry/storm.c` retains its Alpine-rootfs contract and performs 200 mutation/lookup iterations per run.
The five `pcachex` cases use ABI4's typed translation-cache directory, intentionally retaining state across
matrix invocations so cold-save and warm-restore runs must produce the same checked-in goldens.
