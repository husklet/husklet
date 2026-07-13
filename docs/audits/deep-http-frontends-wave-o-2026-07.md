# HTTP/API/frontends deep audit — wave O (2026-07)

Documentation-only route-to-product-flow audit. The daemon registers 59 Docker API method/path entries
(counting archive's GET/PUT/HEAD separately). Each route was traced to its handler, serialization shape,
tests, client facade, and CLI/GUI use.

## Compatibility endpoints that are intentionally empty

Two handlers return empty collections:

- `GET /plugins` → `system.rs:308` returns `[]`;
- `GET /images/search` → `images/query.rs:83` returns `[]`.

Neither has a `dd-client` method or GUI/CLI product action. They are nevertheless Docker API discovery
surfaces: removing the registrations changes a valid empty capability response into the daemon's 404
fallback. Keep the tiny handlers/routes unless the advertised API profile is formally narrowed. Their
implementation is the required stub; there is no removable internal implementation behind them.

## Registered “success” surfaces requiring truthful migration

These are not behavior-neutral cuts, but their present defaults can mislead external Docker clients:

- `POST /auth` ignores the request body and always returns “Login Succeeded” with an empty token
  (`system.rs:123-132`). It has no internal client/frontend caller. Preserve the route for compatibility,
  but return an explicit unsupported/authentication error unless registry credentials are actually
  validated. False success is worse than a typed failure.
- `GET /distribution/:name/json` fabricates a digest from the name, size zero, and a single linux/arm64
  platform (`images/query.rs:127-139`) without consulting local or registry manifests. No product caller
  uses it. Either implement it from `dd-images` manifest data or return a truthful unsupported/not-found
  response; do not expose invented metadata.
- `GET /containers/:id/top` returns one synthetic PID-1 row using the configured command
  (`containers/inspect/top.rs:5-34`). It correctly rejects stopped containers, but a running multiprocess
  container is under-reported. Keep the route and serialized Docker shape; migrate the body to the live
  process tree rather than deleting it.

`system/df` uses documented Docker sentinels for unavailable rw/volume/build-cache accounting. Unlike the
three cases above, the client facade and GUI consume it, and image/container counts/sizes are real. Keep.

## Client facade and GUI/CLI reachability

All 20 public `dd-client::Client` methods have at least one caller beyond their definition. The lowest-use
methods are `disk_usage` and `container_logs` (two occurrences each: definition plus GUI snapshot/terminal
flow). Network, volume, image, and lifecycle mutations are wired through GUI messages; list/system calls
feed periodic snapshots. No client method is a proven safe deletion.

The client deliberately exposes only the GUI/CLI subset of the Docker API. Routes with no facade method
(build, archive, exec, attach, events, stats streaming, push/pull, compose-facing network operations) are
still exercised by the real Docker CLI and scenario suites. “No Rust caller” is therefore not evidence of
unreachability.

Every `ddcli` subcommand reaches dispatch. `App`, install/context/daemon/doctor, workspace actions, `Run`,
`Mac`, and external image shorthand all have direct module flows. GUI resource views map to client list and
mutation methods; no entire registered view is unreachable. The definition-only widget helpers identified
in waves F/I remain the only exact GUI symbol cuts.

## Required wire fields versus removable internals

Docker handlers that accept but do not act on query/body fields must retain those serde fields so strict
clients deserialize and negotiate correctly. Examples include attach `stream`/`logs`, exec-start fields,
and container-delete `link`. Remove neither field nor rename; item-level dead-code annotations document
the compatibility role.

Likewise, empty arrays, zero/sentinel sizes, PascalCase keys, and explicit acronym renames are observable
wire behavior. A handler may be internally simple without being dead. Route removal requires Docker CLI,
bollard, compose, and serialized golden evidence, not only product-GUI reachability.

## Behavior-neutral cleanup

- Remove the definition-only `_touch` helper/import from daemon test support (wave L).
- Remove stale comments saying an endpoint was “previously unrouted” once route history is no longer useful
  (for example `system_prune`); tests and route registration own that fact.
- Replace broad route-module glob imports with explicit handler imports only after all 59 entries remain
  compiler-checked. This reduces accidental exports without changing routing.
- Keep `plugins_list` and `image_search` implementations adjacent to explicit “compatibility empty”
  comments so future dead-code passes do not repeatedly flag them.

No handler/helper beyond previously documented candidates is safely deletable: symbol-only helpers feed
registered routes even when no Rust frontend calls them.

## Performance boundaries

Do not consolidate streaming endpoints into buffered client helpers. Events, logs, image pull/build output,
stats, archive transfer, attach, and exec hijack deliberately stream or upgrade connections; buffering them
would increase latency/memory and can deadlock interactive flows. The GUI's `container_logs` currently
collects bytes for a bounded display use, but that does not authorize changing daemon streaming semantics.

Route registration itself is negligible. Removing compatibility endpoints provides no meaningful runtime
gain; correctness and client negotiation dominate. Expensive filesystem work (`system/df`, changes,
export) is already request-driven, with overlay diff offloaded through `spawn_blocking`. Preserve that
separation.

## Ordered plan

1. Keep empty plugin/search compatibility routes and document them explicitly.
2. Add Docker-wire tests for auth/distribution/top, then replace fabricated success with truthful behavior.
3. Retain all 20 client methods and all GUI/CLI product actions on current evidence.
4. Apply only comment/import/helper cleanup without changing route bodies or serialized fields.
5. Protect streaming/hijack paths from frontend deduplication or buffering refactors.
