#ifndef HL_LINUX_ABI_HOST_SOCKET_ABI_H
#define HL_LINUX_ABI_HOST_SOCKET_ABI_H

/* ---- SHAPE: the scalar types the rest of the vocabulary is written in. ---
 *
 * socklen_t is 32 bits on Linux and is passed BY ADDRESS to accept(),
 * getsockname() and getsockopt(), so its width is observable rather than
 * cosmetic.  Note what is deliberately absent: nothing below is spelled with
 * the host's pid_t/uid_t/gid_t.  mingw's pid_t is __int64 on Win64, which would
 * silently grow struct ucred from the 12 bytes the guest ABI expects (and that
 * syscall/net.c checks for before writing SO_PEERCRED) to 20. */
typedef unsigned int socklen_t;
typedef unsigned short sa_family_t;
typedef uint16_t in_port_t;
typedef uint32_t in_addr_t;

/* ---- SHAPE: address families.  Linux values. ---------------------------- */
#define AF_UNSPEC 0
#define AF_UNIX 1
#define AF_LOCAL 1
#define AF_FILE 1
#define AF_INET 2
#define AF_IPX 4
#define AF_APPLETALK 5
#define AF_INET6 10
#define AF_KEY 15
#define AF_NETLINK 16
#define AF_ROUTE 16
#define AF_PACKET 17
#define AF_BLUETOOTH 31
#define AF_VSOCK 40
#define AF_MAX 46

#define PF_UNSPEC AF_UNSPEC
#define PF_UNIX AF_UNIX
#define PF_LOCAL AF_LOCAL
#define PF_INET AF_INET
#define PF_INET6 AF_INET6
#define PF_NETLINK AF_NETLINK
#define PF_PACKET AF_PACKET
#define PF_MAX AF_MAX

/* ---- SHAPE: socket types, plus the two flag bits that ride on the type
 * word.  SOCK_CLOEXEC is 0x80000 and equals O_CLOEXEC / EFD_CLOEXEC /
 * EPOLL_CLOEXEC on Linux; sentry.c relies on that identity when it rewrites a
 * guest's close-on-exec bit without knowing which call produced it, so the
 * value is not free to differ here. ---------------------------------------- */
#define SOCK_STREAM 1
#define SOCK_DGRAM 2
#define SOCK_RAW 3
#define SOCK_RDM 4
#define SOCK_SEQPACKET 5
#define SOCK_DCCP 6
#define SOCK_PACKET 10
#define SOCK_CLOEXEC HL_LINUX_SOCK_CLOEXEC
#define SOCK_NONBLOCK HL_LINUX_SOCK_NONBLOCK
#define SOCK_TYPE_MASK 0xf

/* ---- SHAPE: option levels.  SOL_SOCKET is 1, not Winsock's 0xffff. ------ */
#define SOL_SOCKET 1
#define SOL_IP 0
#define SOL_TCP 6
#define SOL_UDP 17
#define SOL_IPV6 41
#define SOL_RAW 255
#define SOL_NETLINK 270

/* ---- SHAPE: SOL_SOCKET option names.  Linux/x86-64 values. -------------- */
#define SO_DEBUG 1
#define SO_REUSEADDR 2
#define SO_TYPE 3
#define SO_ERROR 4
#define SO_DONTROUTE 5
#define SO_BROADCAST 6
#define SO_SNDBUF 7
#define SO_RCVBUF 8
#define SO_KEEPALIVE 9
#define SO_OOBINLINE 10
#define SO_NO_CHECK 11
#define SO_PRIORITY 12
#define SO_LINGER 13
#define SO_BSDCOMPAT 14
#define SO_REUSEPORT 15
#define SO_PASSCRED 16
#define SO_PEERCRED 17
#define SO_RCVLOWAT 18
#define SO_SNDLOWAT 19
#define SO_RCVTIMEO 20
#define SO_SNDTIMEO 21
#define SO_BINDTODEVICE 25
#define SO_ATTACH_FILTER 26
#define SO_DETACH_FILTER 27
#define SO_PEERNAME 28
#define SO_TIMESTAMP 29
#define SO_ACCEPTCONN 30
#define SO_PEERSEC 31
#define SO_SNDBUFFORCE 32
#define SO_RCVBUFFORCE 33
#define SO_PASSSEC 34
#define SO_MARK 36
#define SO_PROTOCOL 38
#define SO_DOMAIN 39

