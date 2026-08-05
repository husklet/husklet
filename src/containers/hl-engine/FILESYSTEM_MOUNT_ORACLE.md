# Filesystem mount oracle audit

This audit covers the mounted-path identity fix in `eda7c460b`. The retained C
engine was read only.

## Retained implementation studied

- `../engine/src/linux_abi/syscall/fs.c`: `svc_mount`, the syscall dispatcher
  cases for `mount` and `umount2`, and open/write read-only admission.
- `../engine/src/linux_abi/container/vfs.c`: `struct vol`, `add_vol`,
  `vol_mkmountpoint`, `rt_add_vol`, `rt_del_vol`, `jail_match`, and `jail_pick`.

The C engine owns mounts in an append-only, process-global `g_vols` table. Each
entry retains guest prefix identity, canonical host backing, a pinned directory
descriptor/host handle, kind flags, and read-only state. Registration publishes
the completed entry by release-storing the count. Resolution acquire-loads the
count and selects the longest live guest prefix. Runtime unmount marks an entry
dead and closes its host handle; it does not compact the table, avoiding races
with concurrent resolution. Engine teardown owns the remaining pinned handles.

`svc_mount` copies and lexically normalizes guest paths before changing state.
Bind mounts securely resolve an existing source and return `EFAULT`, `EINVAL`,
`EACCES`, or `ENOENT` before registration. tmpfs creates a private host directory,
registers it, and removes it if registration fails. Unsupported real filesystems
return `ENODEV`; already-synthesized pseudo filesystems are no-op successes.
Remount changes read-only state. Ordinary open/write operations route through the
selected jail and enforce that state. There is no partial mount result. Filesystem
host calls may block; mount registration itself has no cancellation or signal
protocol. The implementation is common to both guest ISAs and relies on POSIX
host descriptors and atomic publication rather than an ISA-specific branch.

## Rust ownership and comparison

| Retained capability | Rust owner | Status |
| --- | --- | --- |
| Guest mount namespace and longest-prefix routing | `hl-vfs::MountNamespace` and resolver | Implemented |
| Host backing and guest mount identity | `hl-engine` `NativePath` / `MountPaths` | Fixed: the pending open now retains the resolver-provided guest identity |
| Main executable absolute symlink confinement | `hl-engine` `FileSource::inside_root` | Fixed: an executable physically below the root uses root-relative confined open |
| External main executable support | `hl-engine` source selection | Preserved and regression-tested |
| Bind/tmpfs lifecycle | `hl-container::Volumes` and daemon container lifecycle | Implemented; tmpfs backing remains private and is reclaimed on removal |
| Runtime guest `mount(2)` and `umount2(2)` mutation | engine syscall personality | Remaining gap outside this launch-time mount fix |
| Runtime remount read-only mutation | engine syscall personality | Remaining gap outside this launch-time mount fix |

The defect was an identity divergence rather than host-path resolution: mounted
paths resolved to the correct external host backing, but `PendingOpen::new`
attempted to reconstruct their guest identity by stripping the rootfs prefix.
That is impossible for an external bind/tmpfs path and changed a successful
resolution into `Access`. `PendingOpen::at_guest` now accepts the guest identity
already selected by the mount resolver, matching the C `jail_pick` contract.

Linux uses confined `openat2`/path descriptors while macOS uses its native
no-follow/path-only equivalents. The fix changes neither platform mechanism; it
preserves the guest namespace identity above those adapters.
