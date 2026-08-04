# Container copy oracle audit

The four `cpcmd` cases exercise the typed `hl-container` filesystem surface,
not an ambient Docker shell. The retained implementation studied before this
move was:

- `../engine/src/linux_abi/fdcache.c`: `fsgen_bind`, `fsgen_flush`,
  `hl_fdcache_generation_poll`, `hl_fdcache_resolution_bump`, and
  `hl_fdcache_reset`;
- `../engine/src/linux_abi/syscall/dispatch.c`: the pre-dispatch generation
  poll and namespace-mutation invalidation calls;
- `../engine/src/core/launch.c`: filesystem-generation binding propagation.

The daemon owns one fixed-width generation file for a container. Every engine
maps that file and owns its last-seen generation plus its descriptor and path
caches. An external extraction completes before publishing a release increment.
The next guest syscall observes the change, performs an acquire load, and drops
the affected caches before dispatch. Guest namespace mutation bumps the shared
resolution epoch immediately. The cache mutex protects threaded mutation;
fork/chroot reset inherited caches, and rebinding releases the prior mapping.
POSIX hosts map the generation file directly; the fork-local fallback has the
host-specific branch.

Rust owns archive validation, bounded extraction, mount routing, and generation
publication in `hl-container::Filesystem::{extract,archive}` and
`Containers::filesystem`. The testing application owns only bounded translation
between repository files and those typed calls. Each case has one isolated
container and temporary host output, and cleanup remains unconditional. The
matrix covers file and directory transfer in both directions; live cache
coherence remains covered by the separate `filesystem_coherence` public
contract.
