//! Networking — sockets over loopback (TCP/UDP/UNIX), client+server across a fork.

use dd_tests::{group, port, Group};

/// Networking — sockets over loopback (TCP/UDP/UNIX), client+server across a fork. PORTABLE: the one
/// POSIX source runs on every engine (Linux x2 + macOS), golden-checked so the behaviour must be
/// byte-identical across platforms. The acid test that a real networked service (postgres/redis shape)
/// works the same emulated-on-Linux and native-on-macOS.
pub(super) fn net() -> Group {
    group(
        "net",
        vec![
            port("tcp", "net_tcp.c").out("tcp echo=HELLO-SOCKET exit=0\n"), // socket/bind/listen/accept/connect
            port("udp", "net_udp.c").out("udp echo=datagram-42\n"), // SOCK_DGRAM sendto/recvfrom
            port("unix", "net_unix.c").out("unix reply=sum=335\n"), // AF_UNIX socketpair full-duplex
            port("sockopt", "net_sockopt.c").out("sockopt reuse=1 nodelay=1 soerr=0 ok=1\n"), // get/setsockopt
            port("nonblock", "net_nonblock.c").out("nonblock inprogress=1 writable=1 soerr=0\n"), // async connect
            port("sendmsg", "net_sendmsg.c").out("sendmsg sent=6 got=6 data=ABCDEF\n"), // sendmsg/recvmsg iovec
        ],
    )
}
