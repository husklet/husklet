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

## Selected domain: versioned daemon negotiation

Docker CLI 29.1.3 defaults to its current Engine API prefix (`/v1.52`) and does
not automatically downgrade to this daemon's truthfully advertised v1.43
contract.  Using Docker's standard `DOCKER_API_VERSION=1.43` compatibility
selection reaches the implemented API, but `docker version` still failed:
`/_ping` and unversioned `/version` existed while the versioned API router omitted
`/v1.43/version`.  Docker's version endpoint is valid both unversioned (for
negotiation) and under a supported prefix (for the selected contract).

Concrete reproduction from commit `cf15cdd33e538956ef42d37094d877268a9cda4e`:

```text
DOCKER_API_VERSION=1.43 docker version
request returned 404 Not Found for API route ... /v1.43/version

curl --unix-socket /tmp/.../docker.sock http://localhost/version
{"ApiVersion":"1.43","MinAPIVersion":"1.24",...}
```

### Retained C oracle audit

The retained engine has no Docker HTTP router.  Its corresponding versioned
admission and readiness protocol is the complete domain in
`/Users/x/dd/engine/src/core/activation.c`, studied through `transfer`,
`activation_prepare`, `activation_handshake`, both POSIX and Windows
`activation_start` arms, `hl_activation_child`, `hl_activation_wait`,
`hl_activation_try_wait`, `hl_activation_kill`, and
`hl_activation_process_destroy`.  Supporting declarations were studied in
`/Users/x/dd/engine/include/hl/activation.h`; there is no assembly entry in this
protocol domain.

- State and identity: the parent-owned `hl_activation_process` retains the
  control descriptor, child/process-domain identity, nonce, completion flag,
  cached terminal status and exit, and Windows process/job handles.  The request
  and reply carry exact magic, ABI, size, and nonce identity.  State lives until
  destroy; destroy kills and waits before releasing it.
- Ordering and partial I/O: `transfer` completes the entire bounded record,
  retrying `EINTR` and rejecting EOF or other partial termination.  The parent
  sends the request and attached roles, validates the complete reply, then sends
  one commit byte.  The child cannot enter the guest before that commit.  Wait
  validates the terminal reply and host exit together before caching completion.
- Blocking, cancellation, signals, and teardown: handshake and wait are
  blocking; `try_wait` polls without blocking and retries `EINTR`.  POSIX kill
  first targets the process group and then drains the nonce-owned process domain;
  Windows terminates the job.  Repeated domain termination is successful, while
  killing a finished activation returns busy.  POSIX signal delivery uses an
  async-safe self-pipe and a mutex only in the relay thread; no global lock spans
  transfer or wait.
- Errors: malformed magic/ABI/size/nonce, truncated transfer, invalid descriptor
  roles, a missing commit, or inconsistent child exit is corruption, never a
  readiness claim.  Invalid caller values return invalid argument; host spawn,
  wait, pipe, and job failures return platform failure; allocation is distinct.
- Host and architecture branches: POSIX uses socketpair, descriptor passing,
  process groups, `posix_spawn` (or `fork` for a controlling terminal), and
  birth-recorded process domains.  Windows uses a named pipe, explicit inherited
  handles, `CreateProcessW`, and nested job objects, and refuses terminals.  The
  request admits both AArch64 and x86-64; backend availability remains the engine
  owner's decision.

### Capability matrix

| Retained C capability | Rust owner | Status before this lane |
|---|---|---|
| exact minimum/implemented version validation | `hl-daemon::api::http::router` version prefixes | implemented for 1.24-1.43 |
| complete bounded protocol transfer before readiness | `hl-daemon::Server` plus Axum request parsing | implemented |
| version discovery before selecting a contract | unversioned `/_ping` and `/version` | implemented |
| version report after selecting a supported contract | versioned `/version` | missing: routing 404 |
| truthful implemented and minimum version report | `http::system::{ping,version}` | implemented (1.43/1.24) |
| malformed/unknown operation refusal | Axum routing and `ApiError` | implemented |
| instance-owned lifecycle and teardown | `Server`, `SocketGuard`, and `Containers` | implemented |
| POSIX/Windows launch mechanism and ISA selection | Rust engine activation/composition | separate owner; no HTTP divergence |

The coherent change routes the same truthful version report through every
already-supported prefix.  It does not admit API 1.44 or later, alter response
models, or expand product scope.

### Docker 29 default-version blocker and supported workflow follow-up

After the versioned route was added, a bounded `strace` of Docker CLI 29.1.3
showed this exact sequence with no `DOCKER_API_VERSION` override:

```text
HEAD /_ping
HTTP/1.1 200 ... api-version: 1.43 ...
HEAD /_ping
HTTP/1.1 200 ... api-version: 1.43 ...
GET /v1.52/version
HTTP/1.1 404 Not Found
```

Both `/version` and `/v1.43/version` independently returned 200 with
`ApiVersion=1.43` and `MinAPIVersion=1.24`.  The Docker 29 client binary contains
an explicit compiled minimum-API refusal and will not negotiate down to 1.43;
`DOCKER_API_VERSION=1.43` is required for this older server contract.  Advertising
or aliasing v1.52 would claim response and request semantics this daemon has not
implemented, so the router continues to reject it.

The supported-version workflow then exposed a generic wire-format divergence:
Docker CLI spells query booleans as `1` and `0` (for example `docker ps -a`
sends `all=1`), while several handlers deserialized directly to Rust `bool` and
accepted only `true` and `false`.  The HTTP query owner now consistently accepts
Docker's `1`, `0`, case-compatible true/false spellings, and an empty false value,
while rejecting every other spelling.  The inventory covers build `nocache`,
`rm`, and `forcerm`; archive `copyUIDGID` and `noOverwriteDirNonDir`; container
list `all`; container removal `force` and `v`; system prune `volumes`; and volume
removal `force`.  Log, stats, inspect-size, image-list, and image-removal options
already pass through explicit string parsers rather than direct bool
deserialization and retain their existing validation.

## Previous domain: daemon readiness and negotiation

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
