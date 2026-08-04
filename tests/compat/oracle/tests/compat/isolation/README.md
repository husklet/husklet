# Isolation compatibility suite

This suite transfers the seven legacy `ext_iso` guests byte-for-byte and records every bare-guest registration from `ext/isolation.rs`. Configuration variants run through ABI4 launch fields. Normalized outputs are checked exactly and across both production ISAs; image-rootfs shell registrations remain explicit external-service cases in `images.tsv`.

The original unconstrained `cpudefault.c` and `cpusysfs.c` registrations print the host's active CPU count, so their legacy oracle output cannot be a portable checked-in golden. They remain preserved and explicitly mapped as `excluded-replaced`; derived pure-C contract adapters assert the same cross-path agreement and multicore invariants with stable verdict output. Their configured `--cpus=2` variants still run the exact legacy sources.