/* ---- SHAPE: send/recv flags.  Linux values. ----------------------------- */
#define MSG_OOB 0x0001
#define MSG_PEEK 0x0002
#define MSG_DONTROUTE 0x0004
#define MSG_CTRUNC 0x0008
#define MSG_PROXY 0x0010
#define MSG_TRUNC 0x0020
#define MSG_DONTWAIT 0x0040
#define MSG_EOR 0x0080
#define MSG_WAITALL 0x0100
#define MSG_FIN 0x0200
#define MSG_SYN 0x0400
#define MSG_CONFIRM 0x0800
#define MSG_RST 0x1000
#define MSG_ERRQUEUE 0x2000
#define MSG_NOSIGNAL 0x4000
#define MSG_MORE 0x8000
#define MSG_WAITFORONE 0x10000
#define MSG_BATCH 0x40000
#define MSG_ZEROCOPY 0x4000000
#define MSG_FASTOPEN 0x20000000
#define MSG_CMSG_CLOEXEC 0x40000000

/* ---- SHAPE: shutdown(2) directions, and listen(2)'s backlog ceiling. ---- */
#define SHUT_RD 0
#define SHUT_WR 1
#define SHUT_RDWR 2

#define SOMAXCONN 4096

/* ---- SHAPE: ancillary-data types carried at SOL_SOCKET. ----------------- */
#define SCM_RIGHTS 1
#define SCM_CREDENTIALS 2
#define SCM_SECURITY 3

/* ---- SHAPE: the generic address, and the storage that outlives it. -------
 *
 * Linux's sockaddr has NO leading length byte.  That absence is the entire
 * reason container/netns.c carries a sockaddr translation for macOS, whose
 * sockaddr_in begins { u8 sin_len; u8 sin_family; ... }: a raw byte copy
 * between the two makes a guest AF_INET(2) arrive as sin_len=2 with
 * sin_family=AF_UNSPEC, and the server never answers.  Writing the Linux layout
 * here means this host needs no such translation at all. */
struct sockaddr {
    sa_family_t sa_family;
    char sa_data[14];
};

/* 128 bytes, 8-byte aligned, exactly as Linux sizes it.  syscall/net.c uses
 * sizeof(struct sockaddr_storage) as the clamp on a guest's declared sockaddr
 * capacity before writing an accept()ed peer back, so the number is part of the
 * observable behaviour and not an internal detail. */
struct sockaddr_storage {
    sa_family_t ss_family;
    char __ss_padding[128 - sizeof(sa_family_t) - sizeof(unsigned long)];
    unsigned long __ss_align;
};

/* ---- SHAPE: AF_UNIX.  sun_path is 108 bytes on Linux against 104 on macOS.
 * The longer one is the guest's, and the shorter one is why the fchdir-and-
 * shorten dance in container/netns.c exists on that host; nothing here needs
 * it, because nothing here truncates. ------------------------------------- */
#define UNIX_PATH_MAX 108

struct sockaddr_un {
    sa_family_t sun_family;
    char sun_path[UNIX_PATH_MAX];
};

/* ---- SHAPE: AF_INET / AF_INET6. ----------------------------------------- */
struct in_addr {
    in_addr_t s_addr; /* network byte order */
};

/* A plain array rather than glibc's union-plus-`#define s6_addr` alias.  The
 * macro form works on Linux and this tree compiles against it there, but a
 * preprocessor symbol named s6_addr in a unity translation unit that also
 * decodes guest structures buys nothing and can only cost. */
struct in6_addr {
    uint8_t s6_addr[16];
};

#define IN6ADDR_ANY_INIT {{0}}
#define IN6ADDR_LOOPBACK_INIT {{0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1}}

#define INADDR_ANY ((in_addr_t)0x00000000U)
#define INADDR_LOOPBACK ((in_addr_t)0x7f000001U)
#define INADDR_BROADCAST ((in_addr_t)0xffffffffU)
#define INADDR_NONE ((in_addr_t)0xffffffffU)

#define INET_ADDRSTRLEN 16
#define INET6_ADDRSTRLEN 46

