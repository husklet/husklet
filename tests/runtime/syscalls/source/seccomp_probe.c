// seccomp(2) capability-probe operations that container runtimes (libseccomp/runc) call before installing
// a profile, plus classic-BPF socket attach. All arch-neutral (the action words and error codes are the
// same on aarch64 and x86_64):
//   - SECCOMP_GET_ACTION_AVAIL (op 2): every canonical SECCOMP_RET_* action word (incl USER_NOTIF) is
//     available (0); an action word carrying data bits, or an unknown one, is EOPNOTSUPP; a nonzero flag is
//     EINVAL.
//   - SECCOMP_GET_NOTIF_SIZES (op 3): succeeds and fills nonzero struct sizes.
//   - SECCOMP_SET_MODE_FILTER with an unknown flag or a zero-length program is EINVAL.
//   - SO_ATTACH_FILTER installs a classic-BPF socket filter (accepted); SO_DETACH_FILTER with no filter
//     attached is EINVAL.
// Deterministic verdict (return-class + errno; the notif sizes are printed only as "nonzero") so the JIT
// stdout is byte-identical to native.
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <linux/filter.h>

#define SECCOMP_SET_MODE_FILTER 1u
#define SECCOMP_GET_ACTION_AVAIL 2u
#define SECCOMP_GET_NOTIF_SIZES 3u
#define S_RET_KILL_PROCESS 0x80000000u
#define S_RET_KILL_THREAD 0x00000000u
#define S_RET_TRAP 0x00030000u
#define S_RET_ERRNO 0x00050000u
#define S_RET_USER_NOTIF 0x7fc00000u
#define S_RET_TRACE 0x7ff00000u
#define S_RET_LOG 0x7ffc0000u
#define S_RET_ALLOW 0x7fff0000u

static long sc(unsigned op, unsigned flags, void *arg) {
    errno = 0;
    return syscall(SYS_seccomp, op, flags, arg);
}
static void avail(const char *name, uint32_t action) {
    long r = sc(SECCOMP_GET_ACTION_AVAIL, 0, &action);
    printf("avail_%-13s r=%ld e=%d\n", name, r < 0 ? -1L : 0, r < 0 ? errno : 0);
}

int main(void) {
    avail("kill_process", S_RET_KILL_PROCESS);
    avail("kill_thread", S_RET_KILL_THREAD);
    avail("trap", S_RET_TRAP);
    avail("errno", S_RET_ERRNO);
    avail("user_notif", S_RET_USER_NOTIF);
    avail("trace", S_RET_TRACE);
    avail("log", S_RET_LOG);
    avail("allow", S_RET_ALLOW);
    avail("errno_data", S_RET_ERRNO | 13u); // data bits set -> unavailable
    avail("bogus", 0x12345678u);
    {
        uint32_t a = S_RET_ALLOW;
        long r = sc(SECCOMP_GET_ACTION_AVAIL, 1 /*bad flag*/, &a);
        printf("avail_flag_einval r=%ld e=%d\n", r < 0 ? -1L : 0, r < 0 ? errno : 0);
    }
    {
        struct { uint16_t notif, resp, data; } sz = {0, 0, 0};
        long r = sc(SECCOMP_GET_NOTIF_SIZES, 0, &sz);
        printf("notif_sizes r=%ld e=%d nonzero=%d\n", r < 0 ? -1L : 0, r < 0 ? errno : 0,
               (sz.notif | sz.resp | sz.data) != 0);
    }
    syscall(SYS_prctl, 38 /*PR_SET_NO_NEW_PRIVS*/, 1, 0, 0, 0);
    {
        struct sock_fprog p = {.len = 1, .filter = (struct sock_filter[]){BPF_STMT(BPF_RET | BPF_K, S_RET_ALLOW)}};
        long r = sc(SECCOMP_SET_MODE_FILTER, 0x80 /*unknown flag*/, &p);
        printf("filter_badflag r=%ld e=%d\n", r < 0 ? -1L : 0, r < 0 ? errno : 0);
    }
    {
        struct sock_fprog p = {.len = 0, .filter = NULL};
        long r = sc(SECCOMP_SET_MODE_FILTER, 0, &p);
        printf("filter_len0 r=%ld e=%d\n", r < 0 ? -1L : 0, r < 0 ? errno : 0);
    }
    {
        int s = socket(AF_INET, SOCK_DGRAM, 0);
        struct sock_fprog p = {.len = 1, .filter = (struct sock_filter[]){BPF_STMT(BPF_RET | BPF_K, 0)}};
        errno = 0;
        int a = setsockopt(s, SOL_SOCKET, SO_ATTACH_FILTER, &p, sizeof p);
        printf("so_attach_filter r=%d e=%d\n", a < 0 ? -1 : 0, a < 0 ? errno : 0);
        errno = 0;
        int d = setsockopt(s, SOL_SOCKET, SO_DETACH_FILTER, NULL, 0);
        printf("so_detach_filter r=%d e=%d\n", d < 0 ? -1 : 0, d < 0 ? errno : 0);
        close(s);
    }
    return 0;
}
