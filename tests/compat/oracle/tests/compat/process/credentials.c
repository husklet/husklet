// Process credential + capability conformance for a default `docker run` ROOT container (HL_UID=0/HL_GID=0).
// The container's guest identity is FIXED (root), and every query path must agree AND round-trip:
//   - getuid/geteuid/getgid/getegid/getresuid/getresgid are self-consistent and equal the /proc/self/status
//     Uid:/Gid: 4-column lines (real/effective/saved/fs);
//   - a privileged setresgid+setresuid DROP is reflected in the subsequent getres*id AND the status lines, and
//     the dropped-privilege task can NOT regain root (setuid(0) -> EPERM);
//   - capset() narrows the EFFECTIVE set visibly in capget(2) AND status CapEff;
//   - PR_CAPBSET_DROP clears a bounding bit visibly in PR_CAPBSET_READ AND status CapBnd;
//   - PR_SET_NO_NEW_PRIVS is reflected in PR_GET_NO_NEW_PRIVS + status NoNewPrivs and is irreversible;
//   - PR_SET_KEEPCAPS / PR_GET_SECUREBITS / setfsuid round-trip.
// Reported as a single boolean (ok=1) exactly like selfcaps; validated by running THROUGH the engine.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/fsuid.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PR_GET_SECUREBITS
#define PR_GET_SECUREBITS 27
#endif
#define CAP_KILL 5
#define CAP_SETPCAP 8
#define CAP_NET_RAW 13
#define DOCKER_CAP 0x00000000a80425fbULL

struct chdr { unsigned version; int pid; };
struct cdata { unsigned eff, prm, inh; };

static unsigned long long capget_eff(void) {
    struct chdr h = {0x20080522u, 0};
    struct cdata d[2];
    memset(d, 0, sizeof d);
    if (syscall(SYS_capget, &h, d) != 0) return ~0ULL;
    return ((unsigned long long)d[1].eff << 32) | d[0].eff;
}
static int capset_eff(unsigned long long eff) {
    struct chdr h = {0x20080522u, 0};
    struct cdata d[2];
    memset(d, 0, sizeof d);
    d[0].eff = (unsigned)eff;
    d[1].eff = (unsigned)(eff >> 32);
    d[0].prm = (unsigned)DOCKER_CAP;
    d[1].prm = (unsigned)(DOCKER_CAP >> 32);
    return (int)syscall(SYS_capset, &h, d);
}

