# VPN.md — route a workspace's network through a VPN

Status: design + **partial implementation landed** (see "Implementation status" below).
Layers on top of the per-workspace isolated network space and its
`~/.dd/net/<ws>.json` hand-off.
Owner surface: `dd-cli`, a small userspace-tunnel helper, `dd-jit-darwin` engine
socket-broker path.

## Implementation status

Landed (the direct-SOCKS5 path, end to end):

* **Model** — `dd-term-core::workspace::VpnConfig { kind: VpnKind, endpoint }` on
  `Workspace` (`vpn: Option<VpnConfig>`). `VpnKind ∈ { Socks5, Http, Wireguard, Openvpn }`.
  Persisted as one `vpn = <kind>:<endpoint>` line in `~/.dd/workspaces.conf` (e.g.
  `vpn = socks5:127.30.0.1:1080`, `vpn = wireguard:vpn/wg.conf`); absent line = direct.
  Back-compatible parse + round-trip test.
* **CLI** — `ddcli workspace create <name> --image … --vpn <spec>`; `<spec>` is a bare
  SOCKS5 `host:port` or a `<kind>:<endpoint>`. Re-create without `--vpn` preserves it;
  `--vpn off` / `--vpn ""` clears it.
* **Builder key** — `ContainerBuilder::egress_socks(addr)` pushes `DD_EGRESS_SOCKS`
  (empty = nothing), unit-tested alongside the other DD_* keys.
* **Launch wiring** — `ddjit_launcher.rs` arms `egress_socks()` from
  `VpnConfig::socks_endpoint()` when the workspace has a SOCKS5 VPN; nothing is set
  otherwise (direct egress, unchanged).
* **Engine redirect** — `netns.c` `egress_connect()` (SOCKS5 CONNECT, blocking with
  `O_NONBLOCK` save/restore, IPv4+IPv6) + `egress_should_redirect()`; called from the
  "Real host connect" site in `net.c` case 203. **Inert when `DD_EGRESS_SOCKS` is unset**
  — `egress_should_redirect()` short-circuits to 0 and the direct `connect()` runs
  byte-for-byte as before.
* **GUI** — a "Network" pane in the New-Workspace form and the workspace Settings pane
  (`dd-gui/src/bin/term.rs`) with a VPN/proxy endpoint field, saved via `save_workspace`.

Not yet implemented (future work): the userspace-WireGuard/OpenVPN **helper** (`wsvpn.rs`
per §7.1) that fronts a tunnel config as a SOCKS5 proxy — so `VpnKind::Wireguard/Openvpn`
and `Http` are modeled + persisted but the launcher only arms the engine redirect for
`Socks5` (it prints a note for the others). Also deferred: in-tunnel DNS (§4.2 DNS-leak
closure), `workspace vpn set|clear|status` subcommands (§5), and the UDP `sendto`/`sendmsg`
egress sites (only TCP `connect` is redirected today).

## 1. Goal

Send **all egress from a workspace's containers** through a VPN tunnel, isolated
per-workspace:

* workspace **A** → VPN-1 (e.g. a corporate WireGuard),
* workspace **B** → direct (no VPN),
* workspace **C** → VPN-2,

all at once, on one mac, with no kernel changes and coexisting with a system-wide VPN
(this host runs Mullvad).

## 2. Why dd makes this easy — the socket-broker insight

There is **no real kernel netns** in dd. A guest's "network" is emulated entirely in the
engine's syscall layer: every `socket`/`connect`/`sendto` the guest makes is brokered by
`dd-jit-darwin/.../syscall/net.c`, and an external connection ultimately becomes **one
`connect()` call the engine makes on a macOS socket** — the "Real host connect" block:

```c
// dd-jit-darwin/src/runtime/os/linux/syscall/net.c  (case 203, connect)
struct sockaddr_storage ss;
socklen_t hl = sa_l2m(sa, (socklen_t)a2, &ss);      // Linux sockaddr -> macOS sockaddr
cr = connect((int)a0, (struct sockaddr *)&ss, hl);  // <-- the single egress choke point
```

DNS is likewise brokered: the guest's queries hit the embedded `127.0.0.11` resolver
(`netns.c` `dns_build_response`) which calls the host `getaddrinfo`.

Because **the engine already owns every outbound connect**, we do not need routes, pf,
or a tun device to steer a workspace's traffic. We redirect that one `connect()` (and the
resolver) through a per-workspace tunnel endpoint. This is impossible to do cleanly with
kernel-netns runtimes (they need `ip netns` + veth + policy routing); dd's userspace
model turns it into a ~1-function change plus a helper process.

