// cgroup v2 well-formedness + read-only enforcement. The engine synthesizes a container's unified-hierarchy
// view (memory.max/cpu.max/... under a ro cgroup2 mount) that a bare host may not expose, so every content
// assertion is "absent OR well-formed": true where the file does not exist (bare native) AND true when the
// engine serves a properly shaped value -- a boolean-derived golden that is stable across environments yet
// still catches a malformed/inconsistent synthesized file (mem.current>mem.max, cpu.max unparsable, missing
// controller). Runtimes (JVM UseContainerSupport, Go automaxprocs, runc, node-exporter) parse exactly these.
// Separately: the cgroup2 mount is read-only, so a write-intent open of a cgroup file must FAIL (EROFS on the
// engine, ENOENT on a host without the file) -- never a silent fake-success that lets a runtime believe it
// changed a limit it did not.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "pf.h"

#define CG "/sys/fs/cgroup/"

// read a cgroup file; returns 1 if absent (open failed), else 0 and fills buf.
static int absent(const char *leaf, char *buf, int cap) {
    char p[128];
    snprintf(p, sizeof p, "%s%s", CG, leaf);
    return pf_read(p, buf, cap) < 0;
}
static int has(const char *hay, const char *k) { return strstr(hay, k) != 0; }

int main(void) {
    char b[8192], b2[8192];

    // /proc/self/cgroup is the v2 single line "0::/<path>" (present on both bare host and engine).
    pf_read("/proc/self/cgroup", b, sizeof b);
    int self_fmt = strncmp(b, "0::/", 4) == 0 && strchr(b, '\n') == strrchr(b, '\n');
    printf("self_cgroup_fmt=%d\n", self_fmt);

    // memory.max: "max" or a positive byte count.
    { int a = absent("memory.max", b, sizeof b);
      unsigned long long mx = 0; int wf = a || (strncmp(b, "max", 3) == 0) || ((mx = strtoull(b, 0, 10)) > 0);
      printf("memory_max_ok=%d\n", wf);
      // memory.current <= memory.max and non-negative (skip the relation when either is absent/unlimited).
      int a2 = absent("memory.current", b2, sizeof b2);
      unsigned long long cur = a2 ? 0 : strtoull(b2, 0, 10);
      int rel = a || a2 || (strncmp(b, "max", 3) == 0) || (cur <= mx);
      printf("memory_current_le_max=%d\n", rel); }

    // memory.stat: canonical key set a runtime parses.
    { int a = absent("memory.stat", b, sizeof b);
      printf("memory_stat_ok=%d\n", a || (has(b, "anon ") && has(b, "file ") && has(b, "pgfault "))); }
    // memory.events: oom/oom_kill/max counters present.
    { int a = absent("memory.events", b, sizeof b);
      printf("memory_events_ok=%d\n", a || (has(b, "oom ") && has(b, "oom_kill ") && has(b, "max "))); }

    // cpu.max: "<quota> <period>" or "max <period>", period>0.
    { int a = absent("cpu.max", b, sizeof b);
      int wf = a; char q[32]; int per = 0;
      if (!a && sscanf(b, "%31s %d", q, &per) == 2 && per > 0 && (strcmp(q, "max") == 0 || strtoll(q, 0, 10) > 0))
          wf = 1;
      printf("cpu_max_ok=%d\n", wf); }
    // cpu.stat: usage/throttle counters keyed by name.
    { int a = absent("cpu.stat", b, sizeof b);
      printf("cpu_stat_ok=%d\n", a || (has(b, "usage_usec") && has(b, "nr_throttled") && has(b, "nr_periods"))); }
    // cpu.weight in [1,10000].
    { int a = absent("cpu.weight", b, sizeof b);
      long w = a ? 0 : strtol(b, 0, 10);
      printf("cpu_weight_ok=%d\n", a || (w >= 1 && w <= 10000)); }

    // pids.max parseable, pids.current > 0 and <= max.
    { int a = absent("pids.max", b, sizeof b);
      int unl = !a && strncmp(b, "max", 3) == 0;
      long long mx = a ? 0 : (unl ? (1LL << 62) : strtoll(b, 0, 10));
      int a2 = absent("pids.current", b2, sizeof b2);
      long long cur = a2 ? 0 : strtoll(b2, 0, 10);
      printf("pids_max_ok=%d\n", a || unl || mx > 0);
      printf("pids_current_ok=%d\n", a || a2 || (cur > 0 && cur <= mx)); }

    // cgroup.controllers advertises at least cpu/memory/pids.
    { int a = absent("cgroup.controllers", b, sizeof b);
      printf("controllers_ok=%d\n", a || (has(b, "cpu") && has(b, "memory") && has(b, "pids"))); }
    // cgroup.type == domain.
    { int a = absent("cgroup.type", b, sizeof b);
      printf("cgroup_type_ok=%d\n", a || strncmp(b, "domain", 6) == 0); }
    // cgroup.procs lists our own pid (we are a member of the one container cgroup).
    { int a = absent("cgroup.procs", b, sizeof b);
      int found = 0; char pidstr[16]; snprintf(pidstr, sizeof pidstr, "%d", getpid());
      char *sv; for (char *t = strtok_r(b, " \n", &sv); t; t = strtok_r(0, " \n", &sv))
          if (!strcmp(t, pidstr)) { found = 1; break; }
      printf("procs_has_self=%d\n", a || found); }
    // cgroup.events populated marker.
    { int a = absent("cgroup.events", b, sizeof b);
      printf("cgroup_events_ok=%d\n", a || has(b, "populated")); }

    // A nonexistent controller file -> ENOENT (open fails), not a phantom fd.
    { int fd = open(CG "nonexistent.controller", O_RDONLY);
      printf("bogus_enoent=%d\n", fd < 0); if (fd >= 0) close(fd); }

    // Read-only mount: a write-intent open must NOT succeed (EROFS on the engine, ENOENT on a bare host).
    { int fd = open(CG "cpu.max", O_WRONLY);
      printf("cpu_max_wopen_fails=%d\n", fd < 0); if (fd >= 0) close(fd); }
    { int fd = open(CG "memory.max", O_WRONLY);
      printf("memory_max_wopen_fails=%d\n", fd < 0); if (fd >= 0) close(fd); }
    return 0;
}
