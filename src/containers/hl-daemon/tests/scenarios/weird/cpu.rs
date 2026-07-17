//! CPU-feature probing: getauxval AT_HWCAP/AT_PAGESZ, SSE2/NEON SIMD dispatch (x86 `cpuid` /
//! arm HWCAP), the rdtsc / cntvct_el0 timestamp counter, the vDSO clock, the plain cc canary, and
//! shell-level cpu-topology (nproc, /proc/cpuinfo). The compiled cases run via `cc()` (xfail on Linux).

use super::cc;
use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // getauxval: exercise the auxv-reading path. AT_HWCAP bit semantics differ by arch (and QEMU
        // amd64 reports a sparse mask), so assert on AT_PAGESZ which is a stable 4096 on both arches.
        cc("weird/auxval-hwcap", "-O2", r#"#include <stdio.h>
#include <sys/auxv.h>
int main(){
  unsigned long hw=getauxval(AT_HWCAP);   /* exercise AT_HWCAP read */
  unsigned long ps=getauxval(AT_PAGESZ);
  (void)hw;
  printf("AUXV=%lu\n", ps); return 0;
}"#).has("AUXV=4096"),
        // SIMD dispatch: x86 executes the `cpuid` instruction (a classic translator trap) to detect
        // SSE2 (EDX bit26 — baseline-guaranteed on every x86-64, incl. QEMU); arm reads HWCAP_ASIMD.
        cc("weird/simd-probe", "-O2", r#"#include <stdio.h>
#if defined(__x86_64__)
#include <cpuid.h>
int main(){ unsigned a,b,c,d; __get_cpuid(1,&a,&b,&c,&d);
  printf("SIMD=%s\n",(d&(1u<<26))?"ok":"no"); return 0; }
#elif defined(__aarch64__)
#include <sys/auxv.h>
int main(){ unsigned long h=getauxval(AT_HWCAP);
  printf("SIMD=%s\n",(h&(1u<<1))?"ok":"no"); return 0; }
#endif"#).has("SIMD=ok"),
        // Timestamp counter: x86 `rdtsc` / arm `mrs cntvct_el0` inline asm read monotonic — the JIT
        // must emulate the privileged-ish counter read.
        cc("weird/tsc-counter", "-O2", r#"#include <stdio.h>
#include <stdint.h>
static inline uint64_t rd(){
#if defined(__x86_64__)
 unsigned lo,hi; __asm__ volatile("rdtsc":"=a"(lo),"=d"(hi)); return ((uint64_t)hi<<32)|lo;
#elif defined(__aarch64__)
 uint64_t v; __asm__ volatile("mrs %0, cntvct_el0":"=r"(v)); return v;
#endif
}
int main(){uint64_t a=rd(); for(volatile long i=0;i<100000;i++); uint64_t b=rd();
 printf("TSC=%s\n", b>=a?"ok":"no"); return 0;}"#).has("TSC=ok"),
        // vDSO: clock_gettime(CLOCK_MONOTONIC) is serviced via the vDSO mapping (no real syscall);
        // the translator must run vDSO code correctly and keep the clock monotonic.
        cc("weird/vdso-clock", "-O2", r#"#include <stdio.h>
#include <time.h>
int main(){struct timespec a,b; clock_gettime(CLOCK_MONOTONIC,&a);
  for(volatile long i=0;i<1000000;i++); clock_gettime(CLOCK_MONOTONIC,&b);
  printf("CLOCK=%s\n",(b.tv_sec>a.tv_sec||(b.tv_sec==a.tv_sec&&b.tv_nsec>=a.tv_nsec))?"ok":"no"); return 0;}"#)
            .has("CLOCK=ok"),
        // Plain compile-and-run canary: documents the toolchain (cc1/as/ld fork-exec) gap directly,
        // separate from any syscall — XPASS here means `cc` works under hl again.
        cc("weird/cc-canary", "-O2", r#"#include <stdio.h>
int main(){ long s=0; for(long i=1;i<=1000;i++) s+=i; printf("CC=%ld\n", s); return 0; }"#)
            .has("CC=500500"),
        // nproc: online-CPU count (shell, no compiler) — isolates cpu-topology from the toolchain.
        scen("weird/nproc", "alpine:latest")
            .exec("[ \"$(nproc)\" -ge 1 ] && echo NPROC-OK || echo NPROC-BAD")
            .has("NPROC-OK"),
        // /proc/cpuinfo presence + feature line (shell).
        scen("weird/proc-cpuinfo", "alpine:latest")
            .exec("grep -qE '^(processor|Features|flags)' /proc/cpuinfo && echo CPUINFO-OK || echo CPUINFO-NO")
            .has("CPUINFO-OK"),
    ]
}
