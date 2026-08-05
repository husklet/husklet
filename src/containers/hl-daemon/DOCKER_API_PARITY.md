# Docker API parity inventory

This inventory compares the local, single-node Docker Engine API v1.43 surface
with the routes composed in `api::http::router`. Swarm orchestration, plugins,
secrets, configs, services, nodes, and session transport are outside the
container daemon's product boundary rather than silent compatibility claims.

| API domain | Implemented contract | Remaining bounded gaps |
|---|---|---|
| daemon negotiation | `_ping`, `version`, `info`, auth, plugins | registry auth validation is local policy |
| containers | create, list, inspect, diff, update, export/archive, logs, top, stats, attach, lifecycle, wait, remove | container namespace sharing and several privileged create controls |
| exec | create, inspect, start/attach, resize, signal, wait, remove | detached process retention policy |
| images | build/prune, list, inspect, history, tag, commit, pull/push/search, load/save/remove, distribution | advanced BuildKit/session options |
| networks | list/create/inspect/remove/prune/connect/disconnect | IPv6, overlay, config-only, ingress, custom IPAM ranges |
| volumes | list/create/inspect/remove/prune | external drivers and cluster volumes |
| events and accounting | events, system disk usage, system prune | swarm-scoped event kinds and object classes |

## Selected domain: daemon readiness and negotiation

The route existed but returned only the `OK` body. Docker-compatible clients use
the unversioned ping as the readiness boundary and consume its headers before
issuing versioned work. The endpoint now publishes the API, builder,
experimental, OS, and swarm capability headers on both GET and HEAD (Axum's GET
route supplies HEAD with the same headers and no body). This closes the highest
value gap without advertising unsupported orchestration domains.

## Retained C oracle audit

The retained engine has no Docker HTTP implementation. The corresponding
readiness/lifecycle oracle is `/Users/x/dd/engine/src/core/activation.c`:
`activation_start`, `activation_handshake`, `hl_activation_child`,
`hl_activation_wait`, `hl_activation_kill`, and
`hl_activation_process_destroy`, including their POSIX and Windows arms.

- Ownership and lifetime: the parent owns the opaque activation process and its
  control channel until wait/destroy; the child owns the engine and adopted
  descriptors after the nonce/ABI request is validated. Destroy kills and waits
  before releasing the process object.
- Ordering: the parent sends a magic/ABI/nonce-stamped request, requires the
  child's validated reply, sends a commit byte, and only then exposes a live
  process. Exit is a later reply. EOF before commit prevents guest execution.
- Locking and cancellation: the point-to-point control channel requires no
  process-global lock. POSIX waits retry `EINTR`; teardown kills the complete
  process domain and drains it. Windows substitutes a named pipe and job object,
  preserving the same protocol ordering.
- Errors: malformed handshakes and partial transfers fail activation rather than
  claiming readiness. Repeated domain termination succeeds; stale POSIX process
  records are reclaimed, while unexpected host failures remain platform errors.
- Architecture and host mapping: the wire carries the guest ISA but readiness
  ordering is ISA-neutral. POSIX uses socketpair/SCM_RIGHTS and process groups;
  Windows uses a named pipe, inherited duplicated handles, and a job object.
- Rust mapping: `hl-daemon::Server::bind` owns socket publication,
  `serve_loop` owns accepting and draining connections, and `SocketGuard` removes
  only the inode it published. `system::ping` is the protocol commit point: it
  reports readiness only after a connection reached the bound router and now
  returns explicit, truthful daemon capabilities. The retained nonce handshake
  has no HTTP analogue and remains owned by the Rust engine activation adapter.

There are no locking, errno, partial-I/O, or guest-architecture branches inside
the ping response itself; those stay in the listener and transport owners.