struct sockaddr_in {
    sa_family_t sin_family;
    in_port_t sin_port; /* network byte order */
    struct in_addr sin_addr;
    unsigned char sin_zero[8];
};

struct sockaddr_in6 {
    sa_family_t sin6_family;
    in_port_t sin6_port; /* network byte order */
    uint32_t sin6_flowinfo;
    struct in6_addr sin6_addr;
    uint32_t sin6_scope_id;
};

/* SHAPE.  Written over the byte array, so they hold for any alignment of the
 * argument -- container/netns.c applies them to an in6_addr taken from the
 * middle of a sockaddr_storage. */
#define HL_LINUX_IN6_OCTET(a, i) (((const struct in6_addr *)(a))->s6_addr[(i)])

#define HL_LINUX_IN6_TOP12_ZERO(a)                                                                                     \
    (HL_LINUX_IN6_OCTET(a, 0) == 0 && HL_LINUX_IN6_OCTET(a, 1) == 0 && HL_LINUX_IN6_OCTET(a, 2) == 0 &&                \
     HL_LINUX_IN6_OCTET(a, 3) == 0 && HL_LINUX_IN6_OCTET(a, 4) == 0 && HL_LINUX_IN6_OCTET(a, 5) == 0 &&                \
     HL_LINUX_IN6_OCTET(a, 6) == 0 && HL_LINUX_IN6_OCTET(a, 7) == 0 && HL_LINUX_IN6_OCTET(a, 8) == 0 &&                \
     HL_LINUX_IN6_OCTET(a, 9) == 0 && HL_LINUX_IN6_OCTET(a, 10) == 0 && HL_LINUX_IN6_OCTET(a, 11) == 0)

#define IN6_IS_ADDR_UNSPECIFIED(a)                                                                                     \
    (HL_LINUX_IN6_TOP12_ZERO(a) && HL_LINUX_IN6_OCTET(a, 12) == 0 && HL_LINUX_IN6_OCTET(a, 13) == 0 &&                 \
     HL_LINUX_IN6_OCTET(a, 14) == 0 && HL_LINUX_IN6_OCTET(a, 15) == 0)

#define IN6_IS_ADDR_LOOPBACK(a)                                                                                        \
    (HL_LINUX_IN6_TOP12_ZERO(a) && HL_LINUX_IN6_OCTET(a, 12) == 0 && HL_LINUX_IN6_OCTET(a, 13) == 0 &&                 \
     HL_LINUX_IN6_OCTET(a, 14) == 0 && HL_LINUX_IN6_OCTET(a, 15) == 1)

#define IN6_IS_ADDR_V4MAPPED(a)                                                                                        \
    (HL_LINUX_IN6_OCTET(a, 0) == 0 && HL_LINUX_IN6_OCTET(a, 1) == 0 && HL_LINUX_IN6_OCTET(a, 2) == 0 &&                \
     HL_LINUX_IN6_OCTET(a, 3) == 0 && HL_LINUX_IN6_OCTET(a, 4) == 0 && HL_LINUX_IN6_OCTET(a, 5) == 0 &&                \
     HL_LINUX_IN6_OCTET(a, 6) == 0 && HL_LINUX_IN6_OCTET(a, 7) == 0 && HL_LINUX_IN6_OCTET(a, 8) == 0 &&                \
     HL_LINUX_IN6_OCTET(a, 9) == 0 && HL_LINUX_IN6_OCTET(a, 10) == 0xff && HL_LINUX_IN6_OCTET(a, 11) == 0xff)

#define IN6_IS_ADDR_LINKLOCAL(a) (HL_LINUX_IN6_OCTET(a, 0) == 0xfe && (HL_LINUX_IN6_OCTET(a, 1) & 0xc0) == 0x80)
#define IN6_IS_ADDR_SITELOCAL(a) (HL_LINUX_IN6_OCTET(a, 0) == 0xfe && (HL_LINUX_IN6_OCTET(a, 1) & 0xc0) == 0xc0)
#define IN6_IS_ADDR_MULTICAST(a) (HL_LINUX_IN6_OCTET(a, 0) == 0xff)

/* ---- SHAPE: IP protocol numbers.  The one part of this vocabulary that
 * agrees on every host, because the numbers are IANA's and not any kernel's. */
