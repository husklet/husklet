// Shared helpers for network compat guests: symbolic errno names and a bounded
// watchdog so a hung socket call fails deterministically instead of blocking the
// matrix runner. Arch-neutral: Linux errno numbers are identical across ISAs, but
// names read clearly in golden output.
#ifndef NET_UTIL_H
#define NET_UTIL_H

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static const char *err_name(int e) {
    switch (e) {
        case 0: return "OK";
        case EAGAIN: return "EAGAIN";
        case ECONNREFUSED: return "ECONNREFUSED";
        case ENOTCONN: return "ENOTCONN";
        case EISCONN: return "EISCONN";
        case EINPROGRESS: return "EINPROGRESS";
        case EPIPE: return "EPIPE";
        case ECONNRESET: return "ECONNRESET";
        case EADDRINUSE: return "EADDRINUSE";
        case EINVAL: return "EINVAL";
        case EMSGSIZE: return "EMSGSIZE";
        case EAFNOSUPPORT: return "EAFNOSUPPORT";
        case EDESTADDRREQ: return "EDESTADDRREQ";
        case EBADF: return "EBADF";
        case ENOTSOCK: return "ENOTSOCK";
        case EOPNOTSUPP: return "EOPNOTSUPP";
        case EPERM: return "EPERM";
        case EACCES: return "EACCES";
        case EPROTONOSUPPORT: return "EPROTONOSUPPORT";
        case EPROTOTYPE: return "EPROTOTYPE";
        case ESOCKTNOSUPPORT: return "ESOCKTNOSUPPORT";
        case EADDRNOTAVAIL: return "EADDRNOTAVAIL";
        case ENODEV: return "ENODEV";
        case ENOTTY: return "ENOTTY";
        case ENOSYS: return "ENOSYS";
        default: return "OTHER";
    }
}

// Kill the guest after `seconds` so a wrong-behavior hang surfaces as a failure
// rather than stalling the runner. Never fires on correct, bounded runs.
static void net_watchdog(unsigned seconds) {
    signal(SIGALRM, SIG_DFL);
    alarm(seconds);
}

#endif
