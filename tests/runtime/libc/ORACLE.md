# libc compatibility oracle audit

## Retained implementation studied

This category exercises guest libc rather than an engine-owned libc shim. The
engine contract beneath it was audited in the read-only retained tree at
`../engine/src/linux_abi/elf.c` (`elf_interp`, ELF header readers, non-PIE atomic
helpers), `syscall/io.c` (`io_guest_vector_gather`,
`io_guest_vector_scatter`, descriptor virtualization and size gates),
`syscall/mem.c` (`guest_bad_ptr`, `pread_retry`, fixed-map and remap publication),
`syscall/time.c` (`engine_clock_gettime`, `engine_sleep_until`, timer ownership),
`syscall/proc.c` (fork, exec and exit transitions), `syscall/signal.c`
(`syscall_should_restart`, retry and thread-directed delivery), and `signal.c`
(`sigq_push`, `sigq_pop`, `sigreturn_frame`, `maybe_deliver_signal`). The syscall
dispatch and guest-copy paths were followed through
`syscall/dispatch.c`, `syscall/guest_copy.c`, and `syscall/misc.c`.

## Ownership, ordering, and lifetime

The retained engine loads the static ELF and owns its mappings until process
teardown; libc owns allocator arenas, stdio buffers, locale state, conversion
state, exit callbacks, and user-level sorting/searching state inside those guest
mappings. Descriptor numbers remain process-local while shared open-file
descriptions own offsets, so stdio positioning and partial I/O ultimately depend
on ordered engine copy-in/copy-out and OFD updates. Guest pointers and vector
counts are checked before host access. Mapping publication occurs only after
successful backing work. Signal queues and masks are process/task state and
`setjmp`/`sigsetjmp` restoration depends on architecture-specific signal frames.
Exit runs guest libc callbacks before the engine releases mappings and descriptor
ownership.

Blocking I/O and time operations preserve partial progress, `EINTR`, restart,
and deadline behavior. Allocation failures and invalid guest ranges propagate
Linux errno rather than panicking. AArch64 and x86-64 use distinct ELF layouts,
syscall tables, register conventions, and signal-frame codecs, then join the
same descriptor, memory, task, time, and VFS ownership. Host-specific clock,
locale, and filesystem mechanisms are adapters; guest-visible output remains
the pinned Linux static-libc contract.

## Retained-C to Rust capability matrix

| Retained capability | Rust owner | State |
|---|---|---|
| static ELF inspection, segments, interpreter and initial stack | `hl-loader` | implemented |
| guest mapping bounds, protection and publication | `hl-memory`; file join in `hl-runtime` | implemented, cohort evidence required |
| guest pointer and bounded copy-in/copy-out | `hl-linux` ABI codecs and `hl-runtime` | implemented |
| descriptor identity, OFD offsets and partial I/O | `hl-descriptor`; joins in `hl-runtime` | implemented, cohort evidence required |
| pathname-backed stdio operations | `hl-vfs`; joins in `hl-runtime` | implemented, cohort evidence required |
| process environment, exit and exec lifecycle | `hl-task`, `hl-loader`, and `hl-runtime` | implemented |
| clock and calendar syscall substrate | `hl-time`; Linux conversion in `hl-runtime` | implemented, host-independent UTC evidence required |
| signal mask, delivery, frame return and restart | `hl-task`, `hl-linux`, and `hl-runtime` | implemented, both-ISA evidence required |
| guest libc allocator, locale, math, stdio and string algorithms | guest static libc | outside engine ownership; preserved as compatibility evidence |

No application- or libc-specific production branch is acceptable. Any mismatch
in this category must be assigned to the generic loader, memory, descriptor,
VFS, task, signal, time, or Linux ABI invariant above.

## Migrated contract

`test.yaml` retains all 95 independently selectable cases, both guest ISAs,
their static optimized GNU C build, required math linkage, exact exit status,
and byte-for-byte golden stdout. The four calendar cases retain `TZ=UTC`.
Sources and goldens are source-controlled inputs; generated executables belong
only under the testing output directory.
