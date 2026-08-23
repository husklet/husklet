// Capability transitions across a uid change, per Linux `cap_emulate_setxuid` (security/commoncap.c).
// The container starts as root holding the docker default set. Each child performs ONE transition and
// reports what the kernel would leave behind, read back through /proc/self/status (CapEff/CapPrm/CapAmb)
// AND through capget(2), with the uid transition itself asserted so the case cannot pass vacuously:
//   drop       - setresuid(2000,2000,2000): every uid nonzero -> permitted AND effective cleared, and the
//                capability-gated PR_SET_SECUREBITS / PR_CAPBSET_DROP / capset-re-raise all become EPERM;
//   keepcaps   - PR_SET_KEEPCAPS then the same drop: permitted survives, effective still cleared, and a
//                capset() re-raises effective from permitted (this is exactly what setpriv(1) does);
//   euid_only  - setresuid(-1,2000,-1) then back: effective cleared on 0->nonzero and restored FROM
//                PERMITTED on nonzero->0, while permitted is never gained by a uid change;
//   fsuid      - setfsuid(2000) moves only CAP_FS_MASK out of effective, and setfsuid(0) puts it back;
//   stay_root  - setresgid(3000,3000,3000) and setresuid(0,0,0): a container that never drops root must
//                see byte-identical capability sets, since group transitions do not touch capabilities.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/fsuid.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define CAP_NET_RAW 13
#define DOCKER_CAP 0x00000000a80425fbULL
// CAP_FS_MASK: CHOWN, DAC_OVERRIDE, DAC_READ_SEARCH, FOWNER, FSETID, LINUX_IMMUTABLE, MKNOD, MAC_*.
#define FS_MASK (0x1fULL | (1ULL << 9) | (1ULL << 27) | (1ULL << 32) | (1ULL << 33))
#define CAP_FOWNER_BIT (1ULL << 3)

struct chdr {
    unsigned version;
    int pid;
};

struct cdata {
    unsigned eff, prm, inh;
};

static int capget_sets(unsigned long long *eff, unsigned long long *prm) {
    struct chdr h = {0x20080522u, 0};
    struct cdata d[2];
    memset(d, 0, sizeof d);
    if (syscall(SYS_capget, &h, d) != 0) return 0;
    *eff = ((unsigned long long)d[1].eff << 32) | d[0].eff;
    *prm = ((unsigned long long)d[1].prm << 32) | d[0].prm;
    return 1;
}

static int capset_sets(unsigned long long eff, unsigned long long prm) {
    struct chdr h = {0x20080522u, 0};
    struct cdata d[2];
    memset(d, 0, sizeof d);
    d[0].eff = (unsigned)eff;
    d[1].eff = (unsigned)(eff >> 32);
    d[0].prm = (unsigned)prm;
    d[1].prm = (unsigned)(prm >> 32);
    return (int)syscall(SYS_capset, &h, d);
}

static int status_line(const char *key, char *out, int n) {
    char b[8192];
    int fd = open("/proc/self/status", O_RDONLY), o = 0, r;
    if (fd < 0) return 0;
    while (o < (int)sizeof b - 1 && (r = (int)read(fd, b + o, sizeof b - 1 - o)) > 0)
        o += r;
    close(fd);
    b[o] = 0;
    size_t kl = strlen(key);
    for (char *p = b; p && *p;) {
        if (!strncmp(p, key, kl)) {
            char *v = p + kl;
            while (*v == ' ' || *v == '\t')
                v++;
            int i = 0;
            while (v[i] && v[i] != '\n' && i < n - 1) {
                out[i] = v[i];
                i++;
            }
            out[i] = 0;
            return 1;
        }
        char *nl = strchr(p, '\n');
        p = nl ? nl + 1 : 0;
    }
    return 0;
}

static unsigned long long status_hex(const char *key) {
    char v[64];
    return status_line(key, v, sizeof v) ? strtoull(v, 0, 16) : ~0ULL;
}

static int status_ids(const char *key, long id[4]) {
    char v[128];
    if (!status_line(key, v, sizeof v)) return 0;
    int i = 0;
    for (char *t = strtok(v, " \t"); t && i < 4; t = strtok(NULL, " \t"))
        id[i++] = strtol(t, 0, 10);
    return i == 4;
}

// The capability sets agree between capget(2) and /proc/self/status, and equal what was asked for.
static int sets_are(unsigned long long eff, unsigned long long prm) {
    unsigned long long ceff = 0, cprm = 0;
    return capget_sets(&ceff, &cprm) && ceff == eff && cprm == prm && status_hex("CapEff:") == eff &&
           status_hex("CapPrm:") == prm;
}

// The uid transition really happened: getresuid, getuid/geteuid and the status Uid: columns all agree.
static int uid_is(uid_t real, uid_t effective, uid_t saved) {
    uid_t r, e, s;
    long col[4];
    if (getresuid(&r, &e, &s) != 0) return 0;
    return r == real && e == effective && s == saved && getuid() == real && geteuid() == effective &&
           status_ids("Uid:", col) && col[0] == (long)real && col[1] == (long)effective && col[2] == (long)saved;
}

static int run_child(int (*body)(void)) {
    pid_t child = fork();
    if (child == 0) _exit(body() ? 0 : 1);
    int st = 0;
    if (waitpid(child, &st, 0) != child) return 0;
    return WIFEXITED(st) && WEXITSTATUS(st) == 0;
}

