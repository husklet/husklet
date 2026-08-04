# Filesystem syscall adapter

`RuntimeFilesystemSyscalls` is the composition boundary between Linux ABI
marshalling, guest memory, descriptor ownership, and VFS objects. It performs no
host operating-system calls.

Descriptor-local `CLOEXEC` state remains in `DescriptorTable`; duplicated
descriptors share OFD status, offsets, file contents, and directory cursors.
Every operation pins an `OperationLease`, so concurrent close retires the
descriptor number without invalidating an admitted operation.

Reads probe guest output before mutating an OFD. Writes use the accessible input
prefix. Vector operations enforce Linux iovec count, overflow, and signed result
limits. Positional I/O does not change the shared offset. VFS owns append and
seek behavior.

`ioctl` is typed for both guest ISAs and deliberately exposes only operations
owned by existing descriptor contracts. `FIONBIO` updates shared OFD
nonblocking state; `FIOCLEX` and `FIONCLEX` update descriptor-local exec flags;
and `FIONREAD` reports the clamped regular-file extent remaining after the
shared offset. Descriptor admission precedes argument access, successful
queries copy out only their four-byte integer, and unknown or device-specific
requests return `ENOTTY`. Socket queue depth, termios, and device policy remain
unsupported until their owning domains provide explicit contracts.

`fstat` converts descriptor-neutral metadata to VFS metadata and delegates exact
AArch64 or x86-64 layout encoding to `hl-linux`.

`getdents64` is transactional:

1. Peek a `DirectoryBatch` without moving the shared cursor.
2. Fit complete Linux dirents, preserving byte-exact names.
3. Reject a pending first record that cannot fit.
4. Probe and copy the staged bytes to guest memory.
5. Commit the emitted count using the batch generation and starting cookie.

Copyout failure never commits cursor progress. Refresh changes the batch
generation, so stale batches cannot advance a new snapshot. EOF returns zero.

`openat` and `openat2` use `RuntimePathHost`. Relative operations retain a
generation-pinned directory lease; absolute paths use the root and ignore
`dirfd`. Host effects commit before an unpublished descriptor transaction is
published. Preparation, capacity, and host-commit failures roll back without a
visible descriptor. `CLOEXEC` is descriptor-local; access, append, and
nonblocking state belong to the new OFD.

Path metadata, access checks, and readlink resolve through pinned path leases and
use staged Linux copyout. Namespace mutations decode to `FsMutationPlan`, retain
every ordered directory base through one `PreparedPathMutation`, and either
commit once or roll back. Absolute operands use root and ignore `dirfd`; relative
operands retain the admitted OFD identity even if descriptor numbers are closed
and reused concurrently. Host xattrs and unsupported mutation mechanisms remain
explicitly unsupported rather than reporting synthetic success.
