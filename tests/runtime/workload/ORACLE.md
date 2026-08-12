# Workload compatibility oracle audit

## Retained implementation studied

This category preserves the complete retained `core/workload` registration.
The read-only engine audit followed the workload through these implementation
owners and entry points:

- `../engine/src/core/dispatch.c`: `run_guest`, guest-bus activation and
  `jit_guest_bus_transition_begin`/`end`;
- `../engine/src/translator/cache.c`: `map_host`, `map_put`, source-range and
  generation invalidation, `stw_dispatch_safepoint`,
  `stw_force_dispatch_flush`, `stw_mapping_begin`/`end`, `stw_flush`,
  `smc_inplace_drop`, and `jit_after_fork`;
- `../engine/src/translator/arena.c`: `hl_arena_reserve`, `hl_arena_bind`,
  `hl_arena_repair`, and `hl_arena_release`;
- `../engine/src/translator/guest/aarch64/translate.c`: `translate_block`,
  `smc_queue_line`, `smc_commit`, and `smc_icflush`;
- `../engine/src/translator/guest/x86_64/translate.c`: `translate_block` and
  `jit86_drop_all_smc_translations`; and its `cache.c`: persistent-cache
  admission, relocation, fork invalidation, wholesale-flush handling, and exec
  re-keying;
- `../engine/src/linux_abi/syscall/mem.c`: `svc_mem` mmap, mprotect, munmap,
  mremap, brk, and accessible-range publication;
- `../engine/src/linux_abi/thread.c`: futex identity and bucket ownership,
  waiter registration/grants, interruptible waits, private-table fork repair,
  shared-file mapping publication, guest-bus transition locking, and thread
  lifecycle;
- `../engine/src/linux_abi/syscall/proc.c`: `bound_fork_prepare`,
  `bound_fork_complete`, `fork_child_hooks`, wait, and reap paths;
- `../engine/src/linux_abi/syscall/io.c` and `net.c`: `svc_io`, `svc_net`,
  guest-buffer validation, partial I/O, file mapping publication, socket
  admission, message copying, and blocking restart behavior.

No retained file was edited. The workload has no additional assembly owner:
architecture-specific entry and CPU-layout assembly is already audited by the
ABI category; these cases reach it through the dispatch and translator owners
above.

## Ownership, ordering, and teardown

The retained translator owns guest-PC identities independently of RW/RX host
storage. A completed translation is published under the JIT lock; translated
execution runs without that lock. Stop-the-world transitions publish an epoch,
force peers to a spilled dispatch boundary, and wait only for peers actually in
translated code. A peer blocked in a futex syscall has already cleared that
state and must not prevent a mapping transition. Old cache generations remain
owned until no registered CPU can enter them. Fork repairs the single survivor's
registries and cache state; exec re-keys the image; final teardown releases
arenas only after lookup and fault reconstruction can no longer reference them.

AArch64 SMC queues cache lines and publishes changes at the architectural
barrier. Unchanged line flushes do not invalidate unrelated translations, while
changed translated bytes retire the affected identity. X86 has coherent guest
instruction caches, so mmap/mprotect/munmap/mremap and write-fault tracking must
invalidate stale translations without an explicit guest cache instruction.
Mapping replacement preserves guest virtual addresses while host mappings and
code-cache storage remain internal.

Futex wait registration and the value check occur under the same bucket lock as
wake selection. Waits preserve mismatch `EAGAIN`, timeout, signal `EINTR`, and
fork repair; process-shared mappings use canonical backing identity. Fork
publishes the child only after all inherited state is prepared and rolls back
partial failure. Wait/reap retire child visibility before identifier reuse.
Socket and file operations validate guest ranges before host work, return a
partial transfer before an error, do not hold descriptor tables across blocking
calls, and preserve restart/cancellation ordering. SQLite and the miniature
server therefore exercise ordinary file/OFD, mmap, fork, loopback socket, and
teardown semantics rather than application-specific policy.

Host differences stay behind the POSIX/Windows adapters. AArch64-only cases use
raw AArch64 generated code and explicit instruction-cache maintenance; AMD64-only
cases use x86 generated code and mapping-driven invalidation. Common compute,
memory, fork, thread, and indirect-dispatch cases run on both guest ISAs.

## Rust ownership comparison

| Retained capability | Rust owner | State |
|---|---|---|
| Guest decode, indirect dispatch, architectural state | `hl-execution::{aarch64,x86}` | implemented |
| Native block cache, W^X publication, chaining and fault reconstruction | `src/runtime/native/exec` | implemented; performance parity remains a gate |
| Mapping identity, protection, replacement and shared backing | `hl-memory` plus `hl-runtime::memory` | implemented with known host-adapter gaps |
| Futex queues, wake ordering, PI and interruption | `hl-sync` plus `hl-runtime::futex` | implemented |
| Task/thread/fork/wait lifecycle | `hl-task` plus `hl-runtime::{fork,process}` | implemented |
| Descriptor, file, SQLite and socket joins | `hl-descriptor`, `hl-vfs`, `hl-network`, and `hl-runtime` adapters | implemented; compatibility cases remain authoritative |
| C persistent translated-image cache | native execution cache | divergent design; cold/warm and nested performance must be measured |
| Cross-host network and filesystem mechanisms | `hl-engine` platform adapters | target-specific verification remains required |

The cases are domain acceptance evidence. They do not authorize executable,
language, database, or runtime-name branches in production code.
