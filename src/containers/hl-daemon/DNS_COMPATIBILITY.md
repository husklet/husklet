# Docker resolver capability audit

This note records the implementation-oracle audit for Docker `HostConfig.Dns`,
`HostConfig.DnsSearch`, and `HostConfig.DnsOptions`.

## Wire and lifecycle contract

Docker's create API accepts ordered lists of nameserver addresses, search domains,
and resolver options. The lists are container policy: they survive create, appear
in inspect, and determine the generated `/etc/resolv.conf` used by the initial
process and every later exec. Nameservers are addresses in the container's network
namespace; a loopback address therefore means container loopback, not host loopback.

Husklet owns the same state in `hl_container::ContainerSpec::resolver`. Creation
validates and transfers the Docker fields before the container record is committed.
`hl-container::Identity::{prepare,open,refresh}` owns the identity-file lifetime:
creation writes an atomic temporary file and renames it, exec reopens the same
persisted file, network refresh rewrites it, and container removal deletes the
per-container identity directory. No process-global resolver state or lock is used.

## Retained C oracle

The read-only retained engine was inspected at these entry points:

- `/Users/x/dd/engine/src/linux_abi/container/netns.c`:
  `dns_enabled`, `dns_dest_is`, `dns_swap`, `dns_build_response`, `dns_send`, and
  `dns_try_send`;
- `/Users/x/dd/engine/src/linux_abi/syscall/net.c`: socket `connect`, `sendto`,
  `sendmsg`, and `sendmmsg` dispatch paths that recognize `127.0.0.11:53`;
- `/Users/x/dd/engine/src/linux_abi/syscall/io.c`: write and vectored-write paths
  for an already intercepted DNS socket;
- `/Users/x/dd/engine/src/linux_abi/dns.c`: host resolver preparation and lookup.

The retained engine does not own Docker's resolver lists or generate
`/etc/resolv.conf`. It intercepts only the built-in `127.0.0.11` address and keeps
per-descriptor DNS state in `g_dns_sock`/`g_dns_peer`, cleared with descriptor
teardown. Custom nameserver addresses follow the ordinary guest socket path. Query
staging is bounded, malformed names are rejected while parsing, and multi-message
sends preserve Linux partial-result ordering. There is no architecture-specific
resolver configuration branch; socket ABI decoding remains architecture-specific
at the syscall boundary.

The Rust ownership mapping is therefore:

| Capability | Rust owner |
|---|---|
| Docker request spelling and address decoding | `hl-daemon::api::model::HostConfig` |
| token bounds and durable resolver policy | `hl-container::Resolver` |
| create-time transfer into persisted state | `hl-daemon::HostSettings` |
| atomic identity-file generation and reuse | `hl-container::Identity` |
| inspect projection | `hl-daemon::api::model::HostInspection` |
| built-in and ordinary DNS socket execution | `hl-runtime` network path |

The implementation deliberately does not add an application-specific resolver
branch or teach the engine about Docker request fields.

## Evidence required before integration

1. Warning-strict `hl-container` tests, including exact generated identity text.
2. Warning-strict `hl-daemon` library and API tests, including create transfer,
   invalid-token refusal, persistence, and inspect projection.
3. A live Docker-surface create/start/exec check that reads `/etc/resolv.conf` and
   confirms the ordered nameserver, search, and option records.
4. The repository dependency/design lint for the changed packages.
