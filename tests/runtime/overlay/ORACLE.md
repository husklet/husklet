# Overlay coherence oracle

## Retained behavior and harness

The byte-exact workload is retained from
`../engine/tests/compat/overlay/coherence.c`. The read-only integration harness
studied was
`../engine/pkgs/rust/tests/spec.rs::overlay_mmap_truncate_and_rename_share_one_copied_up_file`.
It builds the probe, launches it with an empty writable upper and work tree over
a tools layer plus the Alpine rootfs, and then checks the upper directly. The
guest must leave `etc/hostname.moved` containing `MMca` and must publish
`etc/.wh.hostname`.

The probe covers two related lifecycles. First, it creates a new upper file,
copies a lower file into it, and applies the `cp -p` metadata sequence
`fchown` -> `fchmod` -> `futimens`; the same open file description must observe
all three mutations before final close. Second, opening the lower-only hostname
read-write must copy up one inode. A shared mapping, `msync`, `munmap`,
`ftruncate`, close, and rename must all continue to name that copied-up inode.
The old lower name is hidden by a whiteout and the renamed upper file retains
the mapped writes and truncation.

## Retained C implementation audit

The following read-only implementation and entry functions were studied:

- `../engine/src/linux_abi/container/vfs.c`: `secure_resolve`, `resolve_at`,
  `jail_pick`, and `g_vfs_namespace`. These own confined guest-to-host
  resolution and expose the process overlay upper plus ordered lowers.
- `../engine/src/linux_abi/container/vfs/overlay.c`:
  `overlay_lookup_raw`, `overlay_lookup`, `overlay_resolve`,
  `overlay_mkparents`, `ovl_copy_meta`, `overlay_copyup`,
  `overlay_copyup_tree`, and `overlay_whiteout`. Lookup chooses the first
  visible upper/lower inode while honoring whiteout and opaque markers.
  Copy-up creates missing upper parents, copies regular bytes, preserves mode,
  timestamps, and xattrs, then advances the namespace-resolution generation.
- `../engine/src/linux_abi/syscall/fs.c`: the overlay branch of `openat` and
  the `renameat`, `ftruncate`, `fchmod`, `fchown`, and `utimensat`/`futimens`
  paths. A write open performs copy-up before returning the descriptor;
  metadata operations preserve Linux validation/errno order; rename first
  materializes a lower source, moves the upper inode, and whiteouts a lower
  source only after successful host rename.
- `../engine/src/linux_abi/syscall/mem.c`: `mmap`, `munmap`, and `msync`
  dispatch, including `filemap_register`, `filemap_unmap`, and
  `filemap_refresh_emulated` from `thread.c`. A file-backed `MAP_SHARED`
  mapping retains backing identity independently of the guest descriptor;
  stores become visible to the copied-up file and teardown removes only the
  unmapped mapping range.

The overlay upper/lower arrays and path caches are process runtime state.
Descriptors own references to host open file descriptions; file-map entries
retain a duplicate backing descriptor while a mapping lives. Overlay mutation
bumps the shared resolution epoch so cached lower paths cannot survive a
copy-up or rename. File-map mutation is protected by `g_filemap_lock`, while
fast shared-range admission uses atomic bounds/epochs; no filesystem table lock
is held across guest blocking I/O. Close releases descriptor-local overlay
metadata, final descriptor release closes its host handle, and `munmap` removes
the corresponding file-map span. Failures return the syscall's Linux errno and
do not publish a successful rename/whiteout transition. Reads and writes retain
ordinary partial-I/O behavior; this probe treats any short copy write as a
failure. None of the exercised calls has a cancellation protocol.

The overlay algorithm has no guest-ISA branch. AArch64 and x86-64 differ only
in syscall admission and translated register ABI. Host-specific filesystem and
xattr adapters supply metadata operations; coarse host pages add special
file-mapping paths on macOS and Windows, while Linux normally uses its native
page size and mapping behavior.

## C-to-Rust capability matrix

| Retained capability | Rust owner | Current status |
|---|---|---|
| overlay launch paths | `hl-container/src/engine/spec.rs`, `hl-engine` launch options | lower/work options are projected |
| precedence, whiteout and opaque lookup | `hl-vfs/src/overlay/lookup.rs` | modeled and unit-tested |
| transactional copy-up with metadata | `hl-vfs/src/overlay/mutation.rs` and `overlay/model.rs` | modeled behind `OverlayHost` |
| image upper/lower archive behavior | `hl-container/src/filesystem/overlay.rs`, `hl-images` | implemented for container image operations |
| syscall/OFD/shared-map coherence through a production overlay host | `hl-runtime` filesystem composition | **gap:** no production `OverlayHost` implementation or equivalent end-to-end adapter was found |
| YAML construction of lower, upper, and work trees | `apps/testing` runtime runner | **gap:** not expressible by the current case schema |

The typed unsupported status is therefore intentional. Unit-level overlay
planning and container archive behavior do not prove the retained guest syscall
chain, and native QEMU cannot prove copy-up or whiteout without the product
overlay adapter.
