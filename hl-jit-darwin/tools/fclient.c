// fclient.c -- tiny ddjitd (fork-server) client, protocol v2. Models a resident orchestrator
// (hl-dockerd) dispatching a launch:
//   ./fclient SOCK PROG [args...]            -- one launch, exit with the guest's code
//   ./fclient --bench N SOCK PROG [args...]  -- N internal round-trips, print median/min/p75 ms
// The whole point of the fork-server is that the *launcher* is already resident, so the marginal
// launch cost is server-fork + worker-run, NOT a fresh posix_spawn+dyld of the engine. This tiny
// binary measures exactly that marginal cost. (One-shot launches from a cold shell can just use the
// engine's own `hljit --client SOCK PROG ...` mode; that pays one engine spawn for the CLIENT.)
//
// Protocol v2 (see hl-jit/src/runtime/os/linux/forkserver.c):
//   -> [u32 'DDF2'][u32 body_len] [i32 argc][argc*(i32 len+NUL-str)] [i32 envc][envc*...]
//      [i32 cwdlen(+NUL)][cwd]  + stdio fds 0/1/2 via SCM_RIGHTS on the first sendmsg
//   <- [i32 runner_pid] then [i32 raw wait status]
// Build (macOS host): clang -O2 -o fclient hl-tests/tools/fclient.c
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <time.h>
#include <signal.h>
#include <sys/wait.h>
#include <sys/socket.h>
#include <sys/un.h>

extern char **environ;

#define FSRV_MAGIC 0x32464444u // "DDF2" LE
#define BUFSZ (256 * 1024)

static double now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000.0 + t.tv_nsec / 1e6;
}

static int send_fds(int s, const void *buf, size_t len, const int *fds, int nfd) {
    struct iovec iov = {.iov_base = (void *)buf, .iov_len = len};
    char cbuf[CMSG_SPACE(sizeof(int) * 8)];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr mh;
    memset(&mh, 0, sizeof mh);
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = CMSG_SPACE(sizeof(int) * nfd);
    struct cmsghdr *cm = CMSG_FIRSTHDR(&mh);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int) * nfd);
    memcpy(CMSG_DATA(cm), fds, sizeof(int) * nfd);
    ssize_t r;
    do { r = sendmsg(s, &mh, 0); } while (r < 0 && errno == EINTR);
    return r < 0 ? -1 : 0;
}

static int recv_full(int s, void *buf, size_t n) {
    char *p = buf;
    while (n) {
        ssize_t r = recv(s, p, n, 0);
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0) return -1;
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

static int pack_vec(char *out, size_t cap, size_t *o, int n, char *const v[]) {
    if (*o + 4 > cap) return -1;
    int32_t n32 = n;
    memcpy(out + *o, &n32, 4);
    *o += 4;
    for (int i = 0; i < n; i++) {
        int32_t l = (int32_t)strlen(v[i]) + 1;
        if (*o + 4 + (size_t)l > cap) return -1;
        memcpy(out + *o, &l, 4);
        *o += 4;
        memcpy(out + *o, v[i], (size_t)l);
        *o += (size_t)l;
    }
    return 0;
}

// One launch. Returns the runner's raw wait status, or -1 on protocol error.
static int one_launch(const char *sock, int argc, char *const argv[], int devnull) {
    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s", sock);
    if (connect(s, (struct sockaddr *)&un, sizeof un) < 0) { perror("connect"); close(s); return -1; }
    static char buf[BUFSZ];
    size_t o = 8;
    int nenv = 0;
    while (environ && environ[nenv]) nenv++;
    char cwd[4096];
    if (!getcwd(cwd, sizeof cwd)) cwd[0] = 0;
    int32_t cl = cwd[0] ? (int32_t)strlen(cwd) + 1 : 0;
    if (pack_vec(buf, sizeof buf, &o, argc, argv) || pack_vec(buf, sizeof buf, &o, nenv, environ) ||
        o + 4 + (size_t)cl > sizeof buf) {
        fprintf(stderr, "request too large\n");
        close(s);
        return -1;
    }
    memcpy(buf + o, &cl, 4);
    o += 4;
    if (cl) { memcpy(buf + o, cwd, (size_t)cl); o += (size_t)cl; }
    uint32_t magic = FSRV_MAGIC, blen = (uint32_t)(o - 8);
    memcpy(buf, &magic, 4);
    memcpy(buf + 4, &blen, 4);
    // In bench mode redirect the guest's stdio to /dev/null so output doesn't spam; otherwise pass ours.
    int fds[3] = {devnull >= 0 ? devnull : 0, devnull >= 0 ? devnull : 1, devnull >= 0 ? devnull : 2};
    if (send_fds(s, buf, o, fds, 3) < 0) { close(s); return -1; }
    int32_t rpid = -1, st = -1;
    if (recv_full(s, &rpid, 4) || rpid <= 0 || recv_full(s, &st, 4)) { close(s); return -1; }
    close(s);
    return (int)st;
}

static int cmp(const void *a, const void *b) {
    double d = *(const double *)a - *(const double *)b;
    return d < 0 ? -1 : d > 0 ? 1 : 0;
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "--bench") == 0) {
        int n = atoi(argv[2]);
        const char *sock = argv[3];
        int gc = argc - 4;
        char **gv = argv + 4;
        int devnull = open("/dev/null", O_WRONLY);
        for (int i = 0; i < 3; i++) one_launch(sock, gc, gv, devnull); // warmup
        double *ts = malloc(sizeof(double) * n);
        int lastst = 0;
        for (int i = 0; i < n; i++) {
            double t0 = now_ms();
            lastst = one_launch(sock, gc, gv, devnull);
            ts[i] = now_ms() - t0;
        }
        qsort(ts, n, sizeof(double), cmp);
        printf("median=%.3f ms  min=%.3f  p75=%.3f  max=%.3f  rc_last=%d  n=%d\n",
               ts[n / 2], ts[0], ts[(int)(n * 0.75)], ts[n - 1],
               lastst >= 0 && WIFEXITED(lastst) ? WEXITSTATUS(lastst) : -1, n);
        return 0;
    }
    if (argc < 3) { fprintf(stderr, "usage: fclient [--bench N] SOCK PROG [args...]\n"); return 2; }
    int st = one_launch(argv[1], argc - 2, argv + 2, -1);
    if (st < 0) return 125;
    if (WIFSIGNALED(st)) { int sig = WTERMSIG(st); signal(sig, SIG_DFL); raise(sig); return 128 + sig; }
    return WIFEXITED(st) ? WEXITSTATUS(st) : 125;
}
