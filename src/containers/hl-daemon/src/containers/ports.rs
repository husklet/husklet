//! Process-independent published-port forwarder (`docker run -p`).
//!
//! THE BUG (before this): the host `AF_INET` listener for a published port lived INSIDE the guest engine
//! process that happened to call `listen()` (hl-jit `netns.c fwd_maybe_start`). Because every guest
//! fork/clone is a real host process, a prefork/worker server (nginx master→workers, postgres postmaster,
//! or a shell loop `while true; do nc -l -p 9000 -w1; done`) tears down + re-creates that listener on every
//! re-listen — so the host port blinks in and out and `g_fwd_started` races `EADDRINUSE`, even though the
//! CONTAINER is alive the whole time.
//!
//! THE FIX (mirrors docker-proxy / `portmapper/proxy_linux.go:StartProxy`): the host listener is owned by
//! the **daemon**, whose lifetime == the container's, NOT any guest process. For each published
//! `(hostIP, hostPort) → containerPort` we bind a real `AF_INET` listener in the daemon and, per accepted
//! connection, dial the container's `AF_UNIX` virtual-switch inode (the same rendezvous a peer container
//! uses) and pump bytes both ways. The guest can re-`listen()` as often as it likes: the daemon's dial is
//! gap-tolerant (retries across the re-bind window, exactly like the engine's `switch_dial`), so a
//! re-listening server stays continuously reachable. The engine's in-process TCP host listener is disabled
//! (`HL_PUBLISH_DAEMON=1`) so the two never fight over the port; the engine keeps only the guest-side
//! bind/listen→switch redirect + `getsockname`→cport reporting.
//!
//! The switch path is byte-identical to what the engine binds (`netns.c`):
//!   * bridge (0.0.0.0/eth0 bind):  `/tmp/.hlbr-<netid[..40]>/<cIP>:<cport>`
//!   * loopback (127.0.0.1 bind):   `/tmp/.hlnet-<netnsKey[..40]>/p<cport>`
//! We dial both candidates (a published server almost always binds 0.0.0.0 → the bridge path, but a
//! 127.0.0.1 publish lands on loopback), taking whichever connects.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::task::JoinHandle;

use crate::containers::Publish;
use crate::model::Container;

/// One published binding's resolved plan: the host address to listen on and the container-switch inode
/// candidates to forward into. Everything here is fixed for the container's life, so a re-listening guest
/// never needs the daemon to recompute it.
#[derive(Clone)]
struct Binding {
    host_ip: String,
    host_port: u16,
    switch_paths: Vec<String>, // dialed in order; first to connect wins
}

/// Active acceptor tasks per container id. Aborting a handle drops its `TcpListener`, which releases the
/// host port immediately (docker semantics: stop/kill/rm frees the binding). In-flight relay tasks are
/// detached and end on their own EOF.
fn registry() -> &'static Mutex<HashMap<String, Vec<JoinHandle<()>>>> {
    static R: OnceLock<Mutex<HashMap<String, Vec<JoinHandle<()>>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Truncate a key the way the engine's `snprintf("%.40s")` does, so daemon-computed switch paths
/// byte-match the engine's binds.
struct Switch;

impl Switch {
    fn key(s: &str) -> &str {
        &s[..s.len().min(40)]
    }
}

pub(crate) struct Forwarders;

/// Compute the per-binding forwarding plan from live daemon state. `bridge` is this container's
/// (netid, ip) endpoint on its first network (as `spawn_cfg` resolves it); `netns_key` is the container's
/// loopback-namespace key (its own id, or for an exec the target container's id).
fn plan(c: &Container, bridge: &Option<(String, String)>, netns_key: &str) -> Vec<Binding> {
    Publish::new(&c.publish)
        .bindings()
        .into_iter()
        .filter(|p| p.proto == "tcp")
        .map(|p| {
            let mut switch_paths = Vec::new();
            if let Some((netid, ip)) = bridge {
                switch_paths.push(format!(
                    "/tmp/.hlbr-{}/{}:{}",
                    Switch::key(netid),
                    ip,
                    p.container_port
                ));
            }
            switch_paths.push(format!(
                "/tmp/.hlnet-{}/p{}",
                Switch::key(netns_key),
                p.container_port
            ));
            Binding {
                host_ip: p.host_ip,
                host_port: p.host_port,
                switch_paths,
            }
        })
        .collect()
}