## 3. Options considered

**(a) `utun` + userspace WireGuard/OpenVPN, steered by route/pf per source subnet.**
Rejected as the *steering* mechanism: the guest's `172.x` subnet never appears as real IP
packets on any macOS interface — all egress is the *engine process* calling `connect()`
from the **host's** default routing domain (source = host IP). So "route by source
subnet" and pf source rules have nothing to match on. (We *do* still use a userspace
WireGuard tunnel — just as the *endpoint*, per option b, not as a route target.)

**(c) pf / route rules per per-workspace source subnet.** Rejected for the same reason:
no per-workspace source IP exists on the wire. Also needs root and mutates host-global
firewall state (bad with Mullvad's own pf anchors).

**(b) Capture the workspace's egress in the engine and route it through a tunnel.**
✅ **Recommended.** The engine redirects each non-local `connect()` to a per-workspace
**SOCKS5 proxy** that is the front-end of a **userspace WireGuard** tunnel bound to that
workspace. Per-workspace isolation is automatic: each engine process reads its own env
var naming its own proxy; no shared kernel state, no route table, no root.

## 4. Chosen design (option b, concretely)

```
 guest connect(1.2.3.4:443)
        │  (engine net.c case 203)
        ▼
 DD_EGRESS_SOCKS=127.30.0.1:1080 set?  ──no──►  direct connect()  (workspace B)
        │yes
        ▼
 SOCKS5 CONNECT 1.2.3.4:443  ──►  wireproxy (userspace WireGuard, wg-A)  ──►  VPN-1
                                    listens 127.30.0.1:1080
```

### 4.1 The tunnel helper (userspace WireGuard + SOCKS front-end)

Per VPN-enabled workspace, `ddcli` spawns one userspace-WireGuard process that:

* implements WireGuard purely in userspace (no `utun`, no root, no kernel module), and
* exposes a **SOCKS5 proxy on a private loopback endpoint** whose CONNECTs egress
  *inside* the tunnel.

`wireproxy` (github.com/pufferffish/wireproxy, a `wireguard-go` + SOCKS5 wrapper) is the
recommended off-the-shelf implementation — it takes a standard `wg-quick` config plus a
`[Socks5] BindAddress = …` stanza and needs no privileges. (For a fully in-house build,
`boringtun`/`wireguard-go` + a tiny SOCKS5 server is equivalent; OpenVPN via a userspace
`openvpn3` + tun2socks is the analog for OpenVPN configs.)

The helper binds `127.<n>.0.1:1080` — a per-workspace loopback endpoint (same
`loopback_alias` family used by the workspace network layer, so a workspace's alias is its identity for both
DNS and egress). One process per VPN workspace; direct workspaces spawn nothing.

### 4.2 Engine redirect (the only engine change)

Add `egress_connect()` in `netns.c` and call it from the four egress sites in `net.c`
(connect case 203, and the `sendto`/`sendmsg`/`sendmmsg` datagram sites that carry a
dest addr). Sketch:

```c
// netns.c — new
static const char *g_egress_socks;   // DD_EGRESS_SOCKS "host:port", NULL = direct
static int egress_enabled(void) { /* getenv once, cache */ }

// Perform a SOCKS5 CONNECT to (ipbe:port) via the proxy on the guest fd. Returns 0 /
// -errno, mirroring connect(). Blocking + non-blocking (EINPROGRESS) handled like the
// existing bridge dial. IPv4 + IPv6 + (optionally) a name are all expressible in SOCKS5.
static int egress_connect(int fd, const struct sockaddr *dst, socklen_t len);
```

In `net.c` case 203, the "Real host connect" block becomes:

```c
if (egress_enabled() && is_external_inet(sa, a2)) {   // not lo/bridge/DNS/unix
    G_RET(c) = (uint64_t)(int64_t) egress_connect((int)a0, (struct sockaddr*)&ss, hl);
    break;
}
// …existing direct connect() fallthrough…
```

Key points:

* **Loopback, bridge, AF_UNIX, and `127.0.0.11` DNS dials are untouched** — those are
  intra-host emulation and must never go through the VPN. Only genuine external
  AF_INET/AF_INET6 destinations are redirected (the classifiers already exist:
  `lo_is`, `br_connect_is`, `dns_dest_is`, `unix_path_is`).
* **DNS-leak closure**: with a VPN active, the resolver's `getaddrinfo`
  (`dns_build_response`) would otherwise leak names to the *host's* DNS outside the
  tunnel. When `DD_EGRESS_SOCKS` is set, resolve names **through the tunnel** instead:
  send the query to the VPN's DNS server over the SOCKS proxy (SOCKS5 supports TCP
  CONNECT to `<wg_dns>:53`; do DNS-over-TCP there). The WireGuard config's `DNS =` line
  names that server. This keeps both connections *and* lookups inside VPN-1.
