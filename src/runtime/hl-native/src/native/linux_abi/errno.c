#include "errno.h"

int hl_linux_errno_from_darwin(int host_errno) {
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
}

int hl_linux_errno_from_ucrt(int host_errno) {
    /* The UCRT has a sparse classic block and a dense POSIX-2008 block. */
    static const short low[43] = {
        0,  1,  2,  3,  4,  5,  6,  7,  8,  9, 10, 11, 12, 13, 14, 22, 16, 17, 18, 19, 20, 21,
        22, 23, 24, 25, 22, 27, 28, 29, 30, 31, 32, 33, 34, 22, 35, 22, 36, 37, 38, 39, 84,
    };
    static const short high[41] = {
        98, 99, 97, 114, 74, 125, 103, 111, 104, 89, 113, 43, 115, 106, 40, 90, 100, 102, 101, 105, 61,
        67, 42, 92, 63, 60, 107, 131, 88, 95, 95, 22, 75, 130, 71, 93, 91, 62, 110, 26, 11,
    };
    if (host_errno >= 0 && host_errno < (int)(sizeof(low) / sizeof(low[0]))) return low[host_errno];
    if (host_errno >= 100 && host_errno < 100 + (int)(sizeof(high) / sizeof(high[0]))) return high[host_errno - 100];
    return host_errno > 0 && host_errno <= 140 ? 22 : host_errno;
}

int hl_linux_errno_from_host(int host_errno) {
#if defined(__linux__)
    return host_errno;
#elif defined(_WIN32)
    return hl_linux_errno_from_ucrt(host_errno);
#else
    return hl_linux_errno_from_darwin(host_errno);
#endif
}
