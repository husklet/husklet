// Socket-level interface ioctls used by ifconfig/getifaddrs fallback and net
// diagnostics: SIOCGIFCONF (list + NULL-buf size probe), per-interface
// SIOCGIF{ADDR,NETMASK,FLAGS,INDEX,MTU,HWADDR,NAME}, and the error edges
// (nonexistent interface -> ENODEV, bad request -> ENOTTY). The engine serves a
// synthesized container interface set (lo + eth0) which differs from the bare
// oracle host's extra interfaces, so this asserts only container-invariant facts
// about lo: loopback 127.0.0.1/8, IFF_UP|IFF_LOOPBACK, index 1, name-from-index.
#include "net_util.h"
#include <net/if.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <sys/ioctl.h>
#include <sys/socket.h>

int main(void) {
    net_watchdog(10);
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) { printf("socket=FAIL\n"); return 1; }

    // NULL-buffer size probe: kernel reports required byte length, no fill.
    struct ifconf ifc;
    memset(&ifc, 0, sizeof ifc);
    ifc.ifc_buf = NULL;
    int r = ioctl(fd, SIOCGIFCONF, &ifc);
    printf("ifconf_sizeprobe=%s len_positive=%d len_aligned=%d\n",
           r < 0 ? err_name(errno) : "OK",
           r == 0 && ifc.ifc_len > 0,
           r == 0 && (ifc.ifc_len % (int)sizeof(struct ifreq)) == 0);

    // Real fill: find lo and its address.
    char buf[8192];
    ifc.ifc_buf = buf;
    ifc.ifc_len = sizeof buf;
    r = ioctl(fd, SIOCGIFCONF, &ifc);
    int have_lo = 0, lo_is_loopback_addr = 0;
    if (r == 0) {
        int n = ifc.ifc_len / (int)sizeof(struct ifreq);
        for (int i = 0; i < n; i++) {
            struct ifreq *p = &ifc.ifc_req[i];
            if (strcmp(p->ifr_name, "lo") == 0) {
                have_lo = 1;
                struct sockaddr_in *sa = (struct sockaddr_in *)&p->ifr_addr;
                if (sa->sin_family == AF_INET && sa->sin_addr.s_addr == htonl(INADDR_LOOPBACK))
                    lo_is_loopback_addr = 1;
            }
        }
    }
    printf("ifconf_fill=%s lo_present=%d lo_addr_127=%d\n",
           r < 0 ? err_name(errno) : "OK", have_lo, lo_is_loopback_addr);

    struct ifreq ifr;
    memset(&ifr, 0, sizeof ifr);
    strcpy(ifr.ifr_name, "lo");
    r = ioctl(fd, SIOCGIFADDR, &ifr);
    struct sockaddr_in *sa = (struct sockaddr_in *)&ifr.ifr_addr;
    printf("lo_addr=%s is127=%d\n", r < 0 ? err_name(errno) : "OK",
           r == 0 && sa->sin_addr.s_addr == htonl(INADDR_LOOPBACK));

    memset(&ifr, 0, sizeof ifr);
    strcpy(ifr.ifr_name, "lo");
    r = ioctl(fd, SIOCGIFNETMASK, &ifr);
    sa = (struct sockaddr_in *)&ifr.ifr_netmask;
    printf("lo_mask=%s is8=%d\n", r < 0 ? err_name(errno) : "OK",
           r == 0 && sa->sin_addr.s_addr == htonl(0xff000000));

    memset(&ifr, 0, sizeof ifr);
    strcpy(ifr.ifr_name, "lo");
    r = ioctl(fd, SIOCGIFFLAGS, &ifr);
    printf("lo_flags=%s up=%d loopback=%d\n", r < 0 ? err_name(errno) : "OK",
           r == 0 && (ifr.ifr_flags & IFF_UP) != 0,
           r == 0 && (ifr.ifr_flags & IFF_LOOPBACK) != 0);

    memset(&ifr, 0, sizeof ifr);
    strcpy(ifr.ifr_name, "lo");
    r = ioctl(fd, SIOCGIFINDEX, &ifr);
    printf("lo_index=%s is1=%d\n", r < 0 ? err_name(errno) : "OK",
           r == 0 && ifr.ifr_ifindex == 1);

    memset(&ifr, 0, sizeof ifr);
    strcpy(ifr.ifr_name, "lo");
    r = ioctl(fd, SIOCGIFMTU, &ifr);
    printf("lo_mtu=%s positive=%d\n", r < 0 ? err_name(errno) : "OK",
           r == 0 && ifr.ifr_mtu > 0);

    memset(&ifr, 0, sizeof ifr);
    strcpy(ifr.ifr_name, "lo");
    r = ioctl(fd, SIOCGIFHWADDR, &ifr);
    printf("lo_hwaddr=%s loopfam=%d\n", r < 0 ? err_name(errno) : "OK",
           r == 0 && ifr.ifr_hwaddr.sa_family == 772 /* ARPHRD_LOOPBACK */);

    // index 1 -> name lo
    memset(&ifr, 0, sizeof ifr);
    ifr.ifr_ifindex = 1;
    r = ioctl(fd, SIOCGIFNAME, &ifr);
    printf("name_idx1=%s islo=%d\n", r < 0 ? err_name(errno) : "OK",
           r == 0 && strcmp(ifr.ifr_name, "lo") == 0);

    // nonexistent interface -> ENODEV
    memset(&ifr, 0, sizeof ifr);
    strcpy(ifr.ifr_name, "hlmissing0");
    r = ioctl(fd, SIOCGIFADDR, &ifr);
    printf("nodev=%s\n", r < 0 ? err_name(errno) : "OK");

    // unknown request -> ENOTTY
    r = ioctl(fd, 0x0000ABCD, &ifr);
    printf("badioctl=%s\n", r < 0 ? err_name(errno) : "OK");

    close(fd);
    return 0;
}