* SOCKS5 is chosen over transparent tun redirection because it needs no `utun`, no root,
  and expresses the destination explicitly (no reverse-NAT bookkeeping). `wireproxy`
  speaks it natively.

### 4.3 Per-workspace isolation — how it's enforced

Isolation is a property of the process model, not of any shared table:

* Each container runs in **its own engine process**, forked per workspace launch
  (`dd_jit::Runtime::start`). The engine reads `DD_EGRESS_SOCKS` from **its own** env
  (like `DD_NETBR`/`DD_NETNS` today).
* Workspace A's launcher sets `DD_EGRESS_SOCKS=127.30.0.1:1080` (wg-A) → all of A's
  external connects go through VPN-1.
* Workspace B sets nothing → direct.
* Workspace C sets `DD_EGRESS_SOCKS=127.31.0.1:1080` (wg-C) → VPN-2.

There is no cross-talk: an engine can only reach the proxy named in its own env, and the
proxies are distinct userspace tunnels. No route table, no pf, nothing global to get
wrong.

## 5. Configuration — CLI/UX + storage

```
ddcli workspace vpn <ws> set   <path-to.conf>   # a standard WireGuard wg-quick .conf
ddcli workspace vpn <ws> clear                  # back to direct egress
ddcli workspace vpn <ws> status                 # up? handshakes? public IP via tunnel
```

* `set` copies the WireGuard config to `~/.dd/ws/<ws>/vpn/wg.conf` (0600 — it holds a
  private key) and flips a flag in the workspace model.
* Storage in `dd-term-core/src/workspace.rs`:

  ```rust
  pub struct Workspace {
      // …existing…
      /// Egress mode. Direct = no VPN; Wireguard = tunnel via the stored wg.conf.
      pub egress: Egress,                    // enum { Direct, Wireguard { conf: PathBuf } }
  }
  ```

  Persisted as `egress = direct` | `egress = wireguard:vpn/wg.conf` in
  `~/.dd/workspaces.conf` (path relative to the workspace dir; mirror the existing
  `mount`/`env` handling in `WsBuilder::set` / `WorkspaceStore::save`). The `.conf`
  itself lives outside `workspaces.conf` (secret).

* CLI wiring: `cli.rs` adds `Vpn { ws, action: VpnCmd }` under `WorkspaceCmd`;
  `workspace.rs` handles it. `vpn status` shells the helper's admin API (wireproxy exposes
  handshake/transfer info) or does an in-tunnel `curl https://…/ip`.

## 6. Launch integration

In `dd-cli/src/ddjit_launcher.rs::launch`, after resolving the workspace:

1. If `ws.egress == Wireguard { conf }`: call `crate::wsvpn::ensure(&ws)` →
   idempotently spawn the userspace-WG helper (like `wsdaemon::ensure` spawns the
   daemon), detached, listening on the workspace's `127.<n>.0.1:1080`; wait briefly for
   the SOCKS port. Returns the `host:port`.
