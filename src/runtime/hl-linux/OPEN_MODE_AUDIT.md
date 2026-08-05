# Open mode oracle audit

The retained C engine was inspected read-only at
`../engine/src/linux_abi/syscall/fs.c`, in the AArch64 `openat` syscall case
(`case 56`) and the shared confined-path routing it invokes. The syscall confines
the guest path, selects name-bind, mount, or overlay backing, and forwards the
guest mode to host `openat`. Linux consults mode only when `O_CREAT` or
`O_TMPFILE` requests creation; an otherwise-unused nonzero mode is not invalid.
The result is atomic and returns host errno. Descriptor ownership transfers only
on success and teardown closes it. Both guest architectures share this behavior;
flag spellings differ, but mode admission does not. No lock or cancellation
transition precedes the potentially blocking host open.

Rust ownership is `hl-linux::FilesystemAbi`: it validates and translates the
guest syscall into `OpenAbiPlan`; `hl-engine` then confines and performs the host
open. Rust incorrectly rejected a nonzero mode unless creation was requested.
BusyBox passes `0666` through its open wrapper even for read-only `openat`, so
opening the correctly projected `/etc/hosts` returned `EINVAL`. The ABI now
preserves the mode and leaves its relevance to the open operation, matching Linux
and the retained implementation. Creation, confinement, read-only policy, and
host-errno behavior are unchanged.