// value of a /proc/self/status line "key\t..." into out; returns 1 if found.
static int status_line(const char *key, char *out, int n) {
    char b[8192];
    int fd = open("/proc/self/status", O_RDONLY), o = 0, r;
    if (fd < 0) return 0;
    while (o < (int)sizeof b - 1 && (r = (int)read(fd, b + o, sizeof b - 1 - o)) > 0) o += r;
    close(fd);
    b[o] = 0;
    size_t kl = strlen(key);
    for (char *p = b; p && *p;) {
        if (!strncmp(p, key, kl)) {
            char *v = p + kl;
            while (*v == ' ' || *v == '\t') v++;
            int i = 0;
            while (v[i] && v[i] != '\n' && i < n - 1) { out[i] = v[i]; i++; }
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
// parse the 4 whitespace ids of a Uid:/Gid: line.
static int status_ids(const char *key, long id[4]) {
    char v[128];
    if (!status_line(key, v, sizeof v)) return 0;
    int i = 0;
    for (char *t = strtok(v, " \t"); t && i < 4; t = strtok(NULL, " \t")) id[i++] = strtol(t, 0, 10);
    return i == 4;
}

int main(void) {
    // ---- query-path consistency: getuid/getresuid/status all agree on the fixed identity ----
    uid_t ru, eu, su;
    gid_t rg, eg, sg;
    getresuid(&ru, &eu, &su);
    getresgid(&rg, &eg, &sg);
    long su_col[4], sg_col[4];
    int uid_consistent = getuid() == geteuid() && ru == geteuid() && eu == geteuid() && su == geteuid() &&
                         status_ids("Uid:", su_col) && su_col[0] == (long)ru && su_col[1] == (long)eu &&
                         su_col[2] == (long)su && su_col[3] == (long)geteuid();
    int gid_consistent = getgid() == getegid() && rg == getegid() && eg == getegid() && sg == getegid() &&
                         status_ids("Gid:", sg_col) && sg_col[0] == (long)rg && sg_col[1] == (long)eg &&
                         sg_col[2] == (long)sg && sg_col[3] == (long)getegid();

    // setresuid(-1,-1,-1) is a no-op; the ids read back unchanged.
    int noop_ok = setresuid(-1, -1, -1) == 0 && getuid() == (uid_t)su_col[0] && geteuid() == (uid_t)su_col[1];

    // ---- PR_GET_SECUREBITS round-trips (default 0); PR_SET stores it ----
    errno = 0;
    int sb = prctl(PR_GET_SECUREBITS, 0, 0, 0, 0);
    int securebits_ok = sb == 0 && prctl(PR_SET_SECUREBITS, sb, 0, 0, 0) == 0 &&
                        prctl(PR_GET_SECUREBITS, 0, 0, 0, 0) == sb;

    // ---- PR_SET/GET_KEEPCAPS round-trip ----
    int keepcaps_ok = prctl(PR_GET_KEEPCAPS, 0, 0, 0, 0) == 0 && prctl(PR_SET_KEEPCAPS, 1, 0, 0, 0) == 0 &&
                      prctl(PR_GET_KEEPCAPS, 0, 0, 0, 0) == 1 && prctl(PR_SET_KEEPCAPS, 0, 0, 0, 0) == 0 &&
                      prctl(PR_GET_KEEPCAPS, 0, 0, 0, 0) == 0;

    // ---- setfsuid returns the PREVIOUS fs id and never fails ----
    int prev_fsuid = setfsuid(-1);      // query: returns current fsuid without changing it
    int fsuid_ok = prev_fsuid == (int)geteuid() && setfsuid(geteuid()) == (int)geteuid();

    // ---- PR_CAPBSET_DROP clears a bounding bit visibly in PR_CAPBSET_READ and status CapBnd ----
    int bnd_before = prctl(PR_CAPBSET_READ, CAP_NET_RAW, 0, 0, 0);
    int drop = prctl(PR_CAPBSET_DROP, CAP_NET_RAW, 0, 0, 0);
    int bnd_after = prctl(PR_CAPBSET_READ, CAP_NET_RAW, 0, 0, 0);
    unsigned long long s_bnd = status_hex("CapBnd:");
    int capbset_ok = bnd_before == 1 && drop == 0 && bnd_after == 0 &&
                     (s_bnd & (1ULL << CAP_NET_RAW)) == 0 && (s_bnd & (1ULL << CAP_SETPCAP)) != 0;

    // ---- capset() narrows the EFFECTIVE set; capget(2) and status CapEff both reflect it ----
    unsigned long long eff0 = capget_eff();
    unsigned long long want = DOCKER_CAP & ~(1ULL << CAP_KILL);
    int cs = capset_eff(want);
    unsigned long long eff1 = capget_eff();
    unsigned long long s_eff = status_hex("CapEff:");
    int capset_ok = eff0 == DOCKER_CAP && cs == 0 && eff1 == want && s_eff == want;

    // ---- PR_SET_NO_NEW_PRIVS reflected in GET + status, and irreversible ----
    int nnp0 = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);
    int nnp_set = prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    int nnp1 = prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0);
    char nv[16];
    int s_nnp = status_line("NoNewPrivs:", nv, sizeof nv) ? atoi(nv) : -1;
    errno = 0;
    int nnp_clear = prctl(PR_SET_NO_NEW_PRIVS, 0, 0, 0, 0); // no way to clear -> must fail
    int nnp_ok = nnp0 == 0 && nnp_set == 0 && nnp1 == 1 && s_nnp == 1 && nnp_clear == -1 &&
                 prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1;

    // ---- setter behaviour matches the Linux privilege model exactly (in a child, to isolate the change) ----
    // Privileged (root container): a real drop is reflected in getres*id + status and cannot be undone.
    // Unprivileged: setting an id NOT already held is EPERM, while setting a held id succeeds -- and either way
    // the status lines stay consistent with the syscalls. Both branches assert ok, so the case is robust to the
    // guest's configured uid.
    pid_t child = fork();
    if (child == 0) {
        int ok;
        if (geteuid() == 0) {
            // drop gid FIRST (while still privileged), then uid (which strips CAP_SETUID/SETGID).
            ok = setresgid(3000, 3000, 3000) == 0 && setresuid(2000, 2000, 2000) == 0;
            uid_t cru, ceu, csu;
            gid_t crg, ceg, csg;
            getresuid(&cru, &ceu, &csu);
            getresgid(&crg, &ceg, &csg);
            ok = ok && cru == 2000 && ceu == 2000 && csu == 2000 && crg == 3000 && ceg == 3000 && csg == 3000 &&
                 getuid() == 2000 && geteuid() == 2000 && getgid() == 3000 && getegid() == 3000;
            long cu[4], cg[4];
            ok = ok && status_ids("Uid:", cu) && cu[0] == 2000 && cu[1] == 2000 && cu[2] == 2000 && cu[3] == 2000;
            ok = ok && status_ids("Gid:", cg) && cg[0] == 3000 && cg[1] == 3000 && cg[2] == 3000 && cg[3] == 3000;
            errno = 0;
            ok = ok && setuid(0) == -1 && errno == EPERM; // cannot regain root after the drop
        } else {
            uid_t self = getuid();
            gid_t selfg = getgid();
            errno = 0;
            ok = setuid(0) == -1 && errno == EPERM;                  // cannot raise to an id it does not hold
            errno = 0;
            ok = ok && setresuid((uid_t)-1, 0, (uid_t)-1) == -1 && errno == EPERM; // seteuid(0) also EPERM
            ok = ok && setuid(self) == 0 && setgid(selfg) == 0;      // setting a held id succeeds (no-op)
            long cu[4];
            ok = ok && status_ids("Uid:", cu) && cu[0] == (long)self && cu[1] == (long)self; // unchanged + consistent
        }
        _exit(ok ? 0 : 1);
    }
    int st = 0;
    waitpid(child, &st, 0);
    int drop_ok = WIFEXITED(st) && WEXITSTATUS(st) == 0;

    int ok = uid_consistent && gid_consistent && noop_ok && securebits_ok && keepcaps_ok && fsuid_ok &&
             capbset_ok && capset_ok && nnp_ok && drop_ok;
    if (ok)
        printf("credentials ok=1\n");
    else
        printf("credentials ok=0 uid=%d gid=%d noop=%d sec=%d keep=%d fsuid=%d capbset=%d capset=%d nnp=%d drop=%d\n",
               uid_consistent, gid_consistent, noop_ok, securebits_ok, keepcaps_ok, fsuid_ok, capbset_ok, capset_ok,
               nnp_ok, drop_ok);
    return 0;
}
