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

`ParentLease` records the normalized guest parent, the layer that supplied the
observed inode, and independently owned selected and upper capabilities. An
upper selection uses the same capability for observation and mutation. A lower
selection continues to observe the lower but exposes only its paired upper
capability as the mutation target. Copy-up, whiteouts, metadata preservation,
directory merging, cache epochs, and mount routing remain outside this focused
lease slice and must be implemented by their owning overlay resolver and
mutation domains.
