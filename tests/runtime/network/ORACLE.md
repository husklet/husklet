# Network compatibility oracle audit

## Native switch pathname publication

The publication lifecycle was compared with retained
`../engine/src/linux_abi/syscall/net.c` bind, alias-publication, fork/duplicate,
and final-close paths and `../engine/src/host/linux/host.c` host bind/close
adapters. The retained owner publishes the primary AF_UNIX pathname before its
aliases, records only aliases it created, reverses them on partial failure, and
keeps the publication with the shared socket lifetime until final close. Host
calls occur without a guest descriptor-table lock. Publication has no guest-ISA
branch and does not change routing or checkpoint schema.

Rust keeps the same ownership in the native adapter. `PreparedPublication`
owns the successfully created primary and alias pathnames until the complete
set can be attached to the socket `Entry`; dropping it reverses only those
paths. Publication transfers that set once into the existing `Arc<SwitchPath>`.
Duplicate descriptors discover and share that owner through the reactor's weak
pathname registry, while the last `Entry` close drops the final strong owner
and unlinks each pathname exactly once. A pre-existing foreign alias is never
unlinked. Tests cover injected partial-alias failure, a foreign-path collision,
concurrent primary publication, socket reset after rollback, duplicate
ownership, and final-close cleanup through the production native host path.

The retained implementation was studied read-only in
`../engine/src/linux_abi/syscall/net.c`, especially `svc_net`,
`net_precheck`, `net_sockaddr_copyout_begin`, `net_message_bounce_begin`, and
the send/receive message, option, accept, connect, bind, shutdown, ioctl, and
poll branches. Supporting ownership and lifecycle paths were checked in
`../engine/src/linux_abi/syscall/binding.c` (socket syscall binding and
`SCM_RIGHTS`), `../engine/src/linux_abi/syscall/event.c` (epoll descriptor
readiness), `../engine/src/linux_abi/syscall/nonpie_args.h` (guest pointer
admission), `../engine/src/linux_abi/host_poll.h` (host readiness), and
`../engine/src/linux_abi/fork.c` (descriptor transfer during fork). The
namespace and checkpoint call graphs were also followed through
`../engine/src/linux_abi/container/netns.c` (`sock_object_new`,
`sock_pair_identity_assign`, `fd_carry_sock`, `netns_tcp_bind_note`,
`netns_tcp_listen_note`, `netns_tcp_emit`, `br_init`, `br_for_ip`, and endpoint
reset), `../engine/src/linux_abi/checkpoint.c` (`ckpt_capture_socket_state`,
`ckpt_capture_socket_queue`, `ckpt_prepare_restore_sockets`, and the socket
queue/state restore passes), and `../engine/src/linux_abi/host_socket.h` plus
`../engine/src/host/windows/socket.c` (POSIX and Winsock creation, readiness,
error translation, cancellation, and final close).

The C engine records socket family, type, protocol, peer projection, and
private-network routing beside each host descriptor. `svc_net` validates guest
addresses and flag widths before host calls, translates Linux sockaddr,
msghdr/iovec/control layouts in both directions, and commits output lengths
only after successful guest copies. Blocking calls use host readiness and
signal interruption; nonblocking operations preserve `EAGAIN`,
`EINPROGRESS`, `SO_ERROR`, partial transfers, and message boundaries. Accepted
and duplicated descriptors inherit open-file-description state while
`CLOEXEC` remains descriptor-local. Last close releases host/private endpoints.
Fork transfers live descriptors and reconstructs their recorded identity.
The POSIX implementation relies on kernel open-file-description locking and
readiness. The Windows adapter instead protects its refcounted socket object,
connect state, pending error, and close transition with a critical section and
wakes blocked operations through its event before the last reference closes
the Winsock handle. Checkpoint runs while the process is quiescent, captures
each shared socket object once, and restores peer topology before descriptor
aliases and queued rights. No network state is owned by guest-ISA assembly;
guest syscall numbering is selected above this domain.

