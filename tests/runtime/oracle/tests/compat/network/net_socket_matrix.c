// Socket create type/protocol matrix + SOCK_RAW privilege + socketpair(AF_INET)
// + bad family/type. All facts are stack-level and container-invariant, so the
// native oracle and the engine's synthesized network view must agree exactly.
// Unprivileged (uid maps to a non-CAP_NET_RAW context on the oracle VM): SOCK_RAW
// must be EPERM, never allowed (isolation) and never ENOSYS/crash.
#include "net_util.h"
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>

static void mk(const char *tag, int dom, int typ, int proto) {
    int fd = socket(dom, typ, proto);
    int e = fd < 0 ? errno : 0;
    printf("%s=%s\n", tag, err_name(e));
    if (fd >= 0) close(fd);
}

int main(void) {
    net_watchdog(10);
    mk("stream0", AF_INET, SOCK_STREAM, 0);
    mk("stream_tcp", AF_INET, SOCK_STREAM, IPPROTO_TCP);
    mk("dgram0", AF_INET, SOCK_DGRAM, 0);
    mk("dgram_udp", AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    mk("stream_udp", AF_INET, SOCK_STREAM, IPPROTO_UDP);
    mk("dgram_tcp", AF_INET, SOCK_DGRAM, IPPROTO_TCP);
    mk("stream_flags", AF_INET, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    mk("inet6_stream", AF_INET6, SOCK_STREAM, 0);
    mk("inet6_dgram", AF_INET6, SOCK_DGRAM, 0);
    mk("unspec", AF_UNSPEC, SOCK_STREAM, 0);
    mk("badfam", AF_MAX + 50, SOCK_STREAM, 0);
    mk("badtype", AF_INET, 0x999, 0);
    mk("raw_ip", AF_INET, SOCK_RAW, IPPROTO_RAW);
    mk("raw_icmp", AF_INET, SOCK_RAW, IPPROTO_ICMP);
    mk("raw_tcp", AF_INET, SOCK_RAW, IPPROTO_TCP);
    mk("raw6_icmp6", AF_INET6, SOCK_RAW, IPPROTO_ICMPV6);
    mk("packet_raw", AF_PACKET, SOCK_RAW, 3);

    int sv[2];
    int r = socketpair(AF_INET, SOCK_STREAM, 0, sv);
    printf("socketpair_inet=%s\n", err_name(r < 0 ? errno : 0));
    if (r == 0) { close(sv[0]); close(sv[1]); }
    return 0;
}
