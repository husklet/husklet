#include "errno.h"

int hl_linux_errno_from_macos(int host_errno) {
#if defined(__linux__)
    /* The native Linux host already uses the guest errno namespace. */
    return host_errno;
#elif defined(_WIN32)
    /* The UCRT's errno namespace is neither Linux's nor Darwin's, and it is
     * sparse: 1..42 are the C89/POSIX-classic numbers with four holes, then a
     * gap, then a dense 100..140 block the UCRT added for the POSIX-2008 names.
     * Feeding it to the Darwin table below silently mistranslated every value
     * that differs -- measured worst case, a refusal's ENOSYS (UCRT 40) reached
     * the guest as Linux 90 EMSGSIZE, so a "not implemented" arrived claiming
     * the message was too long. Two dense sub-tables, one per block.
     *
     * Slots the UCRT does not assign (15, 26, 35, 37, and 43..99) cannot arise
     * from a UCRT call; they map to EINVAL rather than passing through, which
     * is the same convention the Darwin table uses for its unassigned numbers.
     * EOTHER (131) has no Linux counterpart at all and takes EINVAL for real. */
    static const short low[43] = {
        /*   0.. 9 */ 0,  1,  2,  3,  4,  5,  6,  7,  8,  9,
        /*  10..19 */ 10, 11, 12, 13, 14, 22, 16, 17, 18, 19,
        /*  20..29 */ 20, 21, 22, 23, 24, 25, 22, 27, 28, 29,
        /*  30..39 */ 30, 31, 32, 33, 34, 22, 35, 22, 36, 37,
        /*  40..42 */ 38, 39, 84,
    };
    static const short high[41] = {
        /* 100 EADDRINUSE     */ 98,
        /* 101 EADDRNOTAVAIL  */ 99,
        /* 102 EAFNOSUPPORT   */ 97,
        /* 103 EALREADY       */ 114,
        /* 104 EBADMSG        */ 74,
        /* 105 ECANCELED      */ 125,
        /* 106 ECONNABORTED   */ 103,
        /* 107 ECONNREFUSED   */ 111,
        /* 108 ECONNRESET     */ 104,
        /* 109 EDESTADDRREQ   */ 89,
        /* 110 EHOSTUNREACH   */ 113,
        /* 111 EIDRM          */ 43,
        /* 112 EINPROGRESS    */ 115,
        /* 113 EISCONN        */ 106,
        /* 114 ELOOP          */ 40,
        /* 115 EMSGSIZE       */ 90,
        /* 116 ENETDOWN       */ 100,
        /* 117 ENETRESET      */ 102,
        /* 118 ENETUNREACH    */ 101,
        /* 119 ENOBUFS        */ 105,
        /* 120 ENODATA        */ 61,
        /* 121 ENOLINK        */ 67,
        /* 122 ENOMSG         */ 42,
        /* 123 ENOPROTOOPT    */ 92,
        /* 124 ENOSR          */ 63,
        /* 125 ENOSTR         */ 60,
        /* 126 ENOTCONN       */ 107,
        /* 127 ENOTRECOVERABLE*/ 131,
        /* 128 ENOTSOCK       */ 88,
        /* 129 ENOTSUP        */ 95,
        /* 130 EOPNOTSUPP     */ 95,
        /* 131 EOTHER         */ 22,
        /* 132 EOVERFLOW      */ 75,
        /* 133 EOWNERDEAD     */ 130,
        /* 134 EPROTO         */ 71,
        /* 135 EPROTONOSUPPORT*/ 93,
        /* 136 EPROTOTYPE     */ 91,
        /* 137 ETIME          */ 62,
        /* 138 ETIMEDOUT      */ 110,
        /* 139 ETXTBSY        */ 26,
        /* 140 EWOULDBLOCK    */ 11,
    };
    if (host_errno >= 0 && host_errno < (int)(sizeof(low) / sizeof(low[0]))) return low[host_errno];
    if (host_errno >= 100 && host_errno < 100 + (int)(sizeof(high) / sizeof(high[0]))) return high[host_errno - 100];
    /* 43..99 are unassigned by the UCRT; anything above 140 is not a UCRT errno
     * at all (a negated NTSTATUS, say). Neither can be named in Linux terms. */
    return host_errno > 0 && host_errno <= 140 ? 22 : host_errno;
#else
    /* Indexed by Darwin errno; values are Linux errno numbers. Unknown values pass through. */
    static const short linux_errno[107] = {
        0,  1,   2,  3,   4,   5,  6,   7,   8,   9,   10,  35,  12,  13,  14,  15,  16,  17,  18, 19, 20,  21,
        22, 23,  24, 25,  26,  27, 28,  29,  30,  31,  32,  33,  34,  11,  115, 114, 88,  89,  90, 91, 92,  93,
        94, 95,  96, 97,  98,  99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 40, 36, 112, 113,
        39, 22,  87, 122, 116, 66, 22,  22,  22,  22,  22,  37,  38,  22,  22,  22,  22,  22,  75, 22, 22,  22,
        22, 125, 43, 42,  84,  61, 74,  72,  61,  67,  63,  60,  71,  62,  95,  22,  131, 130, 22,
    };
    return host_errno >= 0 && host_errno < (int)(sizeof(linux_errno) / sizeof(linux_errno[0])) ? linux_errno[host_errno]
                                                                                               : host_errno;
#endif
}
