# File bind audit

Retained oracle tree studied: `../engine` at the read-only revision present on
2026-08-04. The relevant implementation is
`src/core/target/aarch64.c::container_init`,
`src/linux_abi/container/vfs.c::{add_vol,jail_match,secure_resolve_probe}`, and
`src/linux_abi/container/vfs/overlay.c::{name_bind_pick,overlay_lookup}`.

The oracle parses every bind before guest execution. Directory sources retain a
pinned directory descriptor. A non-directory source instead retains its
canonical path plus a pinned descriptor for its parent, matches only the exact
guest mount point, and opens the host leaf relative to that parent. The volume
slot owns access policy and remains append-only until teardown; read-only binds
reject write-intent operations with `EROFS`. Bind lookup precedes overlay lookup,
so a mounted file is neither copied up nor hidden by a lower-layer entry. Path
normalization happens before mount selection, and a file mount cannot acquire
children. The AArch64 and x86-64 targets share this VFS implementation; the host
branches are limited to native descriptor/path mechanisms.

Rust ownership is split between `hl-container::engine::Spec`, which serializes
resolved mounts, and `hl-engine::path::OrdinaryContext`, which owns the bounded
live projection. Directory mounts continue through `HL_VOLUMES` and
`MountNamespace`. Exact regular-file mounts use the existing bounded
`HL_NAME_BINDS` wire and retain their canonical host file, pinned parent, exact
guest path, and read-only state. Lookup requires exact `GuestPath` equality;
basename aliases remain a separate dynamic-library compatibility mechanism.
The open path projects the guest identity while opening the pinned host leaf and
rejects write intent only for a read-only projection.

Remaining differences are explicit: the Rust serializer currently admits
regular files and directories, not the oracle's socket/FIFO/device file-mount
family; unmount lifecycle and mount-table reporting remain owned by the broader
mount migration. Rootfs overlay copy-up is also absent and is not disguised by
this projection.