/// Start (idempotently) the host→container forwarders for a container. Called from `spawn_live` after the
/// guest is spawned; a no-op when the container publishes nothing or forwarders are already running (the
/// restart path re-enters `spawn_live` but the daemon-owned listeners persist).
impl Forwarders {
    async fn start(
        c: &Container,
        bridge: &Option<(String, String)>,
        netns_key: &str,
    ) -> Result<(), String> {
        let bindings = plan(c, bridge, netns_key);
        if bindings.is_empty() {
            return Ok(());
        }
        let cid = c.id.clone();
        {
            let reg = registry().lock().unwrap();
            if reg.get(&cid).map_or(false, |v| !v.is_empty()) {
                return Ok(());
            } // already forwarding (restart)
        }
        // Bind EVERY host listener SYNCHRONOUSLY up front: an occupied host port must FAIL the start with a
        // port-allocation error (docker's "port is already allocated"), not silently no-op in a background
        // acceptor task and leave the container reporting running while no hl listener owns the published port.
        let mut bound = Vec::new();
        for b in bindings {
            let addr = format!("{}:{}", b.host_ip, b.host_port);
            match TcpListener::bind(&addr).await {
                Ok(l) => bound.push((l, b)),
                Err(e) => {
                    // Release the listeners already bound for this start (dropping a TcpListener closes it),
                    // then fail — the caller aborts the whole start.
                    return Err(format!(
                        "driver failed programming external connectivity: Bind for {addr} failed: \
                     port is already allocated ({e})"
                    ));
                }
            }
        }
        let mut handles = Vec::new();
        for (listener, b) in bound {
            let h = tokio::spawn(accept_loop(listener, b, cid.clone()));
            handles.push(h);
        }
        registry().lock().unwrap().insert(cid, handles);
        Ok(())
    }

    /// Stop + release a container's forwarders (host ports freed). Called from stop/kill/remove and the `--rm`
    /// autoremove reaper. Safe to call when none are active.
    pub(crate) fn stop(cid: &str) {
        let handles = registry().lock().unwrap().remove(cid).unwrap_or_default();
        for h in handles {
            h.abort();
        }
    }
}

/// Accept forever on an ALREADY-BOUND listener, spawning a relay per connection. The bind itself now
/// happens synchronously in [`start`] so an EADDRINUSE fails the container start rather than being
/// swallowed here in a background task (docker's "port is already allocated").
async fn accept_loop(listener: TcpListener, b: Binding, cid: String) {
    let _ = &cid; // retained for future per-container diagnostics; the loop below owns the listener
    loop {
        let (host_conn, _) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let paths = b.switch_paths.clone();
        tokio::spawn(async move {
            if let Some(guest_conn) = Switch::dial(&paths).await {
                Relay::run(host_conn, guest_conn).await;
            }
        });
    }
}

