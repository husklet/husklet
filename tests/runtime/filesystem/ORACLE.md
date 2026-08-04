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

## Positional I/O and pathname metadata cohort

The same retained `fs.c` audit followed `openat`, `pread64`, `pwrite64`,
`preadv`/`pwritev`, `readlinkat`, `newfstatat`, `statx`, rename/link/symlink,
and close teardown through their complete dispatch branches. Retained
`host_fd.h`, `host_uio.h`, `container/vfs.c`, and `container/vfs/resolve.c`
were checked for offset-preserving positional operations, vector partial
results, confined dirfd lookup, final-symlink policy, mutation invalidation,
and host-specific adapters. OFD offsets are shared only by ordinary I/O;
positional operations must not mutate them. Metadata copyout validates bounded
guest storage before publication, and mutation becomes visible across fork
before wait returns. Rust ownership maps to `hl-descriptor`, `hl-vfs`,
`hl-linux` filesystem ABI encoding, and `hl-runtime` filesystem composition.

The suite preserves independently built directory, positional, and
path-metadata sources on both guest ISAs. Oracle-environment limitations remain
explicitly broken with evidence; they are not promoted to passes.

## Full legacy filesystem compatibility category

The canonical manifest also owns every registration formerly held by
`tests/runtime/legacy/oracle/tests/compat/filesystem`. The migration preserves
the legacy case names, source bytes, compiler/linker flags, guest targets,
environment, arguments, exit status, and golden bytes. Source and golden names
are flattened to semantic names of at most two underscore-separated words.
Cases needing the legacy scratch-rootfs constructor, and cases whose legacy
disposition was `excluded-macos`, remain explicitly unsupported with this file
as evidence; they are still listed by the runner.

The whole-domain read-only C audit covered these implementation roots and entry
functions:

- `../engine/src/linux_abi/syscall/fs.c`: `svc_fs`, `fs_operation_name`,
  `typed_open_flags`, `typed_host_access`, `typed_host_creation`,
  `guest_fill_linux_stat`, `guest_statfs_magic`, `guest_xattr_set/get/list/remove`,
  `ovldents_free/drop/rewind`, and syscall cases for xattr, mkdir/unlink/link/
  rename, mount/statfs, truncate/fallocate, access/chdir/chmod/chown, openat2/
  openat, close, getdents64, readlinkat, stat/fstat/statx, sync, utimensat,
  umask, and fadvise.
- `../engine/src/linux_abi/container/vfs.c`: filesystem-context share/fork,
  `confine`, canonicalization and symlink resolution, volume and root-handle
  binding, proc-fd publication, memf attach/adopt/materialize and positional
  I/O, memfd seal registry, overlay directory state, cache invalidation, and
  close/fork cleanup.
- `../engine/src/linux_abi/container/vfs/resolve.c`: `resolve_at`, `jail_at`,
  `jail_open_plan`, volume selection, dot-dot containment, final-symlink policy,
  and host-error conversion.
- `../engine/src/host/{linux,macos,windows}` filesystem, directory, xattr, and
  file adapters, including the host-specific stat birth-time, locking,
  notification, sparse-file, and xattr branches responsible for the recorded
  macOS exclusions.

The retained engine owns process root/cwd state until `CLONE_FS` shares it;
fork otherwise detaches that state. Guest descriptors own references to shared
open-file descriptions, so duplicate descriptors share offsets and append/lock
state while descriptor flags remain local. Final close drops overlay snapshots,
path/proc-fd publication, emulated memf/memfd state, and host handles in that
order. Path operations resolve and confine the dirfd-relative guest path before
host mutation, then invalidate positive and negative caches after successful
mutation. Copyout is validated before publishing stat, statx, xattr, directory,
or vector-I/O results; partial I/O is returned before later faults, and host
errors are converted immediately to Linux errno. Blocking FIFO, file-lock, and
child cases retain signal interruption and teardown through their syscall/
task owners; ordinary metadata operations have no cancellation transition.

The Rust ownership comparison maps confined lookup, symlinks, metadata,
directory snapshots, xattrs, and mutation to `hl-vfs`; descriptor identity,
OFD offsets, flags, locks, and final-close lifetime to `hl-descriptor`; Linux
structure encoding and errno admission to `hl-linux`; and filesystem/procfs,
fork/exec, provider-backed storage, and guest-memory copy composition to
`hl-runtime`. Persistent-cache execution remains execution-owned and memfd
mapping/seal interaction joins descriptor, memory, and filesystem owners in
`hl-runtime`. The manifest records known host or runner divergences rather than
claiming those capabilities complete.

### Direct oracle matrix

The migrated category was built and executed with 18 bounded workers, compiling
each declared target with its exact manifest flags and running it under
`qemu-aarch64` or `qemu-x86_64` in an isolated working directory. Of 177
declared case/target rows, 150 matched exit status and stdout byte-for-byte, 15
were pre-declared unsupported host/runner contracts, and 12 rows (six cases on
both ISAs) produced typed QEMU divergences. Those six are `bound-uaccess`,
`fs-mountpseudo`, `fs-mounttab`, `fs-statfs-type`, `openat2-einval`, and
`xattr-edge`; each remains visible as broken. There were no compilation failures
or timeouts. This is oracle classification, not evidence that the Rust engine
passes the category.