Linux hosts mostly delegate INET and UNIX mechanics after ABI validation.
The macOS branch translates option, message, ioctl, and sockaddr constants and
uses private AF_UNIX routing to model container loopback, UDP, DNS, ICMP, and
netlink behavior. Those branches explain the manifest's explicit macOS
unsupported cases. The abortive-linger and half-open TCP cases remain typed
known bugs because the private AF_UNIX transport cannot reproduce the required
TCP reset/HUP ordering.

| Retained C capability | Rust owner | Coverage in this folder |
|---|---|---|
| socket family/type/protocol creation and policy | `hl-network::SocketNamespace`, `hl-runtime::network::syscalls` | socket matrix, socketpair, TCP/UDP/UNIX/IPv6/ICMP |
| bind, connect, listen, accept4 and shutdown state transitions | `hl-network::SocketDescription`, `hl-runtime::network::{syscalls,accept}` | connect edges/refusal, backlog, nonblocking connect, shutdown |
| sockaddr and option ABI conversion with in/out lengths | `hl-linux::network::abi`, `hl-runtime::network::{types,options}` | names, buffer/type/linger/keepalive/IP/TCP options |
| stream/datagram partial I/O and readiness | `hl-runtime::network::{data,wait}`, `hl-event` adapter | peek/waitall/truncation, poll, epoll, FIONREAD, timeouts |
| msghdr/iovec/control translation and batching | `hl-runtime::network::{message,transfer}`, `hl-network::ancillary` | sendmsg, recvmsg, writev, mmsg, credentials and rights |
| descriptor identity across dup, fork, close, and `SCM_RIGHTS` | `hl-descriptor`, `hl-runtime::fork::network`, `hl-runtime::network::import` | dup socket, private UDP fork, SCM_RIGHTS |
| UNIX pathname/abstract/autobind namespace | `hl-network::unix`, runtime VFS pathname adapter | stream/datagram/seqpacket/abstract/autobind/name cases |
| interface, netlink, DNS, ICMP and procfs projections | `hl-runtime::network::{ioctl,netlink}`, `hl-runtime::procfs::network`, concrete host adapter | interfaces/ioctl/netlink/DNS/ICMP/tcp-row |
| cancellation, snapshot and teardown ownership | `hl-network::{blocking,checkpoint}`, `hl-runtime::checkpoint::network` | timeout, peer-close, fork/dup and last-close behaviors |

All 87 legacy registrations are represented independently in `test.yaml` with
their original targets, compile flags, environment, expected exit status, and
byte-exact golden output. `active` means the compatibility contract remains
enabled. `!unsupported` preserves an explicit host limitation and `!broken`
preserves a known behavioral divergence; neither is silently treated as a
passing case.

The provider-loopback source was audited against
`../engine/include/hl/host_services.h`, `../engine/src/linux_abi/syscall/binding.c`, and
the epoll/descriptor integration. Injected socket/file identities survive dup
and fork, `CLOEXEC` closes only the descriptor edge, and readiness follows the
provider subscription lifetime. The folder runner cannot yet construct that
typed injection, so the case is visibly unsupported. Rust ownership maps to
provider-backed `hl-network`/`hl-vfs` objects and `hl-runtime` adapters.

## Provider evidence

| Gate | Result | Meaning |
|---|---:|---|
| AArch64/x86-64 cross-build | 174/174 | all 87 retained registrations build for both ISAs |
| active direct QEMU rows | 142/154 | byte-exact provider parity; 12 host-environment differences listed below |
| typed unsupported/broken rows | 20 rows | enumerated but deliberately excluded from the active provider count |
| provider-loopback cross-build | 2/2 | source builds for both ISAs |
| provider-loopback direct QEMU | 0/2 | expected step-1 `ENOENT` without typed descriptor injection |

On 2026-08-04, a direct source-provider gate used all 18 logical CPUs and fresh
temporary outputs. Both pinned cross-compilers successfully built all 87
manifest registrations: 174/174 AArch64 and x86-64 static artifacts. Source
bytes match the retained fixtures after the intentional include rename from
`net_util.h`/`net_socket_util.h` to `socket_util.h`; all 87 golden files are
byte-identical to the retained expected output.