/// Dial the guest's AF_UNIX switch inode, gap-tolerant across a re-listening server (ENOENT: inode gone
/// mid-rebind; ECONNREFUSED: stale inode, nothing accepting yet). Mirrors `netns.c switch_dial`: retry a
/// fresh socket for ~1.2s (≈TCP SYN retransmit), trying each candidate path, and reject a "dead on
/// arrival" connection (a `-w N` listener whose accept window just closed HUPs with no data).
impl Switch {
    async fn dial(paths: &[String]) -> Option<UnixStream> {
        for _ in 0..60 {
            for p in paths {
                if let Ok(s) = UnixStream::connect(p).await {
                    if !Self::dead_on_arrival(&s).await {
                        return Some(s);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        None
    }

    /// A connection that immediately HUPs with zero readable bytes is a peer mid-exit (see `netns.c
    /// switch_dead_on_arrival`): a listener whose `-w N` window just closed accepted nothing. Distinguish it
    /// from a live-but-idle peer (client-first protocol: server awaits our request) with a brief readable wait
    /// + a MSG_PEEK that consumes nothing. Returns true → caller retries a fresh dial.
    async fn dead_on_arrival(s: &UnixStream) -> bool {
        // ~40ms: returns at once when readable/closed; the small wait only bites a truly-idle live peer.
        match tokio::time::timeout(Duration::from_millis(40), s.readable()).await {
            Ok(Ok(())) => {
                let fd = s.as_raw_fd();
                let mut byte = [0u8; 1];
                // MSG_PEEK|MSG_DONTWAIT: consumes nothing (the guest's later read still sees any data).
                let n = unsafe {
                    libc::recv(
                        fd,
                        byte.as_mut_ptr().cast(),
                        1,
                        libc::MSG_PEEK | libc::MSG_DONTWAIT,
                    )
                };
                if n == 0 {
                    return true;
                } // clean EOF, no data -> dead
                if n < 0 {
                    let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                    if e == libc::EAGAIN || e == libc::EWOULDBLOCK {
                        return false;
                    } // spurious wake, live
                    return true; // ECONNRESET/REFUSED -> dead
                }
                false // real data pending -> live
            }
            _ => false, // not readable within the window (idle but live) or timer error -> keep it
        }
    }
}

/// Bidirectional byte pump between the host connection and the guest switch connection, with half-close so
/// a one-shot request/response (and a server that closes after replying) finishes cleanly.
struct Relay;

impl Relay {
    async fn run(mut host: TcpStream, mut guest: UnixStream) {
        let _ = tokio::io::copy_bidirectional(&mut host, &mut guest).await;
        let _ = host.shutdown().await;
        let _ = guest.shutdown().await;
    }
}

/// Resolve `(bridge, netns_key)` for a container the way `spawn_cfg`/`spawn_live` do, then start its
/// forwarders. Convenience used by `spawn_live`. `bridge` must already be resolved by the caller (it holds
/// the state lock); we only need the netns key, derivable from the container itself.
impl Forwarders {
    pub(crate) async fn start_for(
        c: &Container,
        bridge: &Option<(String, String)>,
    ) -> Result<(), String> {
        // Matches `spawn_cfg`: an exec shares the target container's netns via `netns_key`; a normal container
        // uses its own id. The engine truncates to 40, so pass the untruncated key (t40 clips inside `plan`).
        let ns_key = c.netns_key.as_deref().unwrap_or(&c.id).to_string();
        Self::start(c, bridge, &ns_key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctr(publish: &str) -> Container {
        Container {
            publish: publish.to_string(),
            ..Default::default()
        }
    }

    // ---- t40: byte-truncate to 40, mirroring the engine's snprintf("%.40s") ----
    #[test]
    fn t40_short_unchanged() {
        assert_eq!(Switch::key("abc"), "abc");
        assert_eq!(Switch::key(""), "");
    }
    #[test]
    fn t40_exactly_40_unchanged() {
        let s = "a".repeat(40);
        assert_eq!(Switch::key(&s), s);
    }
    #[test]
    fn t40_over_40_truncated() {
        let s = "b".repeat(50);
        let got = Switch::key(&s);
        assert_eq!(got.len(), 40);
        assert_eq!(got, "b".repeat(40));
    }

    // ---- plan: derive the per-binding host addr + switch-inode candidates ----
    #[test]
    fn plan_with_bridge_has_bridge_then_loopback_paths() {
        let c = ctr("0.0.0.0:8080:80/tcp");
        let bridge = Some(("net123".to_string(), "172.18.0.5".to_string()));
        let plan = plan(&c, &bridge, "nskey");
        assert_eq!(plan.len(), 1);
        let b = &plan[0];
        assert_eq!(b.host_ip, "0.0.0.0");
        assert_eq!(b.host_port, 8080);
        // bridge candidate first (a published server usually binds 0.0.0.0 -> the bridge path), then loopback.
        assert_eq!(
            b.switch_paths,
            vec![
                "/tmp/.hlbr-net123/172.18.0.5:80".to_string(),
                "/tmp/.hlnet-nskey/p80".to_string(),
            ]
        );
    }

    #[test]
    fn plan_without_bridge_is_loopback_only() {
        let c = ctr("127.0.0.1:9000:90/tcp");
        let plan = plan(&c, &None, "nskey");
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].host_ip, "127.0.0.1");
        assert_eq!(plan[0].host_port, 9000);
        assert_eq!(
            plan[0].switch_paths,
            vec!["/tmp/.hlnet-nskey/p90".to_string()]
        );
    }

    #[test]
    fn plan_drops_non_tcp() {
        // Only tcp bindings get a host forwarder; a udp publish yields no plan entry.
        let c = ctr("0.0.0.0:53:53/udp");
        assert!(plan(&c, &None, "nskey").is_empty());
    }

    #[test]
    fn plan_empty_publish_is_empty() {
        assert!(plan(&ctr(""), &None, "nskey").is_empty());
    }

    #[test]
    fn plan_truncates_long_netid_and_netns_key_to_40() {
        // The switch paths must byte-match the engine's %.40s binds, so a >40-char netid / netns key is
        // clipped to its first 40 chars inside the formatted path.
        let long_id = "z".repeat(50);
        let c = ctr("0.0.0.0:8080:80/tcp");
        let bridge = Some((long_id.clone(), "10.0.0.2".to_string()));
        let plan = plan(&c, &bridge, &long_id);
        let clipped = "z".repeat(40);
        assert_eq!(
            plan[0].switch_paths,
            vec![
                format!("/tmp/.hlbr-{clipped}/10.0.0.2:80"),
                format!("/tmp/.hlnet-{clipped}/p80"),
            ]
        );
    }

    #[test]
    fn plan_multiple_bindings_preserve_ports() {
        // Two tcp publishes -> two bindings, each with its own host/container ports.
        let c = ctr("0.0.0.0:8080:80/tcp,0.0.0.0:8443:443/tcp");
        let plan = plan(&c, &None, "k");
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].host_port, 8080);
        assert_eq!(plan[0].switch_paths, vec!["/tmp/.hlnet-k/p80".to_string()]);
        assert_eq!(plan[1].host_port, 8443);
        assert_eq!(plan[1].switch_paths, vec!["/tmp/.hlnet-k/p443".to_string()]);
    }
}