#define IPPROTO_IP 0
#define IPPROTO_ICMP 1
#define IPPROTO_IGMP 2
#define IPPROTO_IPIP 4
#define IPPROTO_TCP 6
#define IPPROTO_EGP 8
#define IPPROTO_PUP 12
#define IPPROTO_UDP 17
#define IPPROTO_IDP 22
#define IPPROTO_TP 29
#define IPPROTO_DCCP 33
#define IPPROTO_IPV6 41
#define IPPROTO_RSVP 46
#define IPPROTO_GRE 47
#define IPPROTO_ESP 50
#define IPPROTO_AH 51
#define IPPROTO_ICMPV6 58
#define IPPROTO_MTP 92
#define IPPROTO_ENCAP 98
#define IPPROTO_PIM 103
#define IPPROTO_COMP 108
#define IPPROTO_SCTP 132
#define IPPROTO_UDPLITE 136
#define IPPROTO_MPLS 137
#define IPPROTO_RAW 255

/* ---- SHAPE: IPPROTO_IP option names, Linux values.  The divergence these
 * reconcile against is real and silent: Linux IP_TOS=1 / IP_TTL=2 /
 * IP_HDRINCL=3 against macOS IP_OPTIONS=1 / IP_HDRINCL=2 / IP_TOS=3, so a
 * pass-through sets a DIFFERENT option rather than failing.  That is what
 * syscall/net.c's ip_opt_l2m() exists for; with these values it is the identity
 * here, exactly as it already is under __linux__. -------------------------- */
#define IP_TOS 1
#define IP_TTL 2
#define IP_HDRINCL 3
#define IP_OPTIONS 4
#define IP_ROUTER_ALERT 5
#define IP_RECVOPTS 6
#define IP_RETOPTS 7
#define IP_PKTINFO 8
#define IP_PKTOPTIONS 9
#define IP_MTU_DISCOVER 10
#define IP_RECVERR 11
#define IP_RECVTTL 12
#define IP_RECVTOS 13
#define IP_MTU 14
#define IP_FREEBIND 15
#define IP_IPSEC_POLICY 16
#define IP_XFRM_POLICY 17
#define IP_PASSSEC 18
#define IP_TRANSPARENT 19
#define IP_MULTICAST_IF 32
#define IP_MULTICAST_TTL 33
#define IP_MULTICAST_LOOP 34
#define IP_ADD_MEMBERSHIP 35
#define IP_DROP_MEMBERSHIP 36
#define IP_UNBLOCK_SOURCE 37
#define IP_BLOCK_SOURCE 38
#define IP_ADD_SOURCE_MEMBERSHIP 39
#define IP_DROP_SOURCE_MEMBERSHIP 40
#define IP_MSFILTER 41

/* ---- SHAPE: IPPROTO_IPV6 option names, Linux values.  IPV6_V6ONLY is the
 * load-bearing one (26 here, 27 on macOS and on Windows): get it wrong and a
 * wildcard `::` bind stays dual-stack, reserving the v4 wildcard too, so a
 * later 0.0.0.0 bind on the same port fails EADDRINUSE.  26, because 26 is what
 * the guest means. --------------------------------------------------------- */
#define IPV6_ADDRFORM 1
#define IPV6_UNICAST_HOPS 16
#define IPV6_MULTICAST_IF 17
#define IPV6_MULTICAST_HOPS 18
#define IPV6_MULTICAST_LOOP 19
#define IPV6_ADD_MEMBERSHIP 20
#define IPV6_DROP_MEMBERSHIP 21
#define IPV6_JOIN_GROUP 20
#define IPV6_LEAVE_GROUP 21
#define IPV6_V6ONLY 26
#define IPV6_RECVPKTINFO 49
#define IPV6_PKTINFO 50
#define IPV6_RECVHOPLIMIT 51
#define IPV6_HOPLIMIT 52
#define IPV6_RECVTCLASS 66
#define IPV6_TCLASS 67

/* ---- SHAPE: the payload structures those option names take. ------------- */
struct ip_mreq {
    struct in_addr imr_multiaddr;
    struct in_addr imr_interface;
};

struct ip_mreqn {
    struct in_addr imr_multiaddr;
    struct in_addr imr_address;
    int imr_ifindex;
};

