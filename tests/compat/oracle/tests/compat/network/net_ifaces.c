// #289: container network-interface introspection — getifaddrs + AF_NETLINK RTM_GETADDR + procfs/sysfs.
// Prints a deterministic summary so the JIT (Linux synth) can be checked. Loopback-only / local.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <dirent.h>
#include <ifaddrs.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>

static int nl_count_addr(void) {
    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    if (fd < 0) { printf("netlink socket=FAIL\n"); return -1; }
    struct sockaddr_nl sa;
    memset(&sa, 0, sizeof sa);
    sa.nl_family = AF_NETLINK;
    if (bind(fd, (struct sockaddr *)&sa, sizeof sa) < 0) { printf("netlink bind=FAIL\n"); close(fd); return -1; }
    struct {
        struct nlmsghdr nh;
        struct ifaddrmsg ifa;
    } req;
    memset(&req, 0, sizeof req);
    req.nh.nlmsg_len = NLMSG_LENGTH(sizeof req.ifa);
    req.nh.nlmsg_type = RTM_GETADDR;
    req.nh.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    req.nh.nlmsg_seq = 1;
    req.ifa.ifa_family = AF_UNSPEC;
    if (send(fd, &req, req.nh.nlmsg_len, 0) < 0) { printf("netlink send=FAIL\n"); close(fd); return -1; }
    char buf[8192];
    int naddr = 0, done = 0;
    while (!done) {
        ssize_t len = recv(fd, buf, sizeof buf, 0);
        if (len <= 0) break;
        for (struct nlmsghdr *nh = (struct nlmsghdr *)buf; NLMSG_OK(nh, len); nh = NLMSG_NEXT(nh, len)) {
            if (nh->nlmsg_type == NLMSG_DONE) { done = 1; break; }
            if (nh->nlmsg_type == NLMSG_ERROR) { done = 1; break; }
            if (nh->nlmsg_type == RTM_NEWADDR) naddr++;
        }
    }
    close(fd);
    return naddr;
}

int main(void) {
    // 1) getifaddrs
    struct ifaddrs *ifa, *p;
    int have_lo = 0, have_eth = 0, lo_v4 = 0, lo_v6 = 0, eth_v4 = 0;
    if (getifaddrs(&ifa) == 0) {
        for (p = ifa; p; p = p->ifa_next) {
            if (!p->ifa_addr) continue;
            int fam = p->ifa_addr->sa_family;
            if (!strcmp(p->ifa_name, "lo")) {
                have_lo = 1;
                if (fam == AF_INET) lo_v4 = 1;
                if (fam == AF_INET6) lo_v6 = 1;
            } else if (!strcmp(p->ifa_name, "eth0")) {
                have_eth = 1;
                if (fam == AF_INET) {
                    eth_v4 = 1;
                    char ip[64];
                    struct sockaddr_in *s = (struct sockaddr_in *)p->ifa_addr;
                    inet_ntop(AF_INET, &s->sin_addr, ip, sizeof ip);
                    printf("getifaddrs eth0 ip=%s\n", ip);
                }
            }
        }
        freeifaddrs(ifa);
    } else {
        printf("getifaddrs=FAIL\n");
    }
    printf("getifaddrs lo=%d eth0=%d lo_v4=%d lo_v6=%d eth_v4=%d\n", have_lo, have_eth, lo_v4, lo_v6, eth_v4);

    // 2) netlink RTM_GETADDR
    int na = nl_count_addr();
    printf("netlink RTM_NEWADDR count=%d\n", na);

    // 3) /proc/net/dev
    FILE *f = fopen("/proc/net/dev", "r");
    int pd_lo = 0, pd_eth = 0;
    if (f) {
        char line[512];
        while (fgets(line, sizeof line, f)) {
            if (strstr(line, "lo:")) pd_lo = 1;
            if (strstr(line, "eth0:")) pd_eth = 1;
        }
        fclose(f);
    }
    printf("procnetdev lo=%d eth0=%d\n", pd_lo, pd_eth);

    // 4) /sys/class/net
    DIR *d = opendir("/sys/class/net");
    int sy_lo = 0, sy_eth = 0;
    if (d) {
        struct dirent *e;
        while ((e = readdir(d))) {
            if (!strcmp(e->d_name, "lo")) sy_lo = 1;
            if (!strcmp(e->d_name, "eth0")) sy_eth = 1;
        }
        closedir(d);
    }
    char mac[64] = "";
    FILE *mf = fopen("/sys/class/net/eth0/address", "r");
    if (mf) { if (fgets(mac, sizeof mac, mf)) { mac[strcspn(mac, "\n")] = 0; } fclose(mf); }
    printf("sysclassnet lo=%d eth0=%d eth0_addr=%s\n", sy_lo, sy_eth, mac);
    return 0;
}
