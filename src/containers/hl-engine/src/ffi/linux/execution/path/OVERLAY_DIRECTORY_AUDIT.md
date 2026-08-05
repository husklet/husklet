# Overlay directory and reverse-projection audit

The retained C oracle was read in
`../engine/src/linux_abi/container/vfs/overlay.c`: `guest_from_host_raw`,
`guest_from_host`, `guest_from_host_volume`, `ovl_push`, `ovl_seen`, and
`overlay_readdir`, together with their call sites in `syscall/fs.c`.

`overlay_readdir` scans the upper and then each lower. The first candidate for
a byte-exact name wins; a whiteout participates in this decision without being
emitted. An opaque directory ends the lower scan. `.` and `..` are synthesized
first. Immediate bind-mount, proc, and provider children are appended after the
layer merge and deduplicated against it. The C arrays grow with the directory;
the Rust adapter instead applies an explicit 65,536-entry resource bound.

`guest_from_host_raw` compares canonical host roots for the upper, every lower,
and active mounts. A component-boundary match is required and the longest host
prefix wins, which is load-bearing for nested mounts. Layer suffixes map from
guest `/`; mount suffixes append to their guest mount point. The C interface
falls back to `/` for an unknown path and later folds through chroot state.

`overlay_entries.rs` performs the host-neutral precedence merge over byte-exact
`GuestName` values and accepts namespace candidates as a separate final input.
`overlay_project.rs` provides typed reverse projection, ignores inactive roots,
uses `Path::strip_prefix` for component boundaries, and returns `None` rather
than silently converting an unknown host path to guest `/`. Chroot folding,
native directory acquisition, inode/cookie assignment, and synthesis of mount,
proc, terminal, and provider candidates remain with their existing owners.
