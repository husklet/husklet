// Namespace magic-link self-consistency -- what lsns/nsenter/runc inspect. For every /proc/self/ns/<name>
// the readlink must be a well-formed "<name>:[<inode>]" string, be stable across two reads, and -- the
// property container tooling relies on -- the inode named in the link text must equal st_ino of a stat()
// of the same file (they compare namespaces by that inode). A live peer process in the same container
// shares the single namespace set, so its /proc/<pid>/ns/net link must equal self's. Before the fix the
// readlink returned hardcoded initial-namespace inodes while stat() followed the magic link to the real
// host nsfs node, so link-inode != stat-inode for every namespace the engine did not share with init.
// Also confirms the ns-related privileged syscalls fail/return CLEANLY (no crash or hang, engine stays
// alive) on the facts that are identical native-vs-engine. All prints are derived booleans -> deterministic
// regardless of the actual (host-varying) inode values. Verdict: every field 1.
#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *const NS[] = {"mnt", "pid", "net", "ipc", "uts", "user", "cgroup", "time", 0};

// Parse the inode from a "<name>:[<digits>]" link into *ino; return 1 if well-formed for namespace `name`.
static int parse_ns(const char *name, const char *link, unsigned long *ino) {
    size_t nl = strlen(name);
    if (strncmp(link, name, nl) || link[nl] != ':' || link[nl + 1] != '[') return 0;
    const char *d = link + nl + 2;
    char *end = NULL;
    unsigned long v = strtoul(d, &end, 10);
    if (end == d || end[0] != ']' || end[1] != 0) return 0;
    *ino = v;
    return 1;
}

int main(void) {
    int wf = 1, stable = 1, inomatch = 1;
    unsigned long net_self = 0;
    for (int i = 0; NS[i]; i++) {
        char p[64], a[128], b[128];
        snprintf(p, sizeof p, "/proc/self/ns/%s", NS[i]);
        ssize_t ra = readlink(p, a, sizeof a - 1);
        ssize_t rb = readlink(p, b, sizeof b - 1);
        if (ra <= 0 || rb <= 0) {
            wf = stable = inomatch = 0;
            continue;
        }
        a[ra] = 0;
        b[rb] = 0;
        stable &= (strcmp(a, b) == 0);
        unsigned long lino = 0;
        int ok = parse_ns(NS[i], a, &lino);
        wf &= ok;
        struct stat st;
        inomatch &= (ok && stat(p, &st) == 0 && (unsigned long)st.st_ino == lino);
        if (!strcmp(NS[i], "net")) net_self = lino;
    }

    // Peer: a live child shares this container's single namespace set -> its net link equals ours.
    int peer_ok = 0;
    pid_t ch = fork();
    if (ch == 0) {
        pause();
        _exit(0);
    }
    usleep(150000); // let the child register in the proc registry
    char pp[64], pl[128];
    snprintf(pp, sizeof pp, "/proc/%d/ns/net", ch);
    ssize_t pr = readlink(pp, pl, sizeof pl - 1);
    if (pr > 0) {
        pl[pr] = 0;
        unsigned long pino = 0;
        peer_ok = parse_ns("net", pl, &pino) && pino == net_self;
    }
    kill(ch, SIGKILL);
    waitpid(ch, NULL, 0);

    // Clean-failure facts that are identical on native and the engine (no crash/hang either way).
    errno = 0;
    int ebadf = (setns(-1, 0) == -1 && errno == EBADF);
    int unshare0 = (unshare(0) == 0);
    errno = 0;
    int cenoent = (chroot("/no-such-dir-xyzzy") == -1 && errno == ENOENT);

    printf("wf=%d stable=%d inomatch=%d peer=%d ebadf=%d unshare0=%d chroot_enoent=%d\n", wf, stable, inomatch,
           peer_ok, ebadf, unshare0, cenoent);
    return 0;
}
