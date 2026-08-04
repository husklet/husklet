// NETLINK_ROUTE cross-interface consistency + RTM_GETLINK single-query edges. Every printed field is a
// protocol-stable fact or a native<->engine consistency boolean, so the golden matches on real Linux and
// on the JIT's synthesized lo+eth0 model alike (interface index/MAC VALUES differ and are never printed).
//   * per interface: netlink RTM_GETLINK agrees with the SIOCGIF* ioctls and if_name<->index round-trips;
//   * a non-dump RTM_GETLINK for a bogus ifindex yields NLMSG_ERROR/-ENODEV (not a spurious link dump).
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <net/if.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>

static int nl_open(void) {
    int fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    if (fd < 0) return -1;
    struct sockaddr_nl sa;
    memset(&sa, 0, sizeof sa);
    sa.nl_family = AF_NETLINK;
    if (bind(fd, (struct sockaddr *)&sa, sizeof sa) < 0) { close(fd); return -1; }
    return fd;
}

// Dump all links; return the RTM_GETLINK view of `want` via out-params. found=0 if absent.
static void getlink_dump(const char *want, int *found, int *type, int *index, int *mtu, int *flags,
                         unsigned char *mac) {
    *found = 0; *type = *index = *mtu = *flags = -1;
    memset(mac, 0, 6);
    int fd = nl_open();
    if (fd < 0) return;
    struct { struct nlmsghdr nh; struct ifinfomsg ifi; } req;
    memset(&req, 0, sizeof req);
    req.nh.nlmsg_len = NLMSG_LENGTH(sizeof req.ifi);
    req.nh.nlmsg_type = RTM_GETLINK;
    req.nh.nlmsg_flags = NLM_F_REQUEST | NLM_F_DUMP;
    req.nh.nlmsg_seq = 1;
    req.ifi.ifi_family = AF_UNSPEC;
    send(fd, &req, req.nh.nlmsg_len, 0);
    char buf[8192];
    int done = 0;
    while (!done) {
        ssize_t len = recv(fd, buf, sizeof buf, 0);
        if (len <= 0) break;
        for (struct nlmsghdr *nh = (struct nlmsghdr *)buf; NLMSG_OK(nh, len); nh = NLMSG_NEXT(nh, len)) {
            if (nh->nlmsg_type == NLMSG_DONE || nh->nlmsg_type == NLMSG_ERROR) { done = 1; break; }
            if (nh->nlmsg_type != RTM_NEWLINK) continue;
            struct ifinfomsg *ifi = NLMSG_DATA(nh);
            char name[64] = "";
            int lmtu = -1, ml = 0;
            unsigned char m[6] = {0};
            int rlen = nh->nlmsg_len - NLMSG_LENGTH(sizeof *ifi);
            for (struct rtattr *rta = IFLA_RTA(ifi); RTA_OK(rta, rlen); rta = RTA_NEXT(rta, rlen)) {
                if (rta->rta_type == IFLA_IFNAME) snprintf(name, sizeof name, "%s", (char *)RTA_DATA(rta));
                else if (rta->rta_type == IFLA_MTU) lmtu = *(int *)RTA_DATA(rta);
                else if (rta->rta_type == IFLA_ADDRESS) {
                    ml = RTA_PAYLOAD(rta);
                    if (ml > 6) ml = 6;
                    memcpy(m, RTA_DATA(rta), ml);
                }
            }
            if (strcmp(name, want) == 0) {
                *found = 1;
                *type = ifi->ifi_type;
                *index = ifi->ifi_index;
                *mtu = lmtu;
                *flags = ifi->ifi_flags;
                memcpy(mac, m, 6);
            }
        }
    }
    close(fd);
}