2. Set the engine env on the builder:

   ```rust
   builder = builder.env("DD_EGRESS_SOCKS", format!("{sock_host}:{sock_port}"));
   ```

   (Add a `ContainerBuilder::egress_socks(addr)` convenience in
   `dd-jit/src/runtime/container/builder.rs` mirroring `bridge()` /`net_isolate()`, so
   it's part of the public dd-jit surface and unit-tested like the other DD_* keys.)

3. Direct workspaces set nothing → the engine's existing direct `connect()` path runs
   unchanged. Zero overhead when VPN is off.

The same wiring applies to daemon-launched containers: `dd-daemon`'s spawn path sets
`DD_EGRESS_SOCKS` from the workspace's egress config for containers in a VPN workspace.

## 7. Phased implementation plan (ordered)

1. **Helper wrapper** — `dd-cli/src/wsvpn.rs` (new): `ensure(ws) -> SocketAddr`.
   Spawns the userspace-WG+SOCKS helper (bundle `wireproxy`, or build the in-house
   `boringtun`+SOCKS variant), detached, per-workspace loopback endpoint, idempotent
   (reuse a live listener). Model on `wsdaemon.rs`.
2. **Storage** — `dd-term-core/src/workspace.rs`: add `egress: Egress` + persistence +
   round-trip test (pure Rust, CI-testable anywhere).
3. **CLI** — `dd-cli/src/cli.rs` + `workspace.rs`: `workspace vpn set|clear|status`;
   copy `.conf` to `~/.dd/ws/<ws>/vpn/wg.conf` (0600).
4. **Builder key** — `dd-jit/src/runtime/container/builder.rs`: `egress_socks(addr)`
   → pushes `DD_EGRESS_SOCKS`; extend the `builder_dialect_matches_daemon_keys` test.
5. **Launch wiring** — `dd-cli/src/ddjit_launcher.rs`: call `wsvpn::ensure` + set the
   env when `egress == Wireguard`.
6. **Engine redirect** — `dd-jit-darwin/.../container/netns.c` + `.../syscall/net.c`:
   `egress_connect()` (SOCKS5 CONNECT, blocking + `EINPROGRESS`) at the four egress
   sites, plus in-tunnel DNS in `dns_build_response`. Gate behind `DD_EGRESS_SOCKS`;
   absent → today's behavior byte-for-byte. **Hand this to the engine agent** (they own
   engine/build). Provide it a self-contained diff + a differential test (a VPN
   workspace's `curl ifconfig.me` returns the VPN exit IP; a direct workspace returns the
   host IP; both byte-identical to `docker -c orbstack run` for non-IP-dependent output).
7. **Status/observability** — `wsvpn::status`: handshake age + an in-tunnel IP probe.

Ship order: 1–5 land the plumbing and config (direct workspaces already fully work; a
VPN workspace spawns the helper but egress still needs step 6). 6 activates redirection.
7 is polish.

## 8. Interface required from the network-space layer

Reuses the **`~/.dd/net/<ws>.json`** workspace-network hand-off. VPN adds one field, written by whoever owns the
workspace's egress config (the CLI, projected in):

```jsonc
{
  "ws": "api",
  "net_id": "ws-api",
  "loopback_alias": "127.30.0.1",
  "egress": { "mode": "wireguard", "socks": "127.30.0.1:1080" }
  //          "mode": "direct"  -> engine gets no DD_EGRESS_SOCKS
}
```

What we need from the network layer specifically:

1. **A stable per-workspace loopback endpoint** (`127.<n>.0.1`) reserved for this
   workspace — shared with DOMAINS, so DNS alias and SOCKS bind live on the same private
   IP. The network layer already allocates the subnet deterministically; deriving the
   `127.<n>` octet from it makes both features collision-free.
2. Confirmation that the layer treats **egress as a per-workspace attribute** and
   surfaces it via the `~/.dd/net/<ws>.json` `egress` field, so the launcher/daemon can
   translate it to `DD_EGRESS_SOCKS` uniformly (CLI launch *and* daemon-run containers).
3. Nothing else — no routes, no pf, no tun. The whole point of option (b) is that the
   network layer stays a userspace emulation and egress is a per-process env var.

## 9. Coexistence with Mullvad (and other system VPNs)

This host runs Mullvad (`mullvad-daemon`, several `utun*`). Behavior:

* **Encapsulation order**: our per-workspace userspace WireGuard opens an outer UDP flow
  to *its* VPN endpoint. That outer UDP is sent via the **host default route** — which,
  when Mullvad is connected, is Mullvad's tunnel. So a VPN workspace is effectively
  **workspace-VPN-over-Mullvad** (double-encapsulated). This is correct and expected:
  the container's traffic exits at VPN-1, whose transport rides inside Mullvad. Document
  it; it is not a bug.
* **Mullvad kill-switch / lockdown mode**: Mullvad's firewall may block non-Mullvad
  traffic when disconnected, and by default blocks LAN. Our outer UDP to the workspace
  VPN endpoint must be permitted. Since it egresses *through* Mullvad's own tunnel when
  connected, it is allowed. When Mullvad is **disconnected with lockdown on**, the outer
  UDP is dropped → the workspace VPN can't handshake → egress fails **closed** (no leak).
  Surface this in `vpn status` ("no handshake — is the host's system VPN blocking?").
* **No pf/route contention**: because we use *userspace* WireGuard + SOCKS and never
  touch `utun`/routes/pf, there is zero interaction with Mullvad's pf anchors or route
  table. We cannot accidentally break Mullvad, and it cannot capture our SOCKS loopback
  traffic (127/8 is loopback-local).
* **Direct workspaces** follow the host default route → through Mullvad if connected,
  direct otherwise — matching normal host behavior, per-workspace and independent of the
  VPN workspaces.
```
