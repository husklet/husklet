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
- `../engine/src/linux_abi/host_socket.h` and `host_poll.h`: POSIX direct
  descriptor behavior, Windows guest/host vocabulary translation, readiness
  sampling, and mixed-descriptor polling.
- `../engine/src/linux_abi/syscall/event.c`: poll/epoll registration, wakeup,
  timeout, oneshot, and spurious-wakeup ordering.

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

For the open-file-description lane, the retained POSIX path lets kernel file
descriptions own status flags, blocking, readiness, and final close. Dup aliases
therefore share the host OFD. Windows instead stores a refcounted socket object
behind handle-table entries; its critical section protects flags, connect state,
pending error, close state, and event delivery. Duplicate publishes another
handle entry holding the same object. Close first removes the handle entry, then
marks the object closing and signals its event so blocked receive/accept/connect
operations leave promptly; the last reference closes the Winsock socket and
event. `WSAEWOULDBLOCK`, `WSAEINPROGRESS`, and `WSAEALREADY` are distinguished,
blocking retries wait in bounded slices, and asynchronous completion is reported
as write readiness plus a latched one-shot error. POSIX errno passes through the
syscall boundary; Windows conditions are explicitly translated. Neither host
adapter contains guest-ISA branches.

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
| OFD-shared token, flags, and final close | kernel OFD on POSIX; refcounted Windows socket object | `SocketDescription` plus descriptor-owned aliases | implemented and concurrency-tested |
| blocking wake and cancellation | kernel poll/epoll; Windows object event and closing flag | `blocking.rs`, `ReadinessRegistry`, `OperationCancellation`, host `cancel` port | implemented and concurrency-tested |
| asynchronous connect/error latch | errno plus poll writability; Windows connecting/pending-error state | `SocketConnectStatus` under the description lock | implemented and unit-tested |
| host error vocabulary translation | native errno / typed Windows conditions | `platform.rs` consumer-owned port, converted to `ObjectError` | implemented; detailed Linux errno mapping remains personality-owned |

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

## Multi-interface launch and switch selection audit

This section records the retained implementation studied for the `HL_NETIFS`
launch projection and immutable Rust interface selection policy. It does not
claim that the Rust runtime already implements the retained AF_UNIX switch.

Read-only retained sources and entry points studied:

- `../engine/src/core/options.c`: `hl_option_definitions` and the
  `hl_options_{init,set,get,bind_process,destroy}` lifetime.
- `../engine/src/core/launch.c`: `cfd_read_full`, `launch_string`,
  `launch_strings_valid`, `hl_read_config_file`, and the
  `APPLY_OPTION("HL_NETIFS", ...)` projection.
- `../engine/include/hl/config.h`: `hl_launch_config` and its
  `network_interfaces_offset` wire field.
- `../engine/pkgs/rust/src/{config,network,wire}.rs`: the retained typed
  producer, eight-interface bound, legacy-field exclusion, and canonical
  newline serialization.
- `../engine/src/linux_abi/container/netns.c`: `HL_NETIF_MAX`, `br_interface`,
  `br_parse_ip`, `br_init`, `br_on`, `br_for_ip`,
  `br_connect_interface`, `br_bind_interface`, bridge path/alias/ephemeral
  allocation, UDP switch state and reference ownership, ICMP routing, and
  `dns_local_lookup`.
- `../engine/src/linux_abi/syscall/net.c`: bind and connect dispatch, including
  stream, datagram, and ICMP ordering.
- `../engine/src/core/target/{aarch64,x86_64}.c`: both targets include the same
  network namespace implementation; no assembly participates in this domain.

The launch reader owns the decoded wire and option strings for the runner's
lifetime. `br_init` copies selected values into process-global bounded arrays on
first use. It sets its one-shot flag before parsing, has no lock or retry, and
has no interface-state teardown; bridge directories persist. Per-descriptor TCP
and UDP interface identities are copied on dup/fork, while UDP paths use shared
atomic references so the last alias removes owned endpoints. Rust instead owns
parsed interfaces immutably per engine in `NetworkPolicy`.

`HL_NETIFS` takes precedence over legacy `HL_NETBR` plus `HL_IP`. Records are
`bridge=IPv4/prefix` in launch order, with bridge length 1 through 40, a nonzero
IPv4 address, prefix 0 through 32, and at most eight entries. Retained parsing
stops at the first malformed row while preserving earlier rows and ignores rows
beyond eight; the Rust producer rejects invalid resolved configuration before
launch, while the Rust decoder rejects the entire malformed record set. Legacy
input creates one `/16` interface. Bridge directory creation uses mode 0700;
POSIX passes the mode and Windows' compatibility wrapper drops it.

Connect excludes `127/8` and selects the first launch-ordered subnet match,
including for overlapping prefixes. Bind excludes `127/8`, assigns the
unspecified address to interface zero, performs a complete exact-own-address
scan, and only then performs the ordered subnet scan. Prefix zero matches every
non-loopback address; prefix 32 matches only the configured address. TCP bind
creates wildcard aliases for the other attached interfaces. UDP persists the
selected local and peer interface across bind/connect/send; ICMP rejects an
unmatched non-loopback destination with `ENETUNREACH`; DNS scans each bridge's
`.names` file in interface order. Path validation can return `EINVAL` or
`ENAMETOOLONG`; ephemeral exhaustion returns `EADDRINUSE`; connect preserves
host errors after bounded retry. Selection is common across host and guest
architectures; only socket and directory adapters vary by host.

| Capability | Rust owner | Status |
|---|---|---|
| ordered, bounded `HL_NETIFS` launch serialization | `hl-container` engine spec | implemented with strict producer validation |
| immutable interface parsing and namespace inventory | `hl-network::NetworkPolicy` | implemented |
| ordered connect and exact-before-subnet bind selection | `hl-network::NetworkPolicy` | implemented and focused-unit-tested |
| selected-interface propagation into live TCP/UDP operations | `hl-network::EgressRoute` plus `hl-runtime::RuntimeNetworkHost` | implemented through the host port; existing host defaults still ignore identity |
| AF_UNIX bridge rendezvous, wildcard aliases, retry, and endpoint teardown | network domain plus runtime descriptor adapter | missing |
| per-interface `.names` lookup and ICMP source selection | network/runtime composition | missing |

Focused evidence covers two serialized interfaces, stable order and distinct
prefixes, bounds and malformed fields, overlapping-prefix connect order,
exact-own-address bind priority, wildcard bind, `/0`, `/32`, loopback exclusion,
and the distinction between the synthetic introspection interface and configured
switch membership. Full TCP/UDP switch behavior remains a later compatibility
gate and must not be inferred from these tests.
