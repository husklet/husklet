//! net — basics expansion (in-process JIT matrix). Owner: net agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! Breadth over the sockets surface a real networked service leans on, loopback-only and deterministic:
//! TCP (multi-client echo, half-close/shutdown, bulk streamed transfer), UDP (connected send/recv),
//! AF_UNIX (named stream + dgram), socket options (SNDBUF/RCVBUF/LINGER/KEEPALIVE/TYPE), non-blocking
//! accept + poll, recv flags (MSG_PEEK/MSG_DONTWAIT/MSG_WAITALL), sendmsg/recvmsg with iovec + msg_name,
//! writev/readv, getsockname/getpeername, select over multiple fds, getaddrinfo(numeric), inet_pton/ntop,
//! gethostname, fcntl socket flags, and connect-refused error semantics.
//!
//! `port(...)` cases prove the networking is byte-identical emulated-on-Linux and native-on-macOS — the
//! acid test that a postgres/redis-shaped service behaves the same. Linux-only extensions (accept4,
//! SO_PEERCRED) are `src(...)` diffed against the native oracle.
#![allow(unused_imports)]
use crate::{fixture, group, in_rootfs, port, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![ext_net()]
}

fn ext_net() -> Group {
    group(
        "ext_net",
        vec![
            // ---- TCP ----
            port("tcp-multi", "ext_net/net_tcp_multi.c").out("tcp_multi total=1266\n"),
            port("tcp-shutdown", "ext_net/net_tcp_shutdown.c").out("tcp_shutdown got=25 eof=1\n"),
            port("tcp-bulk", "ext_net/net_tcp_bulk.c").out("tcp_bulk sum=13056000\n"),
            // ---- UDP ----
            port("udp-connected", "ext_net/net_udp_connected.c").out("udp_connected total=756\n"),
            // ---- AF_UNIX ----
            port("unix-stream", "ext_net/net_unix_stream.c").out("unix_stream reply=UNIX-STREAM\n"),
            port("unix-dgram", "ext_net/net_unix_dgram.c").out("unix_dgram reply=dgram-unix\n"),
            // a datagram sendto/sendmsg to a NAMED AF_UNIX dest must route through the same mapping
            // bind/connect use (abstract-ns here; overlay pathname like /dev/log in the container scenarios).
            // Without it the datagram is dropped (macOS has no abstract ns). Linux-only; diffed vs native.
            src("unix-dgram-abstract", "ext_net/net_unix_dgram_abstract.c").oracle(),
            // a server that binds 0.0.0.0 must answer a 127.0.0.1 client in the SAME container even with a
            // user network (bridge) attached. The 0.0.0.0 bind lands on the per-network AF_UNIX switch (our IP);
            // the 127.0.0.1 dial must fall back there on a FRESH socket. Enable the switch via DD_NETNS/DD_NETBR/
            // DD_IP (as the daemon does for a user network); golden PONG. Linux-only (the switch is Linux).
            src("lo-any-bridge", "ext_net/net_lo_any.c")
                .env("DD_NETNS", "ddc228")
                .env("DD_NETBR", "ddc228br")
                .env("DD_IP", "172.28.0.5")
                .out("lo_any reply=PONG\n"),
            // ---- socket options ----
            port("sockopt-buf", "ext_net/net_sockopt_buf.c")
                .out("sockopt_buf set_ok=1 snd_ge=1 rcv_ge=1\n"),
            port("so-linger", "ext_net/net_so_linger.c")
                .out("so_linger on=1 t=5 keepalive=1 type_stream=1\n"),
            port("sock-flags", "ext_net/net_socket_cloexec.c")
                .out("sock_cloexec before=0 after=1 nonblock=1\n"),
            // ---- non-blocking accept + poll ----
            port("poll-accept", "ext_net/net_poll_accept.c").out("poll_accept ready=1 got=ping\n"),
            // ---- recv flags ----
            port("msg-peek", "ext_net/net_msg_peek.c")
                .out("msg_peek peeked=peekdata read=peekdata same=1\n"),
            port("msg-dontwait", "ext_net/net_msg_dontwait.c")
                .out("msg_dontwait eagain=1 then=2\n"),
            port("msg-waitall", "ext_net/net_msg_waitall.c")
                .out("msg_waitall n=10 data=ABCDEFGHIJ\n"),
            // ---- vectored / ancillary IO ----
            port("writev", "ext_net/net_writev.c").out("writev w=9 r=9 data=foobarbaz\n"),
            port("sendmsg-addr", "ext_net/net_sendmsg_addr.c").out("sendmsg_addr n=8 lo=1\n"),
            // ---- name introspection / multiplexing ----
            port("getpeername", "ext_net/net_getpeername.c")
                .out("getpeername peer_ok=1 srvport=1\n"),
            port("select-multi", "ext_net/net_select_multi.c").out("select_multi ready=2 both=1\n"),
            // ---- address conversion / resolution ----
            port("getaddrinfo", "ext_net/net_getaddrinfo.c")
                .out("getaddrinfo r=0 ip=127.0.0.1 port_ok=1\n"),
            // ---- container DNS: a query to the embedded nameserver 127.0.0.11:53 is intercepted and
            // resolved via the macOS host resolver, with the source reported as the nameserver. "localhost"
            // is deterministic (127.0.0.1) and needs no external network. Linux-only golden (macOS has no
            // 127.0.0.11 responder -> no native oracle); the same syscall path serves both Linux engines.
            src("dns-hostresolver", "ext_net/net_dns.c")
                .out("dns localhost=127.0.0.1 rcode=0 an=1 src_ok=1\n"),
            port("inet-pton", "ext_net/net_inet_pton.c")
                .out("inet_pton v4=192.168.1.42 v6=2001:db8::1 bad=0\n"),
            port("gethostname", "ext_net/net_gethostname.c").out("gethostname r=0 nonempty=1\n"),
            // ---- interface introspection — Linux-only synth (getifaddrs/netlink/procfs/sysfs) ----
            // dd models lo + eth0; with no DD_IP in the bare matrix eth0 is the synthetic 172.17.0.2/16.
            // Covers getifaddrs, AF_NETLINK RTM_GETADDR (glibc/go-sockaddr/minio/consul path), /proc/net/dev
            // and /sys/class/net. Fixed golden (differs from a native-macOS host) -> src, both Linux engines.
            src("ifaces", "ext_net/net_ifaces.c").out(
                "getifaddrs eth0 ip=172.17.0.2\n\
             getifaddrs lo=1 eth0=1 lo_v4=1 lo_v6=1 eth_v4=1\n\
             netlink RTM_NEWADDR count=3\n\
             procnetdev lo=1 eth0=1\n\
             sysclassnet lo=1 eth0=1 eth0_addr=02:42:ac:11:00:02\n",
            ),
            // a socket the guest bind+listens MUST show up in /proc/net/tcp with state 0A so `ss -l`/
            // `netstat -ln` inside the container list it. dd synthesized only the header -> every listener was
            // invisible. Verdict checks ONLY our own fixed port (host-independent) -> golden on both Linux engines.
            src("listen-tcp", "ext_net/net_listen_tcp.c")
                .out("listen_tcp bind=1 listen=1 seen=1 st_listen=1\n"),
            // ---- error semantics ----
            port("connect-refused", "ext_net/net_connect_refused.c")
                .out("connect_refused refused=1\n"),
            // #261 — IPv4-only container network: a connect() to a genuine external IPv6 address has no route
            // and fails at once with ENETUNREACH (not a 2-min host-v6 timeout), so a happy-eyeballs client
            // that tried the AAAA first (apt/curl) falls straight back to IPv4 without Acquire::ForceIPv4.
            // Fixed golden (dd's IPv4-only contract deliberately differs from a raw v6-capable host, matching
            // a real Docker default-bridge container); same syscall path serves both Linux engines.
            src("v6-unreach", "ext_net/net_v6_unreach.c")
                .out("v6_connect enetunreach=1 fast=1\n"),
            // ---- Linux-only extensions — diffed vs native oracle ----
            src("accept4", "ext_net/net_accept4.c").oracle(), // accept4 flag inheritance (no macOS)
            // SO_PEERCRED returns a zeroed ucred under the JIT (uid/pid not populated). xfail Linux;
            // see GAPS "ext-peercred".
            src("peercred", "ext_net/net_peercred.c").oracle(),
        ],
    )
}
