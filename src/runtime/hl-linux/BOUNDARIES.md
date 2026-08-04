# Linux personality ownership

`hl-linux` owns guest-visible Linux ABI geometry, validation, syscall plans,
transactional copyout, errno translation, signal-frame encoding, syscall routing
contracts, and seccomp policy evaluation. It does not own descriptors, sockets,
tasks, mappings, event objects, filesystems, clocks, or IPC objects; those runtime
domains execute the typed plans exported here.

## Retained C oracle

The corresponding working C implementation was inspected before restructuring:

- `src/linux_abi/syscall/event.c`, plus `epoll.c`, `eventfd.c`, and `inotify.c`,
  combine architecture-dependent event layout with concrete kqueue/poll/object
  lifetime. Rust `event/` owns only creation/control/wait plans and staged guest
  copyout; `hl-event` owns object behavior.
- `src/linux_abi/syscall/net.c` and `container/netns.c` combine sockaddr/message
  bounce buffers, ancillary conversion, namespace routing, and host sockets. Rust
  `network/` owns wire decoding/encoding and transactional message copyout;
  `hl-network` owns socket state and transport.
- `src/linux_abi/syscall/proc.c`, `thread.c`, and `linux_abi.c` combine clone,
  fork, exec, wait, credentials, futexes, host processes, and fork publication.
  Rust `process/` owns process syscall marshalling only; `hl-task`, `hl-sync`, and
  `hl-runtime` own lifecycle and cross-domain publication.
- `src/linux_abi/syscall/mem.c`, `logical_vma.c`, and the mapping portions of
  `thread.c` combine guest range validation with concrete host mappings. Rust
  `memory/` owns map/range/advice plans and staged mincore/copyout; `hl-memory`
  owns mappings and protection state.
- `src/linux_abi/syscall/fs.c` combines stat layouts, jailed traversal, overlay,
  device, terminal, xattr, and descriptor behavior. Rust `filesystem/` owns ABI
  operands, mutation plans, stat/statfs wire encoding, and copyout; `hl-vfs`,
  `hl-terminal`, and `hl-descriptor` own the entities.
- `src/linux_abi/signal.c` and `syscall/signal.c` combine queues, host handlers,
  delivery policy, and architecture frames. Rust `signal/` owns syscall and frame
  ABI; task signal state remains in `hl-task`, with delivery composition above it.
- `src/linux_abi/syscall/sysv.c` combines wire structures with concrete shared
  memory, semaphore, and message queues. Rust `sysv/` owns wire values/codecs and
  plans; `hl-ipc` owns object state.
- `src/linux_abi/thread.c` and `syscall/time.c` share futex deadline, timer, and
  clock behavior in the C unity translation unit. Rust `futex/` keeps the coupled
  guest encoding together while execution remains split between `hl-sync` and
  `hl-time`.
- `src/linux_abi/syscall/dispatch.c` and the per-ISA service tables own C routing.
  Rust `syscall/` owns frames, narrow consumer ports, and canonical tables; it
  does not execute a runtime operation.

## Rust module contract

The crate root preserves the established public names (`EventAbi`, `NetworkAbi`,
`ProcessAbi`, `FutexPlan`, and their error types) as aliases. Inside each noun
module the redundant domain prefix is removed: the owner exposes `Abi`, `Error`,
`Plan`, `Operation`, and `WaitVector`. This keeps downstream API compatibility
without flattening implementation namespaces.

`process/copyout.rs` is a real ownership split rather than a line-count shard: it
owns the staged process-output transaction and resource-usage wire value, while
`process/abi.rs` owns request decoding and plan creation.

## Later API splits

- `futex/` currently contains both futex operation encoding and time/timer
  marshalling because the public `TimeFutexAbi` contract couples their deadlines.
  Split only after callers consume separate clock and futex ports.
- `filesystem/abi.rs` still coordinates several filesystem plan types. Further
  splits should follow stable operands or output transactions, not syscall-number
  groupings.
- The retained C syscall files depend on unity-translation-unit globals and host
  helpers. Those dependencies are behavioral oracle evidence, not dependencies to
  reproduce in `hl-linux`; concrete host calls belong in runtime adapters.