struct ipv6_mreq {
    struct in6_addr ipv6mr_multiaddr;
    unsigned int ipv6mr_interface;
};

struct in_pktinfo {
    int ipi_ifindex;
    struct in_addr ipi_spec_dst;
    struct in_addr ipi_addr;
};

struct in6_pktinfo {
    struct in6_addr ipi6_addr;
    unsigned int ipi6_ifindex;
};

/* ---- SHAPE: IPPROTO_TCP option names, and the connection-state names that
 * share the TCP_ prefix with them.  Two namespaces, one spelling: Linux keeps
 * the options as macros and the states as an anonymous enum so that both can
 * coexist, and so does this. ----------------------------------------------- */
#define TCP_NODELAY 1
#define TCP_MAXSEG 2
#define TCP_CORK 3
#define TCP_KEEPIDLE 4
#define TCP_KEEPINTVL 5
#define TCP_KEEPCNT 6
#define TCP_SYNCNT 7
#define TCP_LINGER2 8
#define TCP_DEFER_ACCEPT 9
#define TCP_WINDOW_CLAMP 10
#define TCP_INFO 11
#define TCP_QUICKACK 12
#define TCP_CONGESTION 13
#define TCP_MD5SIG 14
#define TCP_USER_TIMEOUT 18
#define TCP_FASTOPEN 23

enum {
    TCP_ESTABLISHED = 1,
    TCP_SYN_SENT = 2,
    TCP_SYN_RECV = 3,
    TCP_FIN_WAIT1 = 4,
    TCP_FIN_WAIT2 = 5,
    TCP_TIME_WAIT = 6,
    TCP_CLOSE = 7,
    TCP_CLOSE_WAIT = 8,
    TCP_LAST_ACK = 9,
    TCP_LISTEN = 10,
    TCP_CLOSING = 11
};

/* ---- SHAPE: option payloads carried at SOL_SOCKET. -----------------------
 *
 * struct ucred is written with fixed-width members on purpose.  Linux makes it
 * 12 bytes and syscall/net.c gates its SO_PEERCRED writeback on the guest
 * offering at least 12; spelling the members pid_t/uid_t/gid_t would make it 20
 * on Win64, where mingw's pid_t is __int64.  The names are the ones callers
 * use, the widths are the ones the wire uses. */
struct linger {
    int l_onoff;
    int l_linger;
};

struct ucred {
    int32_t pid;
    uint32_t uid;
    uint32_t gid;
};

/* ---- SHAPE: scatter/gather message headers. -----------------------------
 *
 * Linux's msghdr is 56 bytes on x86-64, with msg_iovlen and msg_controllen as
 * size_t (macOS makes both 32-bit, which is why the encoders in syscall/net.c
 * cast on the way in and back out).  The 56 is not incidental: sentry.c
 * rebuilds this header field by field at fixed byte offsets when it relays a
 * message through its private window -- msg_namelen at +8, msg_controllen at
 * +40, msg_flags at +48 -- and any other layout desynchronizes those writes
 * from the struct the same file then passes to recvmsg. */
struct msghdr {
    void *msg_name;
    socklen_t msg_namelen;
    struct iovec *msg_iov;
    size_t msg_iovlen;
    void *msg_control;
    size_t msg_controllen;
    int msg_flags;
};

struct cmsghdr {
    size_t cmsg_len;
    int cmsg_level;
    int cmsg_type;
};

struct mmsghdr {
    struct msghdr msg_hdr;
    unsigned int msg_len;
};

/* SHAPE.  Linux aligns control records to sizeof(size_t), which is 8 on
 * x86-64 -- the same 8 that container/netns.c hard-codes as LX_CMSG_ALIGN when
 * it lays out the GUEST's control buffer, so the two agree by construction
 * rather than by luck.  CMSG_SPACE must stay an integer constant expression:
 * sentry.c and checkpoint.c size stack arrays with it. */
#define CMSG_ALIGN(len) (((len) + sizeof(size_t) - 1) & (size_t)~(sizeof(size_t) - 1))
#define CMSG_SPACE(len) (CMSG_ALIGN(len) + CMSG_ALIGN(sizeof(struct cmsghdr)))
#define CMSG_LEN(len) (CMSG_ALIGN(sizeof(struct cmsghdr)) + (len))
#define CMSG_DATA(cmsg) ((unsigned char *)((struct cmsghdr *)(cmsg) + 1))

