# Freestanding exit oracle audit

This folder owns one freestanding non-PIE smoke case. `main.c` issues only
Linux `exit(42)` through `abi.h`; `golden/empty.bin` requires no output and the
manifest requires status 42.

## Retained C implementation studied

- `../engine/src/linux_abi/number.c` maps x86-64 exit syscall 60 to canonical
  syscall 93; AArch64 issues 93 directly.
- `../engine/src/linux_abi/syscall/dispatch.c` (`service`, `service_local`)
  owns the syscall boundary and canonical family dispatch.
- `../engine/src/linux_abi/syscall/proc.c` (`svc_proc`, case 93) records
  `c->exited` and `c->exit_code` for the calling guest thread.

The exit path retains no guest pointer and allocates no descriptor or other
guest-visible resource. Rust ownership is per-ISA syscall admission in
`hl-linux`, exit state in `hl-task` and the runtime process adapter, and final
engine-result delivery in `hl-engine`.

This is launch and exit-status smoke evidence only. It does not establish
process-wide `exit_group`, multithreaded teardown, robust-futex cleanup,
signals, fork, exec, or descriptor behavior.
