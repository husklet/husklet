# Layered rootfs lookup audit

## Retained C oracle

The retained implementation was read from
`../engine/src/linux_abi/container/vfs/overlay.c`,
`../engine/src/linux_abi/container/vfs/resolve.c`, and the `openat` path in
`../engine/src/linux_abi/syscall/fs.c`. The relevant entry chain is
`overlay_resolve` -> `overlay_lookup` -> `overlay_lookup_raw` ->
`overlay_dir_verdict`/`layer_follow`, followed by the final host `open`.
`secure_resolve` and `jail_open_plan` own the component-confined non-overlay
walk. The overlay owns ordered upper/lower identity, whiteout and opaque
precedence, cross-layer symlink traversal, and copy-up publication. Directory
pins and the filesystem-resolution epoch live for the process and are reset or
invalidated on namespace mutation and fork/chroot transitions. Bind mounts are
routed outside the root overlay. Host descriptors pin every traversed parent;
the final `openat` is relative to a pinned parent, and errors preserve the host
operation's `errno`. AArch64 and x86-64 share this VFS implementation; only
their syscall-number and guest-register front ends differ.

The retained lookup caches are process-scoped and epoch-gated. In particular,
the upper-negative and merged-directory-verdict caches avoid repeated upper
probes, while the ordinary jail open cache replaces a repeated component walk
only when mutation, directory, and no-follow semantics make that safe. No cache
entry lends a lower layer mutation capability.

## Rust ownership and measured issue

Rust maps the confined walker to `hl-vfs::Resolver` and the native layered host
to `hl-engine/src/ffi/linux/execution/path/pin.rs`. `PinEntry` owns the guest
identity and ordered upper/lower descriptor candidates. `ParentLease` duplicates
the selected read capability and a distinct upper mutation capability;
`overlay_publish.rs` owns copy-up and publication ordering. `MountNamespace`
keeps bind/provider routes outside the layered root.

Before this change, every layered `with_entry` call held the single pin-registry
mutex while its closure issued `fstatat`, `openat`, `fstat`, whiteout, opaque,
device, descriptor-duplication, or `readlinkat` work. Consequently unrelated
guest threads serialized behind the slowest host filesystem operation. Closing
a handle also could not proceed until that operation completed, extending
descriptor lifetime and teardown latency. This is broader than the retained C
locking boundary, which does not hold a global overlay registry lock across
host path traversal.

The bounded correction stores each registry value in `Arc<PinEntry>`, clones
that ownership under the mutex, and releases the mutex before invoking the host
operation. Removal still makes the opaque handle immediately unreachable, while
the cloned entry keeps its descriptors alive until an already-admitted operation
finishes. Ordered layer precedence, whiteouts, opaque directories, namespace
routing, epoch publication, and the prohibition on lower mutation are unchanged.

The focused concurrency test is fail-first evidence for the old boundary: an
operation admitted through `with_entry` waits for a concurrent close. Base
`3e039c86ce62bc800c29b6eb85d9c8e4b17114ae`, with only the test applied, held
the registry mutex and failed at its one-second timeout (`1.01 s` test time).
Exact committed candidate `83557636b` permitted close to finish while the
operation remained admitted (`0.01 s` test time), and its warning-strict
`cargo test -p hl-engine overlay_` gate passed 15/15. These timings prove removal
of the global blocking interval; they are not an end-to-end provider throughput
claim. A content-bound native/retained-C/Rust guest workload remains required
before promoting this as a release performance result.