#define CMSG_FIRSTHDR(mhdr)                                                                                            \
    ((size_t)(mhdr)->msg_controllen >= sizeof(struct cmsghdr) ? (struct cmsghdr *)(mhdr)->msg_control                  \
                                                              : (struct cmsghdr *)0)

/* A function rather than a macro for the same reason glibc uses one: the walk
 * has to bounds-check the NEXT record against the control buffer twice -- once
 * for its header and once for its declared length -- and a macro would evaluate
 * its arguments several times while doing it. */
static inline struct cmsghdr *hl_linux_cmsg_nxthdr(struct msghdr *message, struct cmsghdr *record) {
    unsigned char *limit;
    if (message == NULL || record == NULL || message->msg_control == NULL) return NULL;
    if (record->cmsg_len < sizeof(struct cmsghdr)) return NULL;
    limit = (unsigned char *)message->msg_control + message->msg_controllen;
    record = (struct cmsghdr *)((unsigned char *)record + CMSG_ALIGN(record->cmsg_len));
    if ((unsigned char *)(record + 1) > limit) return NULL;
    if ((unsigned char *)record + CMSG_ALIGN(record->cmsg_len) > limit) return NULL;
    return record;
}

#define CMSG_NXTHDR(mhdr, cmsg) hl_linux_cmsg_nxthdr((mhdr), (cmsg))

/* ---- SHAPE: <netdb.h>. --------------------------------------------------- */
#define AI_PASSIVE 0x0001
#define AI_CANONNAME 0x0002
#define AI_NUMERICHOST 0x0004
#define AI_V4MAPPED 0x0008
#define AI_ALL 0x0010
#define AI_ADDRCONFIG 0x0020
#define AI_NUMERICSERV 0x0400

#define NI_NUMERICHOST 1
#define NI_NUMERICSERV 2
#define NI_NOFQDN 4
#define NI_NAMEREQD 8
#define NI_DGRAM 16
#define NI_MAXHOST 1025
#define NI_MAXSERV 32

#define EAI_BADFLAGS (-1)
#define EAI_NONAME (-2)
#define EAI_AGAIN (-3)
#define EAI_FAIL (-4)
#define EAI_NODATA (-5)
#define EAI_FAMILY (-6)
#define EAI_SOCKTYPE (-7)
#define EAI_SERVICE (-8)
#define EAI_ADDRFAMILY (-9)
#define EAI_MEMORY (-10)
#define EAI_SYSTEM (-11)
#define EAI_OVERFLOW (-12)

/* glibc's member order, with ai_addr ahead of ai_canonname.  container/netns.c
 * walks ai_next and reads ai_family and ai_addr, so the order is observed
 * rather than decorative. */
struct addrinfo {
    int ai_flags;
    int ai_family;
    int ai_socktype;
    int ai_protocol;
    socklen_t ai_addrlen;
    struct sockaddr *ai_addr;
    char *ai_canonname;
    struct addrinfo *ai_next;
};

struct hostent {
    char *h_name;
    char **h_aliases;
    int h_addrtype;
    int h_length;
    char **h_addr_list;
};

struct servent {
    char *s_name;
    char **s_aliases;
    int s_port;
    char *s_proto;
};

struct timespec;

/* =========================================================================
 * REAL -- <arpa/inet.h>'s text conversions.
 *
 * The only entries in this file that do their job.  inet_pton and inet_ntop
 * read and write a caller's own buffer and consult nothing else: no descriptor,
 * no resolver, no kernel.  There is therefore no host object for Windows to be
 * missing, and refusing them would be a fiction rather than an honest absence.
 * container/netns.c's DNS responder calls inet_pton(AF_INET) on the private
 * loopback address it was configured with; that call gets the right answer on
 * this host, today.
 * ========================================================================= */

static inline int hl_linux_hexdigit(int character) {
    if (character >= '0' && character <= '9') return character - '0';
    if (character >= 'a' && character <= 'f') return character - 'a' + 10;
    if (character >= 'A' && character <= 'F') return character - 'A' + 10;
    return -1;
}

