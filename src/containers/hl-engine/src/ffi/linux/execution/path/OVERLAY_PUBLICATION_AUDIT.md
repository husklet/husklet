# Native overlay publication audit

The retained C oracle was read in
`../engine/src/linux_abi/container/vfs/overlay.c`: `wh_hostpath`, `wh_exists`,
`overlay_mkparents`, `ovl_copy_xattrs`, `ovl_copy_meta`, `ovl_rm_rf`,
`overlay_clear_whiteout`, `overlay_set_opaque`, `overlay_copyup`, and
`overlay_whiteout`.

The retained implementation resolves lower content without making it writable.
Before mutation it materializes each missing upper ancestor in order, copying
the visible lower directory mode. Regular copy-up writes bytes to the upper and
preserves the complete permission mode, timestamps, and xattrs. Re-creation
removes `.wh.NAME`; deletion removes the upper object and creates `.wh.NAME`;
recreated directories use `.wh..wh..opq` when lower children must stay hidden.
Each namespace relocation invalidates cached resolution. Mount routes bypass
the rootfs union. The retained implementation is process-global and its direct
copy is visible before metadata is complete.

`overlay_publish.rs` confines every operation beneath an already pinned upper
parent. Regular content, mode, timestamps, and xattrs are completed in a hidden
staging inode and one `renameat` publishes it. The whiteout is cleared only
after that upper entry exists; a clear failure attempts to restore the private
staged name, while a failed rollback still leaves the upper entry hidden by the
marker. Source reads use `pread`, so copy
up neither writes the lower nor changes its shared file offset. Whiteout and
opaque markers are likewise staged and renamed. Parent materialization rejects
a symlink or non-directory collision. Directory fsync makes each publication
durable before a caller advances its resolution epoch.

Recursive removal of a non-empty upper directory, recursive directory copy-up,
ownership virtualization, lower-directory metadata selection, and cache-epoch
publication remain with the overlay resolver/mutation owner. This adapter does
not infer a guest path from a host descriptor and does not route bind mounts.
