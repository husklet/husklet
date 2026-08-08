# Positional I/O evidence

Both ISA rows are active. The QEMU oracle cannot referee this fixture at all:
run natively with the guest path rewritten into a writable scratch directory,
`qemu-aarch64` and `qemu-x86_64` both exit 9, because QEMU user mode validates
the guest buffer before the descriptor and so answers `pread64(-1, 1, 3, 0)`
with `EFAULT` where Linux answers `EBADF`. Real Linux is the ground truth here.

The fixture's original check 27 required
`preadv(fd, [{buf,1},{(void*)1,1}], 2, 0)` to return `-EFAULT` with the buffer
untouched. That contract is wrong. Built from the fixture's own source and run
natively on aarch64 against both btrfs and tmpfs, and again through raw `svc 0`
with no libc, Linux returns 1, publishes `'a'` into the first vector, and leaves
the shared file position at 6. `EFAULT` appears only when the leading vector
faults, so nothing at all is transferred. The engine already matched Linux
exactly; only the assertion was wrong, and it now also pins the unchanged shared
position and the leading-fault `EFAULT`.

Positional dispatch was audited across the whole family. `VectorAdapter::call`
selects `preadv`/`pwritev`/`preadv2`/`pwritev2` for every `VectorPosition::At`
and `readv`/`writev` with offset -1 for `VectorPosition::Shared`, and
`VectorAdapter::first_fault` probes the supplied offset with `pread` rather than
`lseek`. No p-variant consults or advances the shared position.

Both rows opt out of the app `diagnostics-floor`. The fixture is straight-line
syscall code with no hot loop, so the native tier reports
`runs=0 builds=0 sites=0` while comparable fixtures in this app report
`runs=15` and `runs=30`. The exit and stdout contracts are unchanged.

The path-metadata source was also compiled for both ISAs and executed in a
fresh temporary working directory. Both QEMU runs exited zero with exact
zero-byte stdout. The shared oracle runner currently executes from the
repository root rather than an isolated scratch directory, where pre-existing
names make the fixture's cleanup assertions fail. Its row remains typed broken
until the runner supplies the already-proven isolated working directory.
