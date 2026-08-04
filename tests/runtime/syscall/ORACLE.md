# Minimal syscall-routing oracle audit

This folder owns one freestanding non-PIE smoke case. `main.c` requires a
positive guest process ID, writes the exact bytes `syscall-ok\n`, and exits
zero. `abi.h` owns the per-ISA syscall spellings and `golden/stdout.txt` owns
the byte contract.

## Retained C implementation studied

- `../engine/src/linux_abi/number.c` maps x86-64 getpid syscall 39, write
  syscall 1, and exit syscall 60 to canonical syscalls 172, 64, and 93;
  AArch64 issues those canonical numbers directly.
- `../engine/src/linux_abi/syscall/dispatch.c` (`service`, `service_local`)
  owns canonical dispatch and rebases the static non-PIE write pointer before
  the I/O family receives it.
- `../engine/src/linux_abi/syscall/proc.c` (`svc_proc`, case 172) returns the
  stable guest identity from `container_pid`; case 93 records exit state.
- `../engine/src/linux_abi/syscall/io.c` (`svc_io`, case 64) owns the stdout
  write and its guest-buffer and descriptor checks.

Getpid observes process identity without changing ownership or blocking. Rust
ownership maps syscall admission to `hl-linux`, PID namespace/task identity to
`hl-task` plus the runtime process adapter, stdout to `hl-descriptor` plus
runtime I/O, and result publication to the process and engine composition.

This proves only minimal routing for getpid, write, and exit on both guest
ISAs. It does not establish PID-namespace lifecycle, fork identity, thread IDs,
partial writes, or broader syscall completeness.
