# Native network migration audit

## Routed bind rollback and wildcard aliases

Audit base: Husklet `d6de96b48f93c98821810922e4fd5826d01d6011`.

Retained C implementation studied read-only:

- `../engine/src/linux_abi/syscall/net.c`: `net_precheck` and syscall case
  `bind`, including the bridge bind transaction around `lo_swap`, host `bind`,
  `udp_ref_create`, `br_alias_wildcard_listener`, `udp_ref_drop`, and the final
  restorative `lo_swap`.
- `../engine/src/linux_abi/container/netns.c`: `stream_swap`, `lo_swap`,
  `br_init`, `br_for_ip`, `br_bind_interface`, `br_path`,
  `br_alias_wildcard_listener`, `udp_ref_create`, `udp_ref_add_alias`,
  `udp_ref_dup`, `udp_ref_drop`, `udp_ref_process_exit`, and the fork reference
  preparation/cancellation hooks.

The C engine keeps guest socket identity in descriptor-indexed tables. A bridge
bind replaces the descriptor atomically with an AF_UNIX socket while preserving
its descriptor number, status/descriptor flags, and selected SOL_SOCKET options.
After the primary pathname bind succeeds, one refcounted `udp_ref` owns the
primary path and every wildcard alias across dup and fork. Any registration or
alias failure drops that ownership, unlinks all recorded paths, and replaces the
bound transport with a fresh unbound socket before returning the original errno.
Close and process exit release the final reference. No host call is made while a
shared table lock is held; the C tables rely on per-process descriptor indexing
and atomic cross-fork reference counts.

The Rust call graph is
`hl_network::NetworkPolicy::bind_route` ->
`hl_runtime::RuntimeNetworkHost::bind_route` -> `Native::switch_socket` -> host
`bind` -> alias creation -> `SwitchPath`, with `Native::close` releasing the
last `Arc<SwitchPath>`. `Reactor::sockets` owns descriptor and projected socket
state; `Reactor::switch_paths` maps accepted AF_UNIX paths back to the shared
path lease.

| Capability | Retained C owner | Rust owner | Status after this change |
|---|---|---|---|
| Select bridge interface and bounded path | `br_bind_interface`, `br_path` | `NetworkPolicy`, `Native::switch_path` | Implemented |
| Replace transport without changing descriptor identity | `stream_swap` | `Native::switch_socket` | Implemented |
| Preserve nonblocking and close-on-exec flags | `stream_swap` | `switch_socket`, `reset_switch_socket` | Implemented |
| Own primary and wildcard alias paths until final duplicate closes | `udp_ref_*`, `br_alias_wildcard_listener` | `Arc<SwitchPath>`, `switch_paths` | Implemented for descriptors adopted by one `Native` reactor |
| Remove every partially created path after alias failure | `udp_ref_drop` | temporary `SwitchPath` | Implemented |
| Restore an unbound guest-family socket after any routed bind failure | final `lo_swap` transaction rollback | `reset_switch_socket` | Corrected: restores AF_INET and clears projected state |
| Restore after primary bind refusal, including exhausted ephemeral search | final `lo_swap` transaction rollback | `bind_route` error paths | Corrected |
| Preserve selected SOL_SOCKET options across transport replacement | `lo_carry_opts`, `stream_swap` | none | Missing; separate coherent option-migration lane |
| Share path ownership across forked engine processes | shared `udp_ref` arena and fork hooks | reactor-local `Arc` | Divergent; requires the engine process/fork ownership domain |
| Release paths on raw process exit | `udp_ref_process_exit` | Rust destructor/normal close | Divergent; abnormal termination relies on stale-path recovery |

Ordering and error semantics: validation precedes descriptor replacement where
possible; after replacement, primary bind and alias publication either commit
all projected state or restore an unbound INET socket. The originating bind error
is returned when restoration succeeds. Restoration failure is returned instead,
because the original socket contract could not be recovered. There is no blocking
or cancellation state in bind itself. The path and address spelling is currently
IPv4-only in Rust, matching the admitted `BindRoute` shape; retained C also has
IPv6 wildcard handling, which remains outside this lane.
