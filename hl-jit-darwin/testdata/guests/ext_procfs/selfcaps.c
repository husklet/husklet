// Capability + security-context conformance. A default `docker run` (root, unprivileged container) DROPS
// all but 14 Linux capabilities; software that gates privileged ops on its effective cap set (nginx master,
// postgres drop-to-unprivileged, capsh/getpcaps, systemd) reads these three sources — and they MUST agree:
//   /proc/self/status Cap* lines, capget(2), and prctl(PR_CAPBSET_READ). Golden (verified vs OrbStack real
//   docker, Server 29.4.0/runc): CapInh=CapAmb=0, CapPrm=CapEff=CapBnd=00000000a80425fb (the 14 default
//   caps), NoNewPrivs=0, Seccomp=2, Seccomp_filters=1, PR_GET_SECCOMP=2. hl previously reported all-ones
//   (over-reporting caps) and omitted the Cap*/Seccomp lines from status entirely.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include "pf.h"

#define DOCKER_CAP 0x00000000a80425fbULL // the 14-cap default bounding/permitted/effective set

// capget(2) directly — libcap/capsh use this, and it MUST agree with /proc/self/status.
struct chdr { unsigned version; int pid; };
struct cdata { unsigned eff, prm, inh; };
static unsigned long long capget_field(int which /*0=eff 1=prm 2=inh*/) {
    struct chdr h = {0x20080522u, 0}; // _LINUX_CAPABILITY_VERSION_3
    struct cdata d[2];
    memset(d, 0, sizeof d);
    if (syscall(SYS_capget, &h, d) != 0) return ~0ULL; // sentinel: forces a mismatch
    unsigned lo = which == 0 ? d[0].eff : which == 1 ? d[0].prm : d[0].inh;
    unsigned hi = which == 0 ? d[1].eff : which == 1 ? d[1].prm : d[1].inh;
    return ((unsigned long long)hi << 32) | lo;
}

int main(void) {
    char b[8192];
    int n = pf_read("/proc/self/status", b, sizeof b);
    char v[128];

    // ---- /proc/self/status cap + security fields (byte-exact strings vs docker) ----
    unsigned long long s_inh = 1, s_prm = 1, s_eff = 1, s_bnd = 1, s_amb = 1;
    int s_nnp = -1, s_sec = -1, s_secf = -1;
    if (pf_line_val(b, "CapInh:", v, sizeof v)) s_inh = strtoull(v, 0, 16);
    if (pf_line_val(b, "CapPrm:", v, sizeof v)) s_prm = strtoull(v, 0, 16);
    if (pf_line_val(b, "CapEff:", v, sizeof v)) s_eff = strtoull(v, 0, 16);
    if (pf_line_val(b, "CapBnd:", v, sizeof v)) s_bnd = strtoull(v, 0, 16);
    if (pf_line_val(b, "CapAmb:", v, sizeof v)) s_amb = strtoull(v, 0, 16);
    if (pf_line_val(b, "NoNewPrivs:", v, sizeof v)) s_nnp = atoi(v);
    if (pf_line_val(b, "Seccomp:", v, sizeof v)) s_sec = atoi(v);
    if (pf_line_val(b, "Seccomp_filters:", v, sizeof v)) s_secf = atoi(v);

    // ---- capget(2) ----
    unsigned long long g_eff = capget_field(0), g_prm = capget_field(1), g_inh = capget_field(2);

    // ---- prctl(PR_CAPBSET_READ) over every cap: reconstruct the bounding mask ----
    unsigned long long bset = 0;
    for (int cap = 0; cap <= 40; cap++)
        if (prctl(PR_CAPBSET_READ, cap, 0, 0, 0) == 1) bset |= (1ULL << cap);

    int p_seccomp = prctl(PR_GET_SECCOMP, 0, 0, 0, 0);

    int status_ok = n > 0 && s_inh == 0 && s_prm == DOCKER_CAP && s_eff == DOCKER_CAP &&
                    s_bnd == DOCKER_CAP && s_amb == 0 && s_nnp == 0 && s_sec == 2 && s_secf == 1;
    int capget_ok = g_eff == DOCKER_CAP && g_prm == DOCKER_CAP && g_inh == 0;
    int capbset_ok = bset == DOCKER_CAP && p_seccomp == 2;
    // The three sources MUST be internally consistent (this is the invariant real software relies on).
    int consistent = s_eff == g_eff && s_bnd == bset;

    int ok = status_ok && capget_ok && capbset_ok && consistent;
    if (ok) {
        printf("selfcaps ok=1\n");
    } else {
        printf("selfcaps ok=0 status=%d capget=%d capbset=%d consistent=%d\n", status_ok, capget_ok,
               capbset_ok, consistent);
        printf("  status inh=%016llx prm=%016llx eff=%016llx bnd=%016llx amb=%016llx nnp=%d sec=%d secf=%d\n",
               s_inh, s_prm, s_eff, s_bnd, s_amb, s_nnp, s_sec, s_secf);
        printf("  capget eff=%016llx prm=%016llx inh=%016llx | capbset=%016llx seccomp=%d\n", g_eff, g_prm,
               g_inh, bset, p_seccomp);
    }
    return 0;
}
