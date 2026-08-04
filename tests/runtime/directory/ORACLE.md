# Directory enumeration oracle

This is the freestanding workload formerly stored at
`tests/runtime/legacy/source/directory.c`. Its syscall sequence and assertions
are unchanged except for using each guest ISA's actual `O_DIRECTORY` value; the
legacy source incorrectly used the x86-64 value on AArch64. The scratch path is
relative instead of `/enum` so the native Linux oracle requires no host-root
write privilege. The final link and subdirectory may appear in either host
filesystem order, but both exact names and types must occur once. Its golden output is
deliberately zero bytes: every observable contract is checked in-process and a
mismatch is reported by a distinct nonzero exit status.

The invalid-buffer probes use a separate open description because QEMU user
mode consumes its host directory stream before surfacing guest `EINVAL` or
`EFAULT`. The cursor-sharing assertions therefore remain authoritative for
Linux and Husklet without encoding that QEMU translation artifact.

## Retained C engine audit

Read-only implementation files and entry points studied:

- `../engine/src/linux_abi/syscall/fs.c`: syscall cases 34/258 (`mkdirat`),
  35/263 (`unlinkat`), 36/266 (`symlinkat`), 56/257 (`openat`), 57/3
  (`close`), and 61/217 (`getdents64`); `typed_open_flags`,
  `typed_host_access`, `ovldents_free`, `ovldents_drop`,
  `ovldents_rewind`, and the merged-overlay snapshot path.
- `../engine/src/linux_abi/container/vfs.c`: `confine`, the guest filesystem
  context and root/cwd ownership, jail/overlay path resolution, directory-fd
  path and overlay-directory tables, merged lookup, copy-up, whiteout, and
  `overlay_readdir` call graph used by mutation and enumeration.
- `../engine/src/host/directory.h` and host implementations in
  `host/linux/directory.c`, `host/macos/directory.c`, and
  `host/windows/directory.c`: directory notification state lifecycle. This is
  adjacent directory state, but not exercised by this enumeration workload.

The C engine owns ordinary directory position through the duplicated host open
file description (`fdopendir(dup(fd))` is cached by descriptor). Overlay
directories instead own one heap-backed merged snapshot per guest descriptor,
with a position, names, and types. The first `getdents64` creates that snapshot;
successful whole records advance it, exhaustion frees it, seek rewinds it, and
close drops it before the host descriptor can be reused. Overlay enumeration
merges upper and lower layers, honors whiteouts, retains `.` and `..`, and uses
the merged entry's real inode where possible. Mutation checks the merged type
and contents before unlink/rmdir, then invalidates path caches. Directory paths
are confined component-by-component beneath the guest root; cwd/root state is
process-local until `CLONE_FS` promotes it to shared storage, and fork without
`CLONE_FS` detaches it.

For `getdents64`, a buffer too small for the next whole record returns `EINVAL`
without advancing. An inaccessible first record returns `EFAULT`; a bad
descriptor returns `EBADF`. A successful partial batch returns its byte count,
and the next call resumes at the next entry. Records are 8-byte aligned and
carry inode, increasing continuation cookie, record length, Linux `d_type`, and
NUL-terminated name. `dup` aliases the same Linux OFD and therefore the same
cursor; closing one alias does not retire the OFD, while final close tears it
down and subsequent use is `EBADF`. These operations do not block or have a
cancellation path. Host syscall failures are translated immediately to Linux
errno.

The domain owners have no guest-architecture branches; only syscall numbers in
the workload differ between AArch64 and x86-64. Linux and macOS use different
host notification mechanisms (inotify versus kqueue), while the retained
Windows notification adapter explicitly returns `ENOSYS`; those notification
branches are outside this workload. Enumeration itself is composed through the
host file/VFS adapters on each host.

## C-to-Rust capability matrix

| C capability | Rust owner | Status exercised here |
|---|---|---|
| confined path resolution and directory/file/symlink creation/removal | `hl-vfs` resolver, mutation service, and `hl-runtime/src/filesystem` composition | mkdir, create, unlink, symlink, rmdir |
| directory entity, stable bounded snapshot, position and teardown | `hl-vfs/src/directory_description.rs` | ordered snapshot, exhaustion, final close |
| shared open-file-description cursor across descriptor aliases | `hl-descriptor` operation lease/table plus `VfsDirectoryDescription` state | alternating reads through `fd` and `dup(fd)` |
| Linux dirent64 layout, alignment and guest copyout | `hl-linux/src/filesystem/abi.rs` | five exact 24-byte records and three `d_type` values |
| whole-record batching, transactional cursor commit, errno ordering | `hl-runtime/src/filesystem/syscalls.rs` | `EINVAL`, `EFAULT`, `EBADF`, continuation, drained zero |
| architecture syscall admission | `hl-linux/src/syscall/table.rs` | AArch64 and x86-64 |

This workload does not prove overlay whiteout precedence, seek/rewind,
non-UTF-8 names, directories larger than the snapshot bound, concurrent
mutation, fork/checkpoint, permission identities, or directory notifications.
Those remain separate focused cohorts; no completeness claim is inferred here.