/* Dotted quad -> four network-order bytes.  Strict on purpose: exactly four
 * decimal octets, no leading zeros, no trailing text.  Leading zeros are
 * rejected rather than interpreted because implementations disagree about
 * whether "010" is 8 or 10, and a parser that picks one silently is the shape
 * of a whole class of address-confusion bugs. */
static inline int hl_linux_pton4(const char *text, unsigned char *out) {
    unsigned char octets[4] = {0, 0, 0, 0};
    int index = 0, digits = 0;
    unsigned int value = 0;
    for (;;) {
        char character = *text++;
        if (character >= '0' && character <= '9') {
            if (digits > 0 && value == 0) return 0; /* leading zero */
            if (++digits > 3) return 0;
            value = value * 10u + (unsigned int)(character - '0');
            if (value > 255u) return 0;
            octets[index] = (unsigned char)value;
        } else if (character == '.') {
            if (digits == 0 || index == 3) return 0;
            index++;
            digits = 0;
            value = 0;
        } else if (character == '\0') {
            if (digits == 0 || index != 3) return 0;
            break;
        } else {
            return 0;
        }
    }
    memcpy(out, octets, 4);
    return 1;
}

/* Colon-hex, with an optional "::" run and an optional trailing dotted quad,
 * -> sixteen network-order bytes.  The shape is the long-settled BSD algorithm:
 * accumulate groups left-aligned, remember where the "::" was, then slide the
 * accumulated tail right to meet the end of the buffer. */
static inline int hl_linux_pton6(const char *text, unsigned char *out) {
    unsigned char bytes[16];
    unsigned char *cursor, *limit, *gap = NULL;
    const char *group_start;
    unsigned int value = 0;
    int seen = 0, character;

    memset(bytes, 0, sizeof bytes);
    cursor = bytes;
    limit = bytes + sizeof bytes;

    if (*text == ':' && *++text != ':') return 0; /* a lone leading ':' is not an address */
    group_start = text;

    while ((character = (unsigned char)*text++) != '\0') {
        int digit = hl_linux_hexdigit(character);
        if (digit >= 0) {
            if (++seen > 4) return 0;
            value = (value << 4) | (unsigned int)digit;
            continue;
        }
        if (character == ':') {
            group_start = text;
            if (seen == 0) {
                if (gap != NULL) return 0; /* at most one "::" */
                gap = cursor;
                continue;
            }
            if (*text == '\0') return 0; /* a single trailing ':' ends nothing */
            if (cursor + 2 > limit) return 0;
            *cursor++ = (unsigned char)(value >> 8);
            *cursor++ = (unsigned char)(value & 0xffu);
            seen = 0;
            value = 0;
            continue;
        }
        if (character == '.' && cursor + 4 <= limit && hl_linux_pton4(group_start, cursor)) {
            cursor += 4; /* embedded v4 tail; pton4 already required the NUL */
            seen = 0;
            break;
        }
        return 0;
    }
    if (seen > 0) {
        if (cursor + 2 > limit) return 0;
        *cursor++ = (unsigned char)(value >> 8);
        *cursor++ = (unsigned char)(value & 0xffu);
    }
    if (gap != NULL) {
        ptrdiff_t tail = cursor - gap, index;
        if (cursor == limit) return 0; /* "::" must stand for at least one group */
        for (index = 1; index <= tail; index++) {
            limit[-index] = gap[tail - index];
            gap[tail - index] = 0;
        }
        cursor = limit;
    }
    if (cursor != limit) return 0;
    memcpy(out, bytes, sizeof bytes);
    return 1;
}

/* REAL.  1 on success, 0 on a malformed address, -1 with EAFNOSUPPORT for a
 * family this function does not describe.  That last one is inet_pton(3)'s own
 * documented contract and is the single place in this file where EAFNOSUPPORT
 * is the truth rather than a misleading invitation to fall back. */
static inline int inet_pton(int family, const char *text, void *out) {
    if (text == NULL || out == NULL) {
        errno = EINVAL;
        return -1;
    }
    if (family == AF_INET) return hl_linux_pton4(text, (unsigned char *)out);
    if (family == AF_INET6) return hl_linux_pton6(text, (unsigned char *)out);
    errno = EAFNOSUPPORT;
    return -1;
}

