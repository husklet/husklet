# Freestanding stdout-write oracle audit

This folder owns one freestanding non-PIE smoke case. `main.c` writes the exact
bytes `compat-write\n` from static storage to stdout and exits zero only when
the write returns the whole byte count. `golden/stdout.txt` is the byte-exact
contract; `abi.h` owns both guest syscall spellings.

## Retained C implementation studied

- `../engine/src/linux_abi/number.c` maps x86-64 write syscall 1 and exit
  syscall 60 to canonical syscalls 64 and 93; AArch64 issues 64 and 93
  directly.
- `../engine/src/linux_abi/syscall/dispatch.c` (`service_local`) applies
  `nonpie_rebase_args` before family dispatch. That is required for the static
  low-address pointer in this ET_EXEC image.
- `../engine/src/linux_abi/syscall/io.c` (`svc_io`, case 64) validates the
  descriptor and guest range, performs `guest_fd_write`, preserves partial or
  error results, applies restart behavior, and raises guest SIGPIPE on EPIPE.
- `../engine/src/linux_abi/syscall/proc.c` (`svc_proc`, case 93) records the
  final exit status.

The descriptor table owns stdout's open file description. The syscall observes
but does not retain the source pointer; the fixture itself converts any partial
or failed write into a nonzero exit. Rust ownership is non-PIE guest-address
translation at the Linux boundary, stdout/OFD behavior in `hl-descriptor` plus
runtime I/O, and exit publication in the process runtime.

This case covers one complete stdout write. It does not establish partial I/O,
blocking or cancellation, concurrent offset behavior, broken-pipe delivery, or
general descriptor semantics.
