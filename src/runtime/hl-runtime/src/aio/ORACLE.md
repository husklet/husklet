# Retained C AIO oracle

This audit covers submission-prefix behavior in `io_submit`. The retained tree
was read only.

## Sources and entry points

- `../engine/src/linux_abi/syscall/aio.c`: `svc_aio` case 2, `aio_do_one`,
  `aio_opcode_supported`, `aio_push`, and `aio_eventfd_kick`.
- The case-2 call path copies the complete guest `struct iocb **` array before
  entering its sequential submission loop. Each element is then copied into a
  64-byte host buffer, synchronously executed, and followed by completion-ring
  insertion and optional eventfd notification.

## State, lifetime, and locking

`g_aioctx` owns fixed context slots. Each live slot owns its bounded completion
ring. `g_aio_lock` protects context allocation, teardown, and ring mutation, but
the C implementation deliberately does not hold it across guest I/O. The copied
pointer array and each copied control block are call-local. A submission becomes
observable only after `aio_push`; a rejected control produces no completion or
eventfd notification.

## Submission and error ordering

The retained implementation first validates accessibility of the complete
pointer array with one `guest_copy_from`. Failure returns `EFAULT` before any
control is submitted. It then processes pointer values in order. A NULL or
unreadable control pointer stops processing with `EFAULT`; an unsupported opcode
stops processing with `EINVAL`. Either error is returned only when zero earlier
controls were submitted. Once a prefix has submitted, `io_submit` returns that
prefix count and leaves exactly those completions queued. Execution results such
as descriptor and I/O errors are completion results rather than synchronous
submission failures.

The submission operation is synchronous, so this path has no blocking or
cancellation transition. Context teardown and a concurrent completion drain are
serialized by `g_aio_lock`; independent I/O executes outside the lock.

## Architecture and host branches

AArch64 syscall 2 and x86-64 syscall 209 both reach canonical AIO operation 2.
The pointer and control layouts are LP64 on both guest ISAs. The retained macOS
implementation emulates kernel AIO synchronously; typed descriptors select the
Rust-backed descriptor path, while remaining descriptors use host `pread`,
`pwrite`, vectored I/O, or `fsync`. None of those branches changes prefix/error
ordering.

## Rust ownership mapping

- `hl_linux::aio::Abi::pointers` owns the bounded, full-array copy. It must retain
  raw NULL elements so validation does not erase an already-valid prefix.
- `hl_runtime::aio::RuntimeAioSyscalls::submit` owns ordered iteration and the
  Linux rule that an error is returned only when the submitted prefix is empty.
- `RuntimeAioSyscalls::submit_one` owns per-control pointer and opcode validation;
  it rejects NULL with `EFAULT` before parsing the control.
- `hl_aio::Catalog` owns context identity, admission, completion capacity, queue
  ordering, wakeups, and teardown.
- `hl_descriptor` and runtime scalar/vector adapters own descriptor pinning and
  I/O result semantics.

Focused tests cover both guest ISAs for a valid control followed by NULL, a sole
NULL, an unsupported opcode after a valid prefix and as the first control, and a
pointer array whose inaccessible tail must prevent the valid prefix from running.