/* REAL.  RFC 5952 presentation: lowercase hex, no leading zeros within a group,
 * the LONGEST run of two or more zero groups replaced by "::", and a v4-mapped
 * or v4-compatible tail rendered as a dotted quad. */
static inline const char *hl_linux_ntop6(const unsigned char *address, char *out, size_t capacity) {
    char text[INET6_ADDRSTRLEN];
    unsigned int groups[8];
    int best = -1, best_length = 0, run = -1, run_length = 0, index;
    char *cursor = text;

    for (index = 0; index < 8; index++)
        groups[index] = ((unsigned int)address[2 * index] << 8) | (unsigned int)address[2 * index + 1];

    for (index = 0; index < 8; index++) {
        if (groups[index] == 0) {
            if (run < 0) {
                run = index;
                run_length = 1;
            } else {
                run_length++;
            }
            if (run_length > best_length) {
                best = run;
                best_length = run_length;
            }
        } else {
            run = -1;
            run_length = 0;
        }
    }
    if (best_length < 2) best = -1; /* a single zero group is written out, not elided */

    for (index = 0; index < 8; index++) {
        if (best >= 0 && index >= best && index < best + best_length) {
            if (index == best) *cursor++ = ':';
            continue;
        }
        if (index != 0) *cursor++ = ':';
        if (index == 6 && best == 0 && (best_length == 6 || (best_length == 5 && groups[5] == 0xffffu))) {
            cursor += snprintf(cursor, sizeof text - (size_t)(cursor - text), "%u.%u.%u.%u", address[12], address[13],
                               address[14], address[15]);
            break;
        }
        cursor += snprintf(cursor, sizeof text - (size_t)(cursor - text), "%x", groups[index]);
    }
    if (best >= 0 && best + best_length == 8) *cursor++ = ':';
    *cursor = '\0';

    if ((size_t)(cursor - text) + 1 > capacity) {
        errno = ENOSPC;
        return NULL;
    }
    memcpy(out, text, (size_t)(cursor - text) + 1);
    return out;
}

static inline const char *inet_ntop(int family, const void *address, char *out, socklen_t capacity) {
    if (address == NULL || out == NULL) {
        errno = EINVAL;
        return NULL;
    }
    if (family == AF_INET) {
        const unsigned char *octets = (const unsigned char *)address;
        char text[INET_ADDRSTRLEN];
        int written = snprintf(text, sizeof text, "%u.%u.%u.%u", octets[0], octets[1], octets[2], octets[3]);
        if (written < 0 || (size_t)written + 1 > (size_t)capacity) {
            errno = ENOSPC;
            return NULL;
        }
        memcpy(out, text, (size_t)written + 1);
        return out;
    }
    if (family == AF_INET6) return hl_linux_ntop6((const unsigned char *)address, out, (size_t)capacity);
    errno = EAFNOSUPPORT;
    return NULL;
}

/* REAL.  The pre-inet_pton spellings, kept because callers still reach for
 * them.  inet_addr's INADDR_NONE ambiguity -- it reports the same value for
 * "255.255.255.255" and for "that is not an address" -- is inherited
 * deliberately rather than repaired: it is the documented behaviour every
 * caller was written against, and quietly fixing it here would make this host's
 * inet_addr disagree with the other two. */
static inline in_addr_t inet_addr(const char *text) {
    in_addr_t value = 0;
    if (text == NULL || !hl_linux_pton4(text, (unsigned char *)&value)) return INADDR_NONE;
    return value;
}

static inline int inet_aton(const char *text, struct in_addr *out) {
    if (text == NULL || out == NULL) return 0;
    return hl_linux_pton4(text, (unsigned char *)&out->s_addr);
}

/* REAL, and thread-unsafe exactly as inet_ntoa(3) has always been -- the static
 * buffer IS the interface, not an oversight here.  One buffer per translation
 * unit that includes this header, which is no weaker a guarantee than the
 * one-per-process the real implementations offer. */
static inline char *inet_ntoa(struct in_addr address) {
    static char text[INET_ADDRSTRLEN];
    const unsigned char *octets = (const unsigned char *)&address.s_addr;
    snprintf(text, sizeof text, "%u.%u.%u.%u", octets[0], octets[1], octets[2], octets[3]);
    return text;
}

#endif