// An ordinary privilege drop: permitted and effective both go, and nothing can bring them back.
static int case_drop(void) {
    int ok = sets_are(DOCKER_CAP, DOCKER_CAP) && uid_is(0, 0, 0);
    ok = ok && setresgid(3000, 3000, 3000) == 0 && setresuid(2000, 2000, 2000) == 0;
    ok = ok && uid_is(2000, 2000, 2000);
    ok = ok && sets_are(0, 0) && status_hex("CapAmb:") == 0;
    // CAP_FOWNER is gone, so the sticky-bit and W+X exemptions no longer apply to this task.
    ok = ok && (status_hex("CapEff:") & CAP_FOWNER_BIT) == 0;
    // Every capability-gated operation the container root could perform is now refused.
    errno = 0;
    ok = ok && capset_sets(DOCKER_CAP, DOCKER_CAP) == -1 && errno == EPERM;
    errno = 0;
    ok = ok && prctl(PR_SET_SECUREBITS, 0, 0, 0, 0) == -1 && errno == EPERM;
    errno = 0;
    ok = ok && prctl(PR_CAPBSET_DROP, CAP_NET_RAW, 0, 0, 0) == -1 && errno == EPERM;
    errno = 0;
    ok = ok && setuid(0) == -1 && errno == EPERM;
    return ok && sets_are(0, 0);
}

// PR_SET_KEEPCAPS: permitted survives the all-nonzero drop, effective does not, and capset re-raises it.
static int case_keepcaps(void) {
    int ok = prctl(PR_SET_KEEPCAPS, 1, 0, 0, 0) == 0;
    ok = ok && setresuid(2000, 2000, 2000) == 0 && uid_is(2000, 2000, 2000);
    ok = ok && sets_are(0, DOCKER_CAP);
    errno = 0;
    ok = ok && prctl(PR_SET_SECUREBITS, 0, 0, 0, 0) == -1 && errno == EPERM;
    ok = ok && capset_sets(DOCKER_CAP, DOCKER_CAP) == 0 && sets_are(DOCKER_CAP, DOCKER_CAP);
    ok = ok && prctl(PR_SET_SECUREBITS, 0, 0, 0, 0) == 0;
    // With CAP_SETUID back in effective this task may return to root, which is exactly why keepcaps is
    // security relevant; the drop child, whose permitted set went to zero, can never get here.
    ok = ok && setuid(0) == 0 && uid_is(0, 0, 0);
    return ok && sets_are(DOCKER_CAP, DOCKER_CAP);
}

// seteuid away from root and back: effective follows euid, permitted is untouched throughout.
static int case_euid_only(void) {
    int ok = setresuid(-1, 2000, -1) == 0 && uid_is(0, 2000, 0);
    ok = ok && sets_are(0, DOCKER_CAP);
    ok = ok && setresuid(-1, 0, -1) == 0 && uid_is(0, 0, 0);
    return ok && sets_are(DOCKER_CAP, DOCKER_CAP);
}

// setfsuid moves only the filesystem capabilities, and only inside the effective set.
static int case_fsuid(void) {
    long col[4];
    int ok = setfsuid(2000) == 0 && status_ids("Uid:", col) && col[3] == 2000 && col[1] == 0;
    ok = ok && sets_are(DOCKER_CAP & ~FS_MASK, DOCKER_CAP);
    ok = ok && setfsuid(0) == 2000 && status_ids("Uid:", col) && col[3] == 0;
    return ok && sets_are(DOCKER_CAP, DOCKER_CAP);
}

// A container that never drops root sees byte-identical capability sets across every id syscall.
static int case_stay_root(void) {
    int ok = sets_are(DOCKER_CAP, DOCKER_CAP);
    ok = ok && setresgid(3000, 3000, 3000) == 0 && setgid(4000) == 0 && setregid(-1, 5000) == 0;
    ok = ok && sets_are(DOCKER_CAP, DOCKER_CAP);
    ok = ok && setresuid(0, 0, 0) == 0 && setuid(0) == 0 && setreuid(-1, 0) == 0 && setfsuid(0) == 0;
    ok = ok && uid_is(0, 0, 0) && sets_are(DOCKER_CAP, DOCKER_CAP);
    // Still privileged: the gated operations that the dropped child was refused all still succeed.
    return ok && prctl(PR_SET_SECUREBITS, 0, 0, 0, 0) == 0 && capset_sets(DOCKER_CAP, DOCKER_CAP) == 0;
}

int main(void) {
    if (geteuid() != 0) {
        printf("capability-transition ok=0 not-root\n");
        return 0;
    }
    int drop = run_child(case_drop);
    int keepcaps = run_child(case_keepcaps);
    int euid_only = run_child(case_euid_only);
    int fsuid = run_child(case_fsuid);
    int stay_root = run_child(case_stay_root);
    // The parent never transitioned, so its own sets must be untouched by all of the above.
    int parent = sets_are(DOCKER_CAP, DOCKER_CAP) && uid_is(0, 0, 0);
    if (drop && keepcaps && euid_only && fsuid && stay_root && parent)
        printf("capability-transition ok=1\n");
    else
        printf("capability-transition ok=0 drop=%d keepcaps=%d euid=%d fsuid=%d root=%d parent=%d\n", drop, keepcaps,
               euid_only, fsuid, stay_root, parent);
    return 0;
}