Direct QEMU execution is provider evidence, not Rust-engine status. Of 154
active registration/ISA rows, 142 matched exit status and stdout byte-for-byte.
Two initially colliding abstract-socket rows passed when rerun sequentially.
The remaining 12 rows are the same six host-environment-sensitive contracts on
both ISAs: DNS injection, fixed interface projection, private bridge ICMP,
forced IPv6 unreachable routing, dual-stack bind policy, and one host-specific
IP socket-option value. These results do not reclassify their typed engine
status. The ten explicitly unsupported/broken registrations remain visible but
were not counted as QEMU active rows.

`provider-loopback` is an additional residual provider contract beyond those
87 registrations. Its source and `provider-loopback ok\n` golden are preserved,
but direct native execution cannot pass because the retired typed provider must
inject its descriptors; its typed unsupported status records that runner gap.

## Dual-stack bridge wildcard audit

The live container routing lane additionally followed retained
`syscall/net.c::svc_net` bind, listen, accept, and connect cases through
`container/netns.c::{br6_any_is,br_bind_interface,br_path,br_v6only_path,
br_alias_wildcard_listener,fd_carry_sock,udp_ref_create,udp_ref_dup,
udp_ref_drop}`. An IPv6 unspecified stream bind joins the first configured
IPv4 bridge; `IPV6_V6ONLY` uses a distinct `.v6only` rendezvous, while a
dual-stack listener owns the ordinary IPv4 path and wildcard aliases. The
AF_UNIX inode remains owned until the last duplicate/fork reference closes.
Bind failures retain `EINVAL`, `EADDRINUSE`, and path-length errors; listen and
accept keep their ordinary blocking, cancellation, signal, and partial-I/O
paths. This mechanism has no guest-ISA branch. Windows leaves the private
AF_UNIX namespace disabled; POSIX hosts use the switch path.

| Capability | Rust owner | Status |
|---|---|---|
| IPv6 `::` selects the first configured bridge | `hl-network::NetworkPolicy::bind_route` | implemented |
| wildcard listener aliases every attached bridge | `hl-network::BindRoute`; native switch path ownership | implemented |
| dual-stack listener owns the IPv4 rendezvous | `hl-engine` native `RuntimeNetworkHost::bind_route` | implemented |
| `IPV6_V6ONLY` remains isolated from IPv4 peers | native socket option projection and `.v6only` path | implemented |
| specific IPv6 binds remain on the native IPv6 stack | `hl-network::NetworkPolicy` | implemented |
| policy-selected bridge listeners invoke host listen/accept | `hl-runtime::RuntimeNetworkSyscalls` | implemented |
| last-close cleanup and failed-bind rollback | native `SwitchPath` ownership and socket restoration | implemented |

## Bind reservation transaction audit

The retained switch bind path in
`../engine/src/linux_abi/syscall/net.c::svc_net` validates the guest address,
selects the loopback or bridge identity, binds the substituted socket, records
the refcounted port owner, and rolls that ownership back if later publication
fails. `../engine/src/linux_abi/container/netns.c::{udp_ref_create,
udp_ref_drop}` keeps the reservation through duplicate and fork lifetime and
removes it on final close. These operations have no guest-ISA branch; host
calls occur outside retained descriptor-table bookkeeping locks.

The current Rust checkpoint schema represents the corresponding namespace
claim as `PortCheckpoint`. `NetworkCatalog::prepare_host_bind` reserves that
identity under one deterministic `slots` then `ports` critical section while
retaining the exact current socket object and a unique reservation generation.
It releases both locks before returning. Commit reacquires the same order,
verifies exact object and reservation identity, and atomically publishes the
bound snapshot and port; dropping or explicitly rolling back the must-use guard
removes only its pending reservation. The live isolated bind path uses the
transaction, so no catalog lock crosses host work and early return cannot leak
a claim. Native alias publication and expanded checkpoint identities remain a
separate capability gap.
