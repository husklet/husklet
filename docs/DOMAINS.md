# DOMAINS.md — a registerable domain per workspace that resolves into its network

Status: design. Layers on top of the per-workspace isolated network space (see
[the network-layer interface](#interface-required-from-the-network-space-layer)).
Owner surface: `dd-cli`, `dd-daemon`, `dd-jit-darwin` engine DNS path.

## 1. Goal

Give every workspace a stable DNS name that resolves to *its* network — usable both
from the **mac host** and **from inside containers**:

* `<ws>.dd`                      → the workspace's service address (its published ports).
* `<container>.<ws>.dd`          → a specific container in that workspace.
* A user-registerable alias      → `ddcli workspace domain <ws> myapp.test` also works.

The default TLD is **`.dd`** (configurable). Example: workspace `api` publishing a web
server → `curl http://api.dd/` from the mac, and `ping db.api.dd` from a sibling
container.

## 2. Chosen approach + why

Two independent resolvers, one per side of the boundary — because macOS host resolution
and in-container resolution use completely different plumbing:

| Side | Mechanism | Address returned |
|------|-----------|------------------|
| **mac host** | `/etc/resolver/dd` → a dd-run UDP DNS responder on `127.0.0.1:<port>` | the workspace's **loopback alias** `127.a.b.c` (a real `lo0` alias) |
| **in-container** | the engine's existing `127.0.0.11:53` intercept (`dns_build_response` in `netns.c`) | the peer container's `172.x.y.z` subnet IP |

Why a **loopback alias per workspace** (`127.a.b.c` on `lo0`) rather than resolving
everything to `127.0.0.1`:

* It removes host-port collisions. Two workspaces both publishing container port 80 can
  bind `127.20.0.1:80` and `127.21.0.1:80` — no host-port remap, `<ws>.dd:80` "just
  works" with native ports.
* It is already a proven pattern on macOS: this very host has OrbStack's
  `127.36.132.94` aliased on `lo0` with a matching `/etc/resolver` entry
  (`scutil --dns` resolver #1). We are copying a known-good design.
* `127.0.0.0/8` is entirely routed to loopback on macOS with no configuration, so any
  `127.a.b.c` is reachable immediately after `ifconfig lo0 alias 127.a.b.c`.

The per-workspace loopback alias is the single new thing we need from the network layer
(§7). Everything else reuses machinery that already exists.

## 3. Host-side resolution (the `.dd` responder)

### 3.1 The resolver file

macOS `mDNSResponder` reads `/etc/resolver/<tld>`: any name ending in `.<tld>` is sent
to the nameserver/port named in that file. We install:

```
# /etc/resolver/dd   (0644, root-owned; created by `ddcli install` / first workspace up)
nameserver 127.0.0.1
port 53535
```

Writing under `/etc/resolver` needs root **once** at install time. `ddcli install`
already escalates to lay down the LaunchAgent; it writes this file in the same step
(and `ddcli uninstall` removes it). No per-workspace privilege is ever required — the
file is static; only the responder's in-memory zone table changes as workspaces come
and go.

Notes on the port: macOS honors a non-53 `port` line in resolver files, so we avoid
binding 53 (which needs root and collides with mDNSResponder). `53535` is the default;
`ddcli` records the chosen port in `~/.dd/dns/config.json` so the responder and the
resolver file agree.

### 3.2 The responder process (`dd-daemon --resolver`)

A single host-wide responder — **not** per-workspace, because it must answer for *all*
workspaces from one socket. It is a new mode of the existing `dd-daemon` binary,
launched by `ddcli` as its own LaunchAgent `com.dd.resolver` (mirrors `com.dd.daemon`
in `dd-cli/src/paths.rs` / `install.rs`).

New crate module `dd-daemon/src/dns/` (tokio UDP + TCP on `127.0.0.1:<port>`):

```
mod dns {
    // Loads and watches ~/.dd/dns/zones.json (see §3.3). Answers A/AAAA/PTR; everything
    // else → NOERROR/NODATA (same policy the engine resolver already uses).
    async fn serve(bind: SocketAddr, zones: Arc<ArcSwap<ZoneTable>>) -> io::Result<()>;

    struct ZoneTable { tld: String, entries: HashMap<String, Ipv4Addr> }
    // keys are fully-qualified lowercased names WITHOUT trailing dot:
    //   "api.dd"        -> 127.20.0.1        (workspace service alias)
    //   "db.api.dd"     -> 127.20.0.1        (container -> same alias; native ports)
    //   "myapp.test"    -> 127.20.0.1        (a registered custom alias, see §5)
}
```

The DNS wire codec is trivial and already exists in C in `netns.c`
(`dns_build_response`, `dns_dec_qname`, `dns_put_rr`); the Rust responder re-implements
the same tiny subset (question echo, A/AAAA/PTR RRs, NXDOMAIN vs NODATA). ~150 lines.

Resolution policy:

* `<ws>.<tld>` and `<container>.<ws>.<tld>` → the workspace's `loopback_alias` (an
  A record). AAAA → NODATA (aliases are v4-only, matching the engine resolver's policy).
* Unknown name under the TLD → NXDOMAIN.
* PTR for a workspace alias → `<ws>.<tld>` (nice-to-have; supports reverse lookups).

### 3.3 The zone table (`~/.dd/dns/zones.json`)

The responder never talks to the per-workspace daemons directly (they come and go). It
watches one file, rewritten atomically whenever a workspace's network comes up/down:

```jsonc
{
  "tld": "dd",
  "workspaces": {
    "api": {
      "alias": "127.20.0.1",
      "aliases_extra": ["myapp.test"],     // user-registered (§5)
      "containers": ["web", "db"]           // -> web.api.dd, db.api.dd, all -> alias
    }
  }
}
```

Producer: the network-space layer already knows a workspace's network on start/stop.
It writes `~/.dd/net/<ws>.json` (§7); a thin `dd-cli`/daemon hook projects the relevant
fields into `zones.json` and `SIGHUP`s (or relies on the file-watch in) the responder.
Keeping `zones.json` as the responder's only input keeps the responder stateless and
crash-safe.

### 3.4 Ports — why the alias makes them free

The engine's published-port forwarder (`fwd_listen_thread` in `netns.c`) today binds
`0.0.0.0:<host_port>` and relays to the guest's AF_UNIX switch socket. With a workspace
loopback alias we ask the network layer (§7) to bind the forwarder on
`<loopback_alias>:<container_port>` instead — a 1:1 identity map on a private IP. Then:

* `curl http://api.dd/`     → resolves to `127.20.0.1`, hits the forwarder on `:80`.
* `redis-cli -h db.api.dd`  → `127.20.0.1:6379`, native port, no remap, no collision
  with another workspace's `:6379` on its own alias.

Fallback if the network layer cannot bind-per-alias in v1: the responder still returns
`127.0.0.1` and users reach services on the existing published **host** port
(`api.dd` names the box, ports stay remapped). Naming works; port-collision-freedom is
the payoff of the alias and can land in a second pass.

### 3.5 Cleanup

* Workspace stop: network layer removes `~/.dd/net/<ws>.json`; the projector drops the
  workspace from `zones.json`; the forwarder + `lo0` alias are torn down by the network
  layer. The responder simply stops answering that name (→ NXDOMAIN).
* `ddcli uninstall`: unloads `com.dd.resolver`, removes `/etc/resolver/dd` and
  `~/.dd/dns/`.

## 4. In-container resolution

Inside a container, `/etc/resolv.conf` already points at `127.0.0.11`, intercepted by
the engine (`net.c` send paths → `dns_build_response` in `netns.c`). Two cases:

1. **Same-workspace peer** (`<container>` or `<container>.<ws>.dd`): already works for
   the bare name via `dns_local_lookup`, which reads the per-network `.names` table
   (`/tmp/.ddbr-<netid>/.names`, written by `dd-daemon/src/runtime/spawn/net.rs`). We
   add one step in `dns_build_response`: **strip a configured `.<ws>.<tld>` (and bare
   `.<tld>`) suffix from `qname` before the `dns_local_lookup` call**, so
   `db.api.dd` resolves to the same `172.x` peer IP as bare `db`. The TLD and the
   owning workspace name are handed to the engine as env (`DD_DNS_TLD=dd`,
   `DD_DNS_WS=api`) by the launcher/daemon — cheap, no new files.

2. **Cross-workspace** (`web.other-ws.dd` from workspace `api`): **denied by default** —
   cross-workspace reachability contradicts network isolation. If the suffix names a
   *different* workspace, the stripped name won't be in this container's `.names` table,
   so it falls through to the host resolver and returns NXDOMAIN (the `.dd` TLD isn't in
   the mac's real DNS). This is the correct default. Opt-in cross-workspace peering is a
   separate feature (a shared `.names` view or an explicit `--link`), out of scope here.

Net effect: inside a container, `<name>.<ws>.dd` is an alias for the bare container name
within the *same* workspace, resolving to the real subnet IP (instant, offline, never
leaks to external DNS — exactly as bare names do today).

## 5. "Register a domain" — CLI/UX + storage

```
ddcli workspace domain <ws> add   <name>     # e.g. myapp.test  (extra alias -> ws)
ddcli workspace domain <ws> rm    <name>
ddcli workspace domain <ws> ls
```

* The canonical `<ws>.dd` name is **implicit** (always present, not stored).
* Extra registered names are stored in the workspace model so they persist and re-apply
  on every launch. Add to `dd-term-core/src/workspace.rs`:

  ```rust
  pub struct Workspace {
      // …existing…
      /// Extra host DNS aliases for this workspace, e.g. "myapp.test". The implicit
      /// "<name>.dd" is always resolvable and not stored here.
      pub domains: Vec<String>,
  }
  ```

  Persisted as repeatable `domain = myapp.test` lines in `~/.dd/workspaces.conf`
  (the block format already supports repeatable keys — mirror `env`/`mount` in
  `WsBuilder::set` and `WorkspaceStore::save`).

* A registered name that is NOT under `.dd` (e.g. `myapp.test`) needs its own
  `/etc/resolver/test` file pointing at the same responder. `ddcli workspace domain add`
  ensures `/etc/resolver/<tld-of-name>` exists (root, once per new TLD) and adds the name
  to `zones.json`. The responder already answers any name present in the table regardless
  of TLD, so no responder change is needed.

* Wire-up: `cli.rs` gets a `Domain { ws, action: DomainCmd }` under `WorkspaceCmd`;
  `workspace.rs` handles it (update the model, refresh `zones.json`, ensure the resolver
  file). The GUI can later expose the same via `ddcli`.

## 6. Phased implementation plan (ordered)

1. **`dd-daemon/src/dns/mod.rs`** (new): the UDP/TCP responder + `ZoneTable` +
   `zones.json` load/watch. Unit-test the wire codec against `netns.c`'s format
   (hand a known query, assert bytes). Runs anywhere (pure Rust) → testable on Linux CI.
2. **`dd-daemon/src/main.rs`**: add `--resolver` mode that binds the responder.
3. **`dd-cli/src/paths.rs`**: add `RESOLVER_LABEL="com.dd.resolver"`,
   `resolver_plist()`, `dns_dir() = ~/.dd/dns`, `zones_json()`, `dd_resolver_conf()`.
4. **`dd-cli/src/install.rs`**: on install, write `/etc/resolver/dd` (root),
   `~/.dd/dns/config.json`, and load `com.dd.resolver`; on uninstall, reverse it.
5. **zones projector** — `dd-cli/src/wsdns.rs` (new): read `~/.dd/net/<ws>.json`
   (§7), project into `zones.json` atomically, signal the responder. Called from the
   workspace up/down path.
6. **`dd-term-core/src/workspace.rs`**: add `domains: Vec<String>` + persistence
   (`domain = …` lines) + round-trip test.
7. **`dd-cli/src/cli.rs` + `workspace.rs`**: `workspace domain add|rm|ls`; ensure
   `/etc/resolver/<tld>` for non-`.dd` names; refresh `zones.json`.
8. **Engine in-container aliasing** — `dd-jit-darwin/.../container/netns.c`
   `dns_build_response`: strip `.<DD_DNS_WS>.<DD_DNS_TLD>` / `.<DD_DNS_TLD>` from
   `qname` before `dns_local_lookup`. Set `DD_DNS_TLD`/`DD_DNS_WS` in the launcher
   (`dd-cli/src/ddjit_launcher.rs`) and daemon spawn env. *(Do NOT edit engine files
   yourself — hand this step to the engine agent; it's ~10 lines + two env reads.)*
9. **Alias-bound forwarder** (depends on §7 loopback alias): network layer binds the
   published-port forwarder on `<alias>:<container_port>`. Until then, responder returns
   `127.0.0.1` (§3.4 fallback).

Ship order: 1–4 give a working responder + `<ws>.dd` → `127.0.0.1`. 5 makes it
workspace-aware. 6–7 add custom names. 8 adds in-container `.dd` names. 9 unlocks
collision-free native ports.

## 7. Interface required from the network-space layer

Per workspace, on network **up**, write `~/.dd/net/<ws>.json` (and delete on **down**):

```jsonc
{
  "ws": "api",
  "net_id": "ws-api",              // == DD_NETBR key / switch dir suffix
  "netns_key": "ws-api",           // == DD_NETNS key
  "subnet": "172.20.0.0/16",
  "gateway": "172.20.0.1",
  "loopback_alias": "127.20.0.1",  // REQUESTED: a stable lo0 alias for this ws
  "containers": [ { "name": "web", "ip": "172.20.0.2" },
                  { "name": "db",  "ip": "172.20.0.3" } ],
  "published":  [ { "container": "web", "container_port": 80,
                    "host_ip": "127.20.0.1", "host_port": 80, "proto": "tcp" } ]
}
```

Concretely, what already exists vs. what we need:

* **Exists**: subnet/gateway/endpoints (`dd-daemon` IPAM `Net`/`Endpoint`), the live
  `.names` table (`spawn/net.rs`), the portmap + host forwarder (`netns.c`).
* **Requested (new)**:
  1. A **stable `loopback_alias`** per workspace (derive `127.<b>.<c>.1` from the
     subnet's second/third octet so it's deterministic), added to `lo0` on up
     (`ifconfig lo0 alias <ip>`), removed on down.
  2. The **`~/.dd/net/<ws>.json`** registry file as the hand-off (superset of what
     VPN.md also consumes — one file, two features).
  3. Optionally bind the published-port **forwarder on `<alias>:<container_port>`**
     (identity map) rather than `0.0.0.0:<host_port>` (enables §3.4 collision-free
     ports). Non-blocking: naming works without it.

## 8. Coexistence & edge cases

* **Mullvad / other VPNs**: `.dd` names resolve to `127.0.0.0/8`, which is loopback and
  never leaves the host, so no VPN or split-DNS interaction. `/etc/resolver/dd` is
  scoped strictly to the `.dd` TLD — it cannot shadow real DNS or Mullvad's resolvers.
* **OrbStack coexistence**: OrbStack already owns `/etc/resolver/*` for its own TLDs and
  a `127.x` lo0 alias; we pick a disjoint TLD (`dd`) and a disjoint `127.x` range
  (derive from `172.18/12`→`127.18/8` upward, and skip any alias already present on
  `lo0`). No conflict.
* **`.local` / mDNS**: never used — `.dd`/`.test` are chosen precisely to avoid the
  multicast-DNS resolver (`.local`).
* **TTL / caching**: responder returns TTL 30s (matches the engine). On workspace
  restart the alias is stable (derived from subnet), so cached answers stay valid.
```
