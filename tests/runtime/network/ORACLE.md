# Network catalog oracle

This audit covers the socket identity/catalog and namespace-observation domain
owned by `hl-network`. It supports the catalog unit and checkpoint tests; it does
not claim full network syscall parity.

## Retained C audit

Read-only files and entry points studied:

- `../engine/src/linux_abi/syscall/net.c`: socket syscall dispatch for
  `socket`, `socketpair`, `bind`, `listen`, `accept`, and `connect`.
- `../engine/src/linux_abi/container/netns.c`: the `g_sock_*`, `g_lo_*`,
  `g_br_*`, and `g_tcp_*` socket catalogs; `sock_object_new`,
  `sock_object_pair`, `fd_carry_sock`, `netns_tcp_bind_note`,
  `netns_tcp_listen_note`, `netns_tcp_emit`, and the close/reset paths.
- `../engine/src/linux_abi/container/vfs.c`: `/proc/net/tcp` and
  `/proc/net/tcp6` synthesis through `netns_tcp_emit`.
- `../engine/src/linux_abi/checkpoint.c`: `ckpt_socket_state`,
  `ckpt_capture_socket_state`, `ckpt_capture_socket_queue`,
  `ckpt_prepare_restore_sockets`, `ckpt_restore_socket_queue_load`,
  `ckpt_prepare_restore_socket_states`, and socket teardown.
- `../engine/src/host/windows/socket.c`: Windows host socket creation, bind,
  listen, accept, connect, close, and AF_UNIX-pair construction adapters.

The retained engine indexes process-global, bounded arrays by guest descriptor.
An atomic nonzero sequence supplies socket-object identity; socket pairs record
both endpoint identities, while dup copies the same endpoint identity and all
emulation metadata. The descriptor lifecycle owns reset and final peer cleanup.
There is no catalog-wide mutex: syscall/process serialization and fixed array
slots provide the C ownership model, while checkpoint uses a stopped/quiescent
process phase. Listen-table observation walks the arrays without a second state
object. Checkpoint capture records each shared object once, captures queued
frames and rights, and restores peer topology before descriptor aliases. Restore
recreates host sockets, options, bind state, listen backlog, and queued traffic;
failure preserves the host errno in the syscall path or aborts the restore
transaction. AF_UNIX pathname length handling has a macOS-specific `fchdir`
fallback, and Windows uses Winsock/AF_UNIX adapters. Guest syscall numbering is
architecture-specific above this domain; socket catalog identity is not.

## Capability matrix

| Capability | Retained C owner | Rust owner | Status |
|---|---|---|---|
| stable socket identity and descriptor aliasing | `g_sock_object`, `g_sock_peer_object`, `fd_carry_sock` | `NetworkCatalog` generation-qualified `SocketId`; descriptor aliasing remains descriptor-owned | implemented, different safer ownership |
| bounded allocation and stale identity rejection | `HL_NFD` arrays and reset | `NetworkCatalog::{allocate,slot,slot_mut}` | implemented |
| paired endpoint identity/lifetime | `sock_object_pair`, peer arrays | `CatalogSocket::UnixPair` shared by two slots | implemented |
| listener pending FIFO/backlog | host accept plus backlog metadata | `catalog/unix.rs` atomic connect/accept transaction | implemented and unit-tested |
| namespace-wide coherent observation | unlocked `netns_tcp_emit` array walk | `NetworkCatalog::namespace_view` under one slots lock | implemented; Rust is coherent |
| port ownership and collision admission | per-fd loopback/bridge port arrays | `NetworkCatalog::claim_port` and owned `PortCheckpoint` set | implemented |
| catalog mutation/checkpoint exclusion | stopped process checkpoint phase | `CheckpointActivity` admission/freeze/thaw | implemented and concurrency-tested |
| socket/pair checkpoint topology | socket state and endpoint restore tables | `catalog/checkpoint.rs` plus `NetworkSocketState` | implemented and unit-tested |
| host resource recreation and accepted sockets | host `socket`/bind/listen plus restore tables | `NetworkCatalogRestore` consumer port | implemented |
| queued payload and `SCM_RIGHTS` checkpoint | queue frame capture/restore | Unix transport snapshots and descriptor/runtime adapters | partial; full cross-domain parity remains to be proven |
| host-specific socket mechanics | POSIX paths and Windows Winsock adapter | `NetworkSocketResource` adapters selected above `hl-network` | implemented boundary; target evidence remains required |

## Semantics and remaining evidence

Catalog operations hold the Rust slots mutex only for in-memory identity and
snapshot transitions. Host reconstruction is intentionally performed while
building a not-yet-published catalog, matching the retained restore transaction;
ordinary live catalog operations do not hold this lock across host calls.
`Capacity`, `Stale`, `Invalid`, and checkpoint errors remain typed here and are
converted to Linux errno only by the Linux personality. The focused unit tests
prove allocation generations, atomic AF_UNIX connection rollback, backlog FIFO,
freeze admission, and checkpoint validation. Full syscall errno, blocking,
cancellation, cross-process descriptor passing, both guest ISAs, and Windows
restore behavior require the broader compatibility gates and are not claimed by
this structural lane.
