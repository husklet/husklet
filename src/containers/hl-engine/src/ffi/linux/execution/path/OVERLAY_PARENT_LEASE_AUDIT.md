# Overlay parent lease audit

The retained C oracle was read from
`../engine/src/linux_abi/container/vfs/overlay.c`, specifically
`overlay_lookup_raw`, `overlay_resolve`, `overlay_copyup`, and
`overlay_copyup_tree`.

The C implementation identifies a path by its guest name, selects the first
visible upper or lower inode for observation, and always relocates mutation to
the writable upper. `overlay_copyup` materializes missing upper ancestors,
copies a lower regular file and its metadata, removes an upper whiteout when
recreating a name, and bumps the resolution epoch after relocation.
`overlay_copyup_tree` extends that ownership rule to directory rename. Lower
layers remain immutable throughout. Bind mounts bypass this union and retain
their own jail lifetime.

The Rust path pin layer previously represented a resolved parent only as one
host descriptor. That loses both the selected layer and the guest identity, so
a later mutation cannot distinguish a lower observation from a writable upper
target without reconstructing identity from a host path.

The first resolver slice now keeps ordinary single-root and mounted walks on
the existing direct-descriptor fast path. Only a layered root allocates a
registry handle. Each layered handle owns the normalized guest byte path and
an ordered set of `(layer, descriptor)` directory candidates. Child inspection
probes upper before lower, retains lower directory candidates when an upper
directory also exists, and stops at upper whiteout and opaque markers. This
allows a walk through an upper directory to select a lower-only descendant
without mutating or path-reconstructing the lower. The split deliberately
avoids imposing a registry lock on ordinary path resolution.

`ParentLease` records the normalized guest parent, the layer that supplied the
observed inode, and independently owned selected and upper capabilities. An
upper selection uses the same capability for observation and mutation. A lower
selection continues to observe the lower but exposes only its paired upper
capability as the mutation target. Copy-up, whiteouts, metadata preservation,
directory merging, cache epochs, and mount routing remain outside this focused
lease slice and must be implemented by their owning overlay resolver and
mutation domains.

The production boundary is not wired yet: `OrdinaryContext` still constructs
one root, and the launch composition still ignores `HL_LOWER` and
`HL_OVERLAY_WORK`. The layered registry and `ParentLease` must next be joined so
`VfsHost::duplicate_parent` returns the guest/layer-aware lease, copy-up creates
missing upper ancestors before publication, and read-directory consumes all
retained directory candidates. Until then the container must continue using
its durable lower-root fallback; changing rootfs to the empty upper alone would
make existing images unbootable.