static void ioctl_facts(const char *name, int *idx, int *mtu, int *flags, unsigned char *mac) {
    *idx = *mtu = *flags = -1;
    memset(mac, 0, 6);
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    if (s < 0) return;
    struct ifreq ifr;
    memset(&ifr, 0, sizeof ifr);
    snprintf(ifr.ifr_name, IFNAMSIZ, "%s", name);
    if (ioctl(s, SIOCGIFINDEX, &ifr) == 0) *idx = ifr.ifr_ifindex;
    if (ioctl(s, SIOCGIFMTU, &ifr) == 0) *mtu = ifr.ifr_mtu;
    if (ioctl(s, SIOCGIFFLAGS, &ifr) == 0) *flags = (unsigned short)ifr.ifr_flags;
    if (ioctl(s, SIOCGIFHWADDR, &ifr) == 0) memcpy(mac, ifr.ifr_hwaddr.sa_data, 6);
    close(s);
}

// Report only protocol-stable facts (type/mtu/flags are the same on real Linux and the model) plus a
// single cross-interface consistency boolean folding the index/MAC values that legitimately differ.
static void report(const char *name, int expect_type, int expect_mtu, int expect_flags) {
    int found, type, nidx, nmtu, nflags;
    unsigned char nmac[6];
    getlink_dump(name, &found, &type, &nidx, &nmtu, &nflags, nmac);
    int iidx, imtu, iflags;
    unsigned char imac[6];
    ioctl_facts(name, &iidx, &imtu, &iflags, imac);
    unsigned nti = if_nametoindex(name);
    char inm[IF_NAMESIZE] = "";
    if (nidx > 0) if_indextoname(nidx, inm);
    int xconsistent = (nidx == iidx) && ((unsigned)nidx == nti) && (nmtu == imtu) &&
                      ((nflags & 0xffff) == (iflags & 0xffff)) && (memcmp(nmac, imac, 6) == 0);
    printf("%s found=%d type=%d mtu=%d flags=0x%x xconsistent=%d roundtrip=%d\n", name, found, type, nmtu,
           nflags & 0xffff, xconsistent, strcmp(inm, name) == 0);
    // The stable protocol facts (identical on native and the model) come last so a mismatch is obvious.
    printf("%s stable type_ok=%d mtu_ok=%d flags_ok=%d\n", name, type == expect_type, nmtu == expect_mtu,
           (nflags & 0xffff) == expect_flags);
}

int main(void) {
    report("lo", 772 /*ARPHRD_LOOPBACK*/, 65536, 0x49 /*UP|LOOPBACK|RUNNING*/);
    report("eth0", 1 /*ARPHRD_ETHER*/, 1500, 0x1043 /*UP|BROADCAST|RUNNING|MULTICAST*/);

    // Non-dump RTM_GETLINK for a bogus ifindex -> Linux answers NLMSG_ERROR/-ENODEV, never a link dump.
    int fd = nl_open();
    struct { struct nlmsghdr nh; struct ifinfomsg ifi; } req;
    memset(&req, 0, sizeof req);
    req.nh.nlmsg_len = NLMSG_LENGTH(sizeof req.ifi);
    req.nh.nlmsg_type = RTM_GETLINK;
    req.nh.nlmsg_flags = NLM_F_REQUEST; // single-object query, not a dump
    req.nh.nlmsg_seq = 7;
    req.ifi.ifi_family = AF_UNSPEC;
    req.ifi.ifi_index = 250;
    send(fd, &req, req.nh.nlmsg_len, 0);
    char buf[8192];
    ssize_t len = recv(fd, buf, sizeof buf, 0);
    int is_error = 0, enodev = 0, multi = 0;
    struct nlmsghdr *nh = (struct nlmsghdr *)buf;
    if (len > 0 && NLMSG_OK(nh, len)) {
        multi = (nh->nlmsg_flags & NLM_F_MULTI) ? 1 : 0;
        if (nh->nlmsg_type == NLMSG_ERROR) {
            is_error = 1;
            enodev = (((struct nlmsgerr *)NLMSG_DATA(nh))->error == -ENODEV);
        }
    }
    close(fd);
    printf("bogus-index error=%d enodev=%d multi=%d\n", is_error, enodev, multi);
    return 0;
}
